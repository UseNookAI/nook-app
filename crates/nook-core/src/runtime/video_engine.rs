//! Ports `runtime/VideoEngine.java`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

use super::engine_process::{append_log, java_double, kill_tree, log_tail};
use super::image_engine::{blank_to_none, parse_double, random_seed, text_or};
use super::model_registry::now_iso;

/// How long one clip may take before the engine gives up on it. CPU renders are this slow.
pub const TIMEOUT: Duration = Duration::from_secs(2 * 3600);

static LEVEL_TAG: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\s*\[[A-Z?]+\s*\]\s*$").expect("level tag pattern"));
static BAR: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\|([=>#\s-]*)\|\s*(\d+)/(\d+)").expect("bar pattern"));
static ANSI: Lazy<Regex> =
    Lazy::new(|| Regex::new("\u{1b}\\[[0-9;]*[A-Za-z]").expect("ansi pattern"));

/// One clip to render (`VideoEngine.Request`).
///
/// - `frames`: frame count; the Wan models take 4n+1
/// - `flow_shift`: the flow models' timestep shift, or 0 for the engine's own default
/// - `t5xxl`: file name beside the model passed as `--t5xxl`, or None
/// - `vae`: file name beside the model passed as `--vae`, or None
/// - `flags`: extra command-line flags, space separated, e.g. `--offload-to-cpu --vae-tiling`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoRequest {
    pub prompt: String,
    pub negative_prompt: String,
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    pub fps: u32,
    pub steps: u32,
    pub cfg_scale: f64,
    pub flow_shift: f64,
    pub seed: u64,
    pub sampler: String,
    pub t5xxl: Option<String>,
    pub vae: Option<String>,
    pub flags: String,
}

impl VideoRequest {
    pub fn seconds(&self) -> f64 {
        if self.fps > 0 {
            self.frames as f64 / self.fps as f64
        } else {
            0.0
        }
    }
}

/// A rendered clip (`VideoEngine.Result`); `elapsed_ms` is the Java `Duration elapsed`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoResult {
    pub file: PathBuf,
    pub request: VideoRequest,
    pub elapsed_ms: u64,
}

impl VideoResult {
    pub fn elapsed(&self) -> Duration {
        Duration::from_millis(self.elapsed_ms)
    }
}

/// What the engine is doing. Loading covers reading the weights and encoding the prompt.
/// Serialized as the Java constant name (`"SAMPLING"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Stage {
    Loading,
    Sampling,
    Decoding,
    Saving,
}

impl std::fmt::Display for Stage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Stage::Loading => "LOADING",
            Stage::Sampling => "SAMPLING",
            Stage::Decoding => "DECODING",
            Stage::Saving => "SAVING",
        })
    }
}

/// Progress of a clip: `(stage, done, total)`, where `done` counts units finished in this stage
/// (sampling steps, weights read), 0 when unknown.
pub type VideoProgress = Arc<dyn Fn(Stage, u32, u32) + Send + Sync>;

/// The error a stopped clip ends with (the original's `CancellationException`); callers tell it
/// from a failure with `err.is::<Stopped>()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stopped;

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Video generation was stopped.")
    }
}

impl std::error::Error for Stopped {}

/// Text-to-video through the stable-diffusion.cpp command line (`-M vid_gen`). Like
/// [`ImageEngine`](super::image_engine::ImageEngine), each clip is one `sd` process that loads
/// the model, renders every frame, writes an MJPEG AVI and exits, so VRAM is held only while a
/// clip is being made. A clip takes minutes, so the engine reads the progress bars the process
/// prints as it goes and can be stopped part way.
pub struct VideoEngine {
    exe: PathBuf,
    log_dir: PathBuf,
}

impl VideoEngine {
    pub fn new(exe: &Path, log_dir: &Path) -> VideoEngine {
        VideoEngine {
            exe: exe.to_path_buf(),
            log_dir: log_dir.to_path_buf(),
        }
    }

    pub fn is_available(&self) -> bool {
        self.exe.exists()
    }

    /// Builds a request from catalog defaults and a fresh seed.
    pub fn request_for(defaults: &BTreeMap<String, String>, prompt: &str) -> VideoRequest {
        VideoRequest {
            prompt: prompt.to_string(),
            negative_prompt: text_or(defaults, "negative", ""),
            width: round_to_16(parse_i64(defaults.get("width"), 832)),
            height: round_to_16(parse_i64(defaults.get("height"), 480)),
            frames: four_n_plus_one(parse_i64(defaults.get("frames"), 33)),
            fps: parse_i64(defaults.get("fps"), 16).clamp(1, u32::MAX as i64) as u32,
            steps: parse_i64(defaults.get("steps"), 20).clamp(0, u32::MAX as i64) as u32,
            cfg_scale: parse_double(defaults.get("cfg"), 6.0),
            flow_shift: parse_double(defaults.get("flowShift"), 0.0),
            seed: random_seed(),
            sampler: text_or(defaults, "sampler", "euler"),
            t5xxl: blank_to_none(defaults.get("t5xxl")),
            vae: blank_to_none(defaults.get("vae")),
            flags: text_or(defaults, "flags", ""),
        }
    }

    /// The command line for one clip (the executable first).
    pub fn command(&self, model_file: &Path, req: &VideoRequest, out: &Path) -> Vec<String> {
        let mut cmd: Vec<String> = vec![
            self.exe.display().to_string(),
            "-M".into(),
            "vid_gen".into(),
            "--diffusion-model".into(),
            model_file.display().to_string(),
            "-p".into(),
            arg_safe(&req.prompt),
            "-W".into(),
            req.width.to_string(),
            "-H".into(),
            req.height.to_string(),
            "--video-frames".into(),
            req.frames.to_string(),
            "--fps".into(),
            req.fps.to_string(),
            "--steps".into(),
            req.steps.to_string(),
            "--cfg-scale".into(),
            java_double(req.cfg_scale),
            "-s".into(),
            req.seed.to_string(),
            "--sampling-method".into(),
            req.sampler.clone(),
            "-o".into(),
            out.display().to_string(),
        ];
        if req.flow_shift > 0.0 {
            cmd.push("--flow-shift".into());
            cmd.push(java_double(req.flow_shift));
        }
        if let Some(t5) = &req.t5xxl {
            cmd.push("--t5xxl".into());
            cmd.push(model_file.with_file_name(t5).display().to_string());
        }
        if let Some(vae) = &req.vae {
            cmd.push("--vae".into());
            cmd.push(model_file.with_file_name(vae).display().to_string());
        }
        cmd.extend(req.flags.split_whitespace().map(String::from));
        if !req.negative_prompt.trim().is_empty() {
            cmd.push("-n".into());
            cmd.push(arg_safe(&req.negative_prompt));
        }
        cmd
    }

    /// Renders one clip into `out` (an `.avi`). Fails with [`Stopped`] when `cancel` fires; the
    /// partial file is removed.
    pub async fn generate(
        &self,
        model_file: &Path,
        req: &VideoRequest,
        out: &Path,
        progress: Option<VideoProgress>,
        cancel: &CancellationToken,
    ) -> Result<VideoResult> {
        if !self.exe.exists() {
            bail!("Video engine binary missing: {}", self.exe.display());
        }
        if let Some(parent) = out.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Could not create {}", parent.display()))?;
        }
        tokio::fs::create_dir_all(&self.log_dir)
            .await
            .with_context(|| format!("Could not create {}", self.log_dir.display()))?;
        let log_file = self.log_dir.join("sd-video.log");
        let cmd = self.command(model_file, req, out);
        append_log(
            &log_file,
            &format!("\n=== {} {}\n", now_iso(), cmd.join(" ")),
        );
        let started = Instant::now();
        // The progress comes on standard output; errors go straight to the log.
        let errors = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
            .with_context(|| format!("Could not open {}", log_file.display()))?;
        let mut c = crate::process::command(&cmd[0]);
        c.args(&cmd[1..])
            .stdout(Stdio::piped())
            .stderr(Stdio::from(errors));
        if let Some(dir) = self.exe.parent() {
            c.current_dir(dir);
        }
        let mut child = crate::process::spawn_managed(&mut c).map_err(|e| {
            anyhow!(
                "Could not start the video engine {}: {e}",
                self.exe.display()
            )
        })?;
        let mut parser = ProgressParser::new(progress);
        let stdout = child.stdout.take();
        let log_for_reader = log_file.clone();
        let reader = tokio::spawn(async move {
            if let Some(stdout) = stdout {
                pump(stdout, &log_for_reader, &mut parser).await;
            }
        });
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        let status = tokio::select! {
            status = child.wait() => status?,
            _ = cancel.cancelled() => {
                kill_tree(&mut child).await;
                let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
                let _ = tokio::fs::remove_file(out).await;
                return Err(anyhow::Error::new(Stopped));
            }
            _ = tokio::time::sleep_until(deadline) => {
                kill_tree(&mut child).await;
                let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
                let _ = tokio::fs::remove_file(out).await;
                bail!("Video generation timed out after {} minutes.", TIMEOUT.as_secs() / 60);
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(2), reader).await;
        let code = status.code().unwrap_or(-1);
        if code != 0 || !out.exists() {
            bail!(
                "Video engine failed (exit {code}). {}",
                log_tail(&log_file, 15)
            );
        }
        let elapsed = started.elapsed();
        tracing::info!(
            "Video {} rendered in {} s ({}x{}, {} frames, {} steps)",
            out.file_name().unwrap_or_default().to_string_lossy(),
            elapsed.as_secs(),
            req.width,
            req.height,
            req.frames,
            req.steps
        );
        Ok(VideoResult {
            file: out.to_path_buf(),
            request: req.clone(),
            elapsed_ms: elapsed.as_millis() as u64,
        })
    }
}

/// A prompt as one Windows command-line argument. Quotes and backslashes are how Windows splits a
/// command line into arguments, and a quote or a trailing backslash in a prompt can end the
/// argument early, so they become ' and /. Line breaks and runs of spaces become one space; the
/// text model reads the same words either way.
pub fn arg_safe(s: &str) -> String {
    static SPACES: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s+").expect("space pattern"));
    let replaced = s.replace('"', "'").replace('\\', "/");
    SPACES.replace_all(&replaced, " ").trim().to_string()
}

pub fn round_to_16(v: i64) -> u32 {
    ((v + 8) / 16 * 16).clamp(16, u32::MAX as i64) as u32
}

/// The nearest frame count of the form 4n+1, at least 5 so the engine writes a video, not a still.
pub fn four_n_plus_one(v: i64) -> u32 {
    // Java's Math.round of a float: half up.
    let n = (((v - 1) as f32 / 4.0) + 0.5).floor() as i64;
    (4 * n.max(1) + 1).clamp(5, u32::MAX as i64) as u32
}

fn parse_i64(s: Option<&String>, def: i64) -> i64 {
    s.and_then(|s| s.trim().parse().ok()).unwrap_or(def)
}

/// Copies the process output to the log and hands each line or progress update to the parser. A
/// progress bar redraws itself after a \r, so only its last state goes to the log: the one
/// followed by a line end, which the Windows C runtime writes as \r\n.
pub async fn pump<R: AsyncRead + Unpin>(
    mut input: R,
    log_file: &Path,
    parser: &mut ProgressParser,
) {
    let mut log = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
    {
        Ok(f) => Some(f),
        Err(e) => {
            tracing::debug!("Could not open {}: {e}", log_file.display());
            None
        }
    };
    let mut write = |s: &str| {
        if let Some(f) = log.as_mut() {
            let _ = f.write_all(format!("{}\n", ProgressParser::strip_ansi(s)).as_bytes());
        }
    };
    let mut segment: Vec<u8> = Vec::new();
    let mut last_bar: Option<String> = None;
    let mut buf = [0u8; 4096];
    loop {
        let n = match input.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                tracing::debug!("Video engine output closed: {e}");
                break;
            }
        };
        for &b in &buf[..n] {
            if b == b'\r' || b == b'\n' {
                if !segment.is_empty() {
                    let s = String::from_utf8_lossy(&segment).into_owned();
                    segment.clear();
                    parser.feed(&s);
                    if b == b'\r' && ProgressParser::is_bar(&s) {
                        last_bar = Some(s);
                        continue;
                    }
                    // The logger's level tag lands after its line, on its own; it tells the log nothing.
                    if LEVEL_TAG.is_match(&s) {
                        continue;
                    }
                    write(&s);
                    last_bar = None;
                } else if b == b'\n' {
                    if let Some(bar) = last_bar.take() {
                        write(&bar);
                    }
                }
            } else {
                segment.push(b);
            }
        }
    }
    if !segment.is_empty() {
        let s = String::from_utf8_lossy(&segment).into_owned();
        parser.feed(&s);
        if !LEVEL_TAG.is_match(&s) {
            write(&s);
        }
    }
}

/// Turns the engine's output into stages and steps. Log lines mark the stages; progress bars
/// (`|=====>   | 5/20 - 3.10s/it`, or `#` while weights load) give the steps.
pub struct ProgressParser {
    progress: Option<VideoProgress>,
    stage: Stage,
}

impl ProgressParser {
    /// A parser at the loading stage; reports `(LOADING, 0, 0)` at once.
    pub fn new(progress: Option<VideoProgress>) -> ProgressParser {
        let p = ProgressParser {
            progress,
            stage: Stage::Loading,
        };
        p.report(0, 0);
        p
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    pub fn is_bar(s: &str) -> bool {
        BAR.is_match(s)
    }

    pub fn strip_ansi(s: &str) -> String {
        ANSI.replace_all(s, "").into_owned()
    }

    pub fn feed(&mut self, raw: &str) {
        let s = ProgressParser::strip_ansi(raw);
        if let Some(m) = BAR.captures(&s) {
            let loading_bar = m[1].contains('#');
            if loading_bar && self.stage != Stage::Loading {
                return; // a late weight load mid-render: keep the stage
            }
            // A step bar during loading means sampling began without the line that says so.
            if !loading_bar && self.stage == Stage::Loading {
                self.advance(Stage::Sampling);
            }
            let done = m[2].parse().unwrap_or(0);
            let total = m[3].parse().unwrap_or(0);
            self.report(done, total);
            return;
        }
        // "sampling using <method>" is printed while the model loads, before the prompt is
        // encoded, so it does not mark the start of sampling; the end of the encoding does.
        if s.contains("get_learned_condition completed") {
            self.advance(Stage::Sampling);
        } else if s.contains("sampling completed")
            || s.contains("generating latent video completed")
        {
            self.advance(Stage::Decoding);
        } else if s.contains("decode_first_stage completed")
            || s.contains("generate_video completed")
        {
            self.advance(Stage::Saving);
        }
    }

    fn advance(&mut self, next: Stage) {
        if next > self.stage {
            self.stage = next;
            self.report(0, 0);
        }
    }

    fn report(&self, done: u32, total: u32) {
        if let Some(p) = &self.progress {
            p(self.stage, done, total);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::runtime::model_catalog::ModelCatalog;
    use parking_lot::Mutex;

    fn recorder() -> (Arc<Mutex<Vec<String>>>, VideoProgress) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let progress: VideoProgress =
            Arc::new(move |stage, done, total| s.lock().push(format!("{stage} {done}/{total}")));
        (seen, progress)
    }

    fn after(cmd: &[String], flag: &str) -> String {
        let i = cmd
            .iter()
            .position(|a| a == flag)
            .unwrap_or_else(|| panic!("{flag} is on the command line"));
        cmd[i + 1].clone()
    }

    #[test]
    fn request_comes_from_the_catalog_defaults() {
        let catalog = ModelCatalog::bundled().unwrap();
        let wan = catalog.find("wan2.1-t2v-1.3b").unwrap();
        let r = VideoEngine::request_for(&wan.defaults, "a fox in the snow");
        assert_eq!(r.prompt, "a fox in the snow");
        assert_eq!(r.width, 832);
        assert_eq!(r.height, 480);
        assert_eq!((r.frames - 1) % 4 + 1, 1, "Wan takes 4n+1 frames");
        assert_eq!(r.fps, 16);
        assert_eq!(r.t5xxl.as_deref(), Some("umt5-xxl-encoder-Q5_K_M.gguf"));
        assert_eq!(r.vae.as_deref(), Some("wan_2.1_vae.safetensors"));
        assert!(!r.negative_prompt.trim().is_empty());
        assert!(r.seed < 2_000_000_000);
        assert!((r.seconds() - 33.0 / 16.0).abs() < 1e-9);
    }

    #[test]
    fn sizes_and_frame_counts_are_what_the_model_takes() {
        assert_eq!(round_to_16(830), 832);
        assert_eq!(round_to_16(480), 480);
        assert_eq!(round_to_16(1), 16);
        assert_eq!(four_n_plus_one(33), 33);
        assert_eq!(four_n_plus_one(48), 49);
        assert_eq!(
            four_n_plus_one(1),
            5,
            "at least 5 frames, so the engine writes a video"
        );
    }

    #[test]
    fn command_names_every_file_beside_the_model() {
        let model = Path::new("C:/nook/models/wan/Wan2.1-T2V-1.3B-Q8_0.gguf");
        let engine = VideoEngine::new(
            Path::new("C:/nook/runtime/bin/cuda/sd/sd-cli.exe"),
            Path::new("C:/nook/logs"),
        );
        let r = VideoRequest {
            prompt: "a cat".into(),
            negative_prompt: "blurry".into(),
            width: 832,
            height: 480,
            frames: 33,
            fps: 16,
            steps: 20,
            cfg_scale: 6.0,
            flow_shift: 3.0,
            seed: 42,
            sampler: "euler".into(),
            t5xxl: Some("umt5.gguf".into()),
            vae: Some("vae.safetensors".into()),
            flags: "--offload-to-cpu  --vae-tiling".into(),
        };
        let cmd = engine.command(model, &r, Path::new("C:/nook/videos/v.avi"));
        assert_eq!(after(&cmd, "-M"), "vid_gen");
        assert_eq!(
            after(&cmd, "--diffusion-model"),
            model.display().to_string()
        );
        assert_eq!(
            after(&cmd, "--t5xxl"),
            model.with_file_name("umt5.gguf").display().to_string()
        );
        assert_eq!(
            after(&cmd, "--vae"),
            model
                .with_file_name("vae.safetensors")
                .display()
                .to_string()
        );
        assert_eq!(after(&cmd, "--video-frames"), "33");
        assert_eq!(after(&cmd, "--fps"), "16");
        assert_eq!(after(&cmd, "--flow-shift"), "3.0");
        assert_eq!(after(&cmd, "--cfg-scale"), "6.0");
        assert_eq!(after(&cmd, "-s"), "42");
        assert_eq!(after(&cmd, "-n"), "blurry");
        assert!(cmd.contains(&"--offload-to-cpu".into()) && cmd.contains(&"--vae-tiling".into()));
        assert!(
            !cmd.contains(&String::new()),
            "doubled spaces in the flags make no empty arguments"
        );
    }

    #[test]
    fn a_prompt_stays_one_argument() {
        assert_eq!(
            arg_safe(" a \"red\" fox \\\n at   dawn "),
            "a 'red' fox / at dawn"
        );
        assert_eq!(arg_safe("ends in a slash\\"), "ends in a slash/");
        assert_eq!(
            arg_safe("雪の中の狐 é"),
            "雪の中の狐 é",
            "other scripts pass through; the engine reads its arguments as UTF-16"
        );
    }

    #[test]
    fn no_flow_shift_leaves_the_engine_default() {
        let engine = VideoEngine::new(Path::new("sd-cli.exe"), Path::new("logs"));
        let r = VideoRequest {
            prompt: "a cat".into(),
            negative_prompt: String::new(),
            width: 832,
            height: 480,
            frames: 33,
            fps: 16,
            steps: 20,
            cfg_scale: 6.0,
            flow_shift: 0.0,
            seed: 1,
            sampler: "euler".into(),
            t5xxl: None,
            vae: None,
            flags: String::new(),
        };
        let cmd = engine.command(Path::new("m.gguf"), &r, Path::new("v.avi"));
        assert!(!cmd.contains(&"--flow-shift".into()));
        assert!(!cmd.contains(&"--t5xxl".into()));
        assert!(!cmd.contains(&"-n".into()));
    }

    /// The order sd-cli master-841 prints a Wan clip in (sd-video.log of the 2026-09-24 render).
    #[test]
    fn progress_follows_the_engines_output() {
        let (seen, progress) = recorder();
        let mut p = ProgressParser::new(Some(progress));
        p.feed("[INFO ] stable-diffusion.cpp:721  - loading diffusion model from 'C:\\m.gguf'");
        p.feed("[INFO ] stable-diffusion.cpp:4397 - sampling using Euler method");
        p.feed(
            "  |######                                            | 120/1000 - 812.32MB/s\u{1b}[K",
        );
        p.feed("[INFO ] stable-diffusion.cpp:6575 - get_learned_condition completed, taking 4.10s");
        p.feed("[INFO ] stable-diffusion.cpp:6940 - generate_video 832x480x33");
        p.feed("  |==>                                               | 1/20 - 5.21s/it\u{1b}[K");
        p.feed("  |==================================================| 20/20 - 5.02s/it\u{1b}[K");
        p.feed("[INFO ] stable-diffusion.cpp:7037 - sampling completed, taking 101.3s");
        p.feed("[INFO ] stable-diffusion.cpp:6606 - decode_first_stage completed, taking 22.4s");
        p.feed("[INFO ] stable-diffusion.cpp:7241 - generate_video completed in 130.2s");
        assert_eq!(
            *seen.lock(),
            [
                "LOADING 0/0",
                "LOADING 120/1000",
                "SAMPLING 0/0",
                "SAMPLING 1/20",
                "SAMPLING 20/20",
                "DECODING 0/0",
                "SAVING 0/0"
            ]
        );
    }

    #[test]
    fn a_late_weight_load_does_not_send_the_stage_back() {
        let (seen, progress) = recorder();
        let mut p = ProgressParser::new(Some(progress));
        p.feed("get_learned_condition completed, taking 20.00s");
        p.feed("|#####     | 3/10 - 900.00MB/s");
        assert_eq!(p.stage(), Stage::Sampling);
        assert_eq!(*seen.lock(), ["LOADING 0/0", "SAMPLING 0/0"]);
    }

    #[test]
    fn a_step_bar_alone_starts_sampling() {
        let (seen, progress) = recorder();
        let mut p = ProgressParser::new(Some(progress));
        p.feed("|==>       | 2/20 - 5.00s/it");
        assert_eq!(
            *seen.lock(),
            ["LOADING 0/0", "SAMPLING 0/0", "SAMPLING 2/20"]
        );
    }

    /// The bytes sd-cli.exe writes into a pipe: the logger's "[INFO ] " tag arrives after its
    /// line (it goes through the buffered C runtime, the text through WriteFile), bars redraw
    /// after \r, and the last bar ends in \r\n because the C runtime writes \n as \r\n.
    #[tokio::test]
    async fn pump_reads_the_engines_pipe_and_logs_each_bar_once() {
        let dir = tempfile::tempdir().unwrap();
        let out = concat!(
            "stable-diffusion.cpp:6575 - get_learned_condition completed, taking 22.30s\r\n[INFO ] ",
            "\r  |================>                                 | 1/3 - 1.97s/it\u{1b}[K",
            "\r  |=================================>                | 2/3 - 5.88it/s\u{1b}[K",
            "\r  |==================================================| 3/3 - 5.74it/s\u{1b}[K\r\n",
            "stable-diffusion.cpp:7037 - sampling completed, taking 11.60s\r\n[INFO ] "
        );
        let (seen, progress) = recorder();
        let log = dir.path().join("sd-video.log");
        let mut parser = ProgressParser::new(Some(progress));
        pump(out.as_bytes(), &log, &mut parser).await;
        assert_eq!(
            *seen.lock(),
            [
                "LOADING 0/0",
                "SAMPLING 0/0",
                "SAMPLING 1/3",
                "SAMPLING 2/3",
                "SAMPLING 3/3",
                "DECODING 0/0"
            ]
        );
        let text = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.iter().filter(|l| l.contains("/3 - ")).count(),
            1,
            "only the last bar is logged"
        );
        assert!(lines
            .iter()
            .any(|l| l.contains("| 3/3 - 5.74it/s") && !l.contains('\u{1b}')));
        assert!(
            lines.iter().all(|l| l.trim() != "[INFO ]"),
            "stray level tags are left out"
        );
    }

    #[test]
    fn bars_are_recognised_with_or_without_colour_codes() {
        assert!(ProgressParser::is_bar("|===>   | 3/20 - 1.00s/it\u{1b}[K"));
        assert!(!ProgressParser::is_bar(
            "[INFO ] sampling using Euler method"
        ));
        assert_eq!(ProgressParser::strip_ansi("abc\u{1b}[K"), "abc");
    }

    #[test]
    fn catalog_defaults_are_strings() {
        // request_for reads plain strings; a number that does not parse falls back rather than failing.
        let d: BTreeMap<String, String> = [("frames", "lots"), ("fps", "0")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let r = VideoEngine::request_for(&d, "x");
        assert_eq!(r.frames, 33);
        assert_eq!(r.fps, 1);
    }

    /// A fake sd: prints what sd-cli prints for a clip and writes the file after `-o`; a prompt of
    /// "slow" hangs (to be stopped), "fail" exits with an error.
    #[cfg(windows)]
    pub(crate) fn fake_sd(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("sd-cli.cmd");
        std::fs::write(
            &path,
            concat!(
                "@echo off\r\n",
                "set out=\r\n",
                "set prompt=\r\n",
                ":loop\r\n",
                "if \"%~1\"==\"\" goto done\r\n",
                "if \"%~1\"==\"-p\" set prompt=%~2\r\n",
                "if \"%~1\"==\"-o\" set out=%~2\r\n",
                "shift\r\n",
                "goto loop\r\n",
                ":done\r\n",
                "echo [INFO ] loading diffusion model\r\n",
                "if \"%prompt%\"==\"fail\" (echo [ERROR] out of memory 1>&2 & exit /b 1)\r\n",
                "echo get_learned_condition completed, taking 1.00s\r\n",
                "if \"%prompt%\"==\"slow\" ping -n 60 127.0.0.1 >nul\r\n",
                "echo   ^|==^>       ^| 1/2 - 1.00s/it\r\n",
                "echo   ^|==========^| 2/2 - 1.00s/it\r\n",
                "echo sampling completed, taking 2.00s\r\n",
                "echo decode_first_stage completed, taking 1.00s\r\n",
                "echo avi> \"%out%\"\r\n",
                "echo generate_video completed in 4.00s\r\n",
            ),
        )
        .unwrap();
        path
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn renders_a_clip_with_progress_and_can_be_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let engine = VideoEngine::new(&fake_sd(&dir.path().join("sd")), &dir.path().join("logs"));
        let mut req = VideoEngine::request_for(&BTreeMap::new(), "a fox");
        let out = dir.path().join("videos").join("clip.avi");
        let (seen, progress) = recorder();
        let r = engine
            .generate(
                Path::new("m.gguf"),
                &req,
                &out,
                Some(progress),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(r.file, out);
        assert!(out.is_file());
        assert_eq!(r.request, req);
        assert_eq!(
            *seen.lock(),
            [
                "LOADING 0/0",
                "SAMPLING 0/0",
                "SAMPLING 1/2",
                "SAMPLING 2/2",
                "DECODING 0/0",
                "SAVING 0/0"
            ]
        );

        req.prompt = "fail".into();
        let failed = dir.path().join("videos").join("failed.avi");
        let err = engine
            .generate(
                Path::new("m.gguf"),
                &req,
                &failed,
                None,
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(!err.is::<Stopped>());
        let msg = err.to_string();
        assert!(msg.starts_with("Video engine failed (exit 1). "), "{msg}");
        assert!(
            msg.contains("out of memory"),
            "stderr reaches the log: {msg}"
        );

        req.prompt = "slow".into();
        let stopped = dir.path().join("videos").join("stopped.avi");
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            c.cancel();
        });
        let t0 = Instant::now();
        let err = engine
            .generate(Path::new("m.gguf"), &req, &stopped, None, &cancel)
            .await
            .unwrap_err();
        assert!(err.is::<Stopped>(), "{err}");
        assert_eq!(err.to_string(), "Video generation was stopped.");
        assert!(t0.elapsed() < Duration::from_secs(30), "stopped part way");
        assert!(!stopped.exists());

        let log = std::fs::read_to_string(dir.path().join("logs").join("sd-video.log")).unwrap();
        assert!(
            log.contains("vid_gen") && log.contains("2/2 - 1.00s/it"),
            "{log}"
        );
    }
}

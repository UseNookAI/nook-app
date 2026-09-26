//! Ports `runtime/ImageEngine.java`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::engine_process::{append_log, java_double, kill_tree, log_stdio, log_tail};
use super::model_registry::now_iso;

/// How long one image may take.
pub const TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// One image to render (`ImageEngine.Request`).
///
/// - `loader`: `checkpoint` (one file, `-m`) or `diffusion` (a bare diffusion model with separate
///   VAE and text encoder, `--diffusion-model`)
/// - `vae`: file name beside the model passed as `--vae`, or None
/// - `llm`: file name beside the model passed as `--llm`, or None
/// - `flags`: extra command-line flags, space separated, e.g. `--offload-to-cpu --diffusion-fa`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRequest {
    pub prompt: String,
    pub negative_prompt: String,
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    pub cfg_scale: f64,
    pub seed: u64,
    pub sampler: String,
    pub weight_type: String,
    pub loader: String,
    pub vae: Option<String>,
    pub llm: Option<String>,
    pub flags: String,
}

/// A rendered image (`ImageEngine.Result`); `elapsed_ms` is the Java `Duration elapsed`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageResult {
    pub file: PathBuf,
    pub seed: u64,
    pub elapsed_ms: u64,
}

impl ImageResult {
    pub fn elapsed(&self) -> Duration {
        Duration::from_millis(self.elapsed_ms)
    }
}

/// Text-to-image through the stable-diffusion.cpp command line. Each request runs one `sd`
/// process that loads the checkpoint, renders, writes a PNG and exits, so VRAM is held only while
/// an image is being made. Turbo checkpoints render in one to four steps, which keeps the per-call
/// model load acceptable for chat use.
pub struct ImageEngine {
    exe: PathBuf,
    log_dir: PathBuf,
    output_dir: PathBuf,
}

impl ImageEngine {
    pub fn new(exe: &Path, log_dir: &Path, output_dir: &Path) -> ImageEngine {
        ImageEngine {
            exe: exe.to_path_buf(),
            log_dir: log_dir.to_path_buf(),
            output_dir: output_dir.to_path_buf(),
        }
    }

    pub fn output_dir(&self) -> &Path {
        &self.output_dir
    }

    pub fn is_available(&self) -> bool {
        self.exe.exists()
    }

    /// Builds a request from catalog defaults, letting the caller override size and steps.
    pub fn request_for(
        defaults: &BTreeMap<String, String>,
        prompt: &str,
        negative: Option<&str>,
        width: Option<u32>,
        height: Option<u32>,
        steps: Option<u32>,
    ) -> ImageRequest {
        let w = width
            .filter(|w| *w > 0)
            .unwrap_or_else(|| parse_int(defaults.get("width"), 512));
        let h = height
            .filter(|h| *h > 0)
            .unwrap_or_else(|| parse_int(defaults.get("height"), 512));
        let s = steps
            .filter(|s| *s > 0)
            .unwrap_or_else(|| parse_int(defaults.get("steps"), 4));
        ImageRequest {
            prompt: prompt.to_string(),
            negative_prompt: negative.unwrap_or("").to_string(),
            width: round_to_64(w),
            height: round_to_64(h),
            steps: s,
            cfg_scale: parse_double(defaults.get("cfg"), 1.0),
            seed: random_seed(),
            sampler: text_or(defaults, "sampler", "euler"),
            weight_type: text_or(defaults, "weightType", "f16"),
            loader: text_or(defaults, "loader", "checkpoint"),
            vae: blank_to_none(defaults.get("vae")),
            llm: blank_to_none(defaults.get("llm")),
            flags: text_or(defaults, "flags", ""),
        }
    }

    /// The command line for one image (the executable first).
    pub fn command(&self, model_file: &Path, req: &ImageRequest, out: &Path) -> Vec<String> {
        let diffusion = req.loader.eq_ignore_ascii_case("diffusion");
        let mut cmd: Vec<String> = vec![
            self.exe.display().to_string(),
            "-M".into(),
            "img_gen".into(),
            if diffusion { "--diffusion-model" } else { "-m" }.into(),
            model_file.display().to_string(),
            "-p".into(),
            req.prompt.clone(),
            "-W".into(),
            req.width.to_string(),
            "-H".into(),
            req.height.to_string(),
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
        if !req.weight_type.trim().is_empty() {
            cmd.push("--type".into());
            cmd.push(req.weight_type.clone());
        }
        if let Some(vae) = &req.vae {
            cmd.push("--vae".into());
            cmd.push(model_file.with_file_name(vae).display().to_string());
        }
        if let Some(llm) = &req.llm {
            cmd.push("--llm".into());
            cmd.push(model_file.with_file_name(llm).display().to_string());
        }
        cmd.extend(req.flags.split_whitespace().map(String::from));
        if !req.negative_prompt.trim().is_empty() {
            cmd.push("-n".into());
            cmd.push(req.negative_prompt.clone());
        }
        cmd
    }

    /// Renders one image into `<output_dir>\<uuid>.png`. `gpu` is the caller's intent; the engine
    /// picks its device itself (as in the original, which did not pass it on).
    pub async fn generate(
        &self,
        model_file: &Path,
        req: &ImageRequest,
        _gpu: bool,
    ) -> Result<ImageResult> {
        if !self.exe.exists() {
            bail!("Image engine binary missing: {}", self.exe.display());
        }
        for dir in [&self.output_dir, &self.log_dir] {
            tokio::fs::create_dir_all(dir)
                .await
                .with_context(|| format!("Could not create {}", dir.display()))?;
        }
        let name = format!("{}.png", uuid::Uuid::new_v4());
        let out = self.output_dir.join(&name);
        let log_file = self.log_dir.join("sd.log");
        let cmd = self.command(model_file, req, &out);
        append_log(
            &log_file,
            &format!("\n=== {} {}\n", now_iso(), cmd.join(" ")),
        );
        let started = Instant::now();
        let (stdout, stderr) = log_stdio(&log_file)?;
        let mut c = crate::process::command(&cmd[0]);
        c.args(&cmd[1..]).stdout(stdout).stderr(stderr);
        if let Some(dir) = self.exe.parent() {
            c.current_dir(dir);
        }
        let mut child = crate::process::spawn_managed(&mut c).map_err(|e| {
            anyhow!(
                "Could not start the image engine {}: {e}",
                self.exe.display()
            )
        })?;
        let status = match tokio::time::timeout(TIMEOUT, child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                kill_tree(&mut child).await;
                let _ = child.wait().await;
                bail!("Image generation timed out after 15 minutes.");
            }
        };
        let code = status.code().unwrap_or(-1);
        if code != 0 || !out.exists() {
            bail!(
                "Image engine failed (exit {code}). {}",
                log_tail(&log_file, 15)
            );
        }
        let elapsed = started.elapsed();
        tracing::info!(
            "Image {name} rendered in {} ms ({}x{}, {} steps)",
            elapsed.as_millis(),
            req.width,
            req.height,
            req.steps
        );
        Ok(ImageResult {
            file: out,
            seed: req.seed,
            elapsed_ms: elapsed.as_millis() as u64,
        })
    }
}

/// A fresh seed below two billion (`Math.abs(nextLong() % 2_000_000_000L)`).
pub(crate) fn random_seed() -> u64 {
    (rand::random::<i64>() % 2_000_000_000).unsigned_abs()
}

fn round_to_64(v: u32) -> u32 {
    ((v + 32) / 64 * 64).max(64)
}

pub(crate) fn blank_to_none(s: Option<&String>) -> Option<String> {
    s.map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub(crate) fn text_or(defaults: &BTreeMap<String, String>, key: &str, fallback: &str) -> String {
    defaults
        .get(key)
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

pub(crate) fn parse_int(s: Option<&String>, def: u32) -> u32 {
    s.and_then(|s| s.trim().parse::<i64>().ok())
        .map(|n| n.clamp(0, u32::MAX as i64) as u32)
        .unwrap_or(def)
}

pub(crate) fn parse_double(s: Option<&String>, def: f64) -> f64 {
    s.and_then(|s| s.trim().parse().ok()).unwrap_or(def)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn requests_come_from_the_catalog_defaults_rounded_to_what_the_model_takes() {
        let r = ImageEngine::request_for(&BTreeMap::new(), "a fox", None, None, None, None);
        assert_eq!((r.width, r.height, r.steps), (512, 512, 4));
        assert_eq!(r.cfg_scale, 1.0);
        assert_eq!(r.sampler, "euler");
        assert_eq!(r.weight_type, "f16");
        assert_eq!(r.loader, "checkpoint");
        assert_eq!(r.negative_prompt, "");
        assert!(r.seed < 2_000_000_000);

        let d = defaults(&[
            ("width", "1000"),
            ("height", "x"),
            ("steps", "8"),
            ("cfg", "3.5"),
            ("vae", " vae.safetensors "),
            ("llm", ""),
            ("loader", "diffusion"),
        ]);
        let r = ImageEngine::request_for(&d, "p", Some("blurry"), Some(700), Some(0), Some(2));
        assert_eq!(r.width, 704, "the caller's size, to a multiple of 64");
        assert_eq!(r.height, 512, "an unreadable default falls back");
        assert_eq!(r.steps, 2);
        assert_eq!(r.cfg_scale, 3.5);
        assert_eq!(r.vae.as_deref(), Some("vae.safetensors"));
        assert_eq!(r.llm, None);
        assert_eq!(round_to_64(10), 64);
        assert_eq!(round_to_64(1000), 1024);
    }

    #[test]
    fn the_command_names_every_file_beside_the_model() {
        let engine = ImageEngine::new(
            Path::new("sd-cli.exe"),
            Path::new("logs"),
            Path::new("images"),
        );
        let model = Path::new("C:/m/z/z-image-turbo.gguf");
        let mut r = ImageEngine::request_for(
            &defaults(&[
                ("loader", "diffusion"),
                ("vae", "ae.safetensors"),
                ("llm", "qwen.gguf"),
                ("flags", " --offload-to-cpu  --diffusion-fa "),
            ]),
            "a cat",
            Some("dark"),
            None,
            None,
            None,
        );
        r.seed = 42;
        let cmd = engine.command(model, &r, Path::new("images/x.png"));
        let after = |flag: &str| cmd[cmd.iter().position(|a| a == flag).unwrap() + 1].clone();
        assert_eq!(after("-M"), "img_gen");
        assert_eq!(after("--diffusion-model"), model.display().to_string());
        assert_eq!(
            after("--vae"),
            model.with_file_name("ae.safetensors").display().to_string()
        );
        assert_eq!(
            after("--llm"),
            model.with_file_name("qwen.gguf").display().to_string()
        );
        assert_eq!(after("--cfg-scale"), "1.0");
        assert_eq!(after("-s"), "42");
        assert_eq!(after("--type"), "f16");
        assert_eq!(after("-n"), "dark");
        assert!(cmd.contains(&"--offload-to-cpu".into()) && cmd.contains(&"--diffusion-fa".into()));
        assert!(
            !cmd.contains(&String::new()),
            "doubled spaces make no empty arguments"
        );

        let plain = ImageEngine::request_for(&BTreeMap::new(), "a cat", None, None, None, None);
        let cmd = engine.command(model, &plain, Path::new("x.png"));
        assert!(cmd.contains(&"-m".into()) && !cmd.contains(&"--diffusion-model".into()));
        assert!(!cmd.contains(&"-n".into()) && !cmd.contains(&"--vae".into()));
    }

    /// A fake sd: writes the file after `-o` (or exits with an error when the prompt is "fail").
    #[cfg(windows)]
    fn fake_sd(dir: &Path) -> PathBuf {
        let path = dir.join("sd-cli.cmd");
        std::fs::write(
            &path,
            concat!(
                "@echo off\r\n",
                "set out=\r\n",
                ":loop\r\n",
                "if \"%~1\"==\"\" goto done\r\n",
                "if \"%~1\"==\"-p\" if \"%~2\"==\"fail\" (echo out of memory & exit /b 1)\r\n",
                "if \"%~1\"==\"-o\" set out=%~2\r\n",
                "shift\r\n",
                "goto loop\r\n",
                ":done\r\n",
                "echo rendering\r\n",
                "echo png> \"%out%\"\r\n",
            ),
        )
        .unwrap();
        path
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn renders_one_png_per_request_and_names_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let engine = ImageEngine::new(
            &fake_sd(dir.path()),
            &dir.path().join("logs"),
            &dir.path().join("images"),
        );
        assert!(engine.is_available());
        let req = ImageEngine::request_for(&BTreeMap::new(), "a fox", None, None, None, None);
        let result = engine
            .generate(&dir.path().join("m.gguf"), &req, true)
            .await
            .unwrap();
        assert!(result.file.is_file());
        assert_eq!(result.file.parent().unwrap(), dir.path().join("images"));
        assert_eq!(result.file.extension().unwrap(), "png");
        assert_eq!(result.seed, req.seed);
        let log = std::fs::read_to_string(dir.path().join("logs").join("sd.log")).unwrap();
        assert!(
            log.contains("img_gen") && log.contains("rendering"),
            "{log}"
        );

        let failing = ImageEngine::request_for(&BTreeMap::new(), "fail", None, None, None, None);
        let err = engine
            .generate(&dir.path().join("m.gguf"), &failing, true)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("Image engine failed (exit 1). "), "{err}");
        assert!(err.contains("out of memory"), "{err}");

        let missing = ImageEngine::new(&dir.path().join("sd.exe"), dir.path(), dir.path());
        let err = missing
            .generate(Path::new("m"), &req, false)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("Image engine binary missing: "));
    }
}

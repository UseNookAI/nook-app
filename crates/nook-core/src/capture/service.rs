//! The screen recorder: one recording or stream at a time, run by FFmpeg (a runtime component,
//! downloaded when first wanted), its sound mixed by Nook ([`super::audio`]) and handed over
//! through a named pipe.
//!
//! A recording is made of parts: each run of FFmpeg writes one Matroska file (safe to keep if
//! Nook or the computer stops mid-way), a pause ends one and resuming starts the next, and
//! stopping joins them into one MP4 beside them. A stream is one run, its bits sent to the
//! service as they are made; a stream that is recorded too writes its part as well. FFmpeg's
//! progress (`-progress pipe:1`) is read into [`CaptureState`], which goes out on
//! [`topic::CAPTURE`] as `{"state": ..}`; the sound's loudness as `{"levels": [..]}`, and the
//! download as `{"install": ..}`.
//!
//! On a Mac, Nook captures the picture itself (ScreenCaptureKit, [`super::mac`]) and hands it to
//! FFmpeg through a named pipe (a FIFO), as it does the sound there.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use super::audio::{self, AudioDevice, AudioSource, Mixer};
use super::plan::{self, Encoder, Quality, Source, Target, Video};
use super::secret;
use super::sources::{self, Screen, Window};
use crate::busy::BusyWork;
use crate::convert::service::free_path;
use crate::events::{self, topic};
use crate::flow::Install;
use crate::runtime::{Backend, EngineComponent, RuntimeManager, StagedProgress};
use crate::settings::Settings;

/// How long FFmpeg may take to start capturing before the attempt counts as failed.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// How long FFmpeg may take to finish a part after `q`.
const STOP_TIMEOUT: Duration = Duration::from_secs(20);

/// What there is to record, and whether FFmpeg is in.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sources {
    pub screens: Vec<Screen>,
    pub windows: Vec<Window>,
    pub microphones: Vec<AudioDevice>,
    pub speakers: Vec<AudioDevice>,
    /// Whether FFmpeg, which records and streams, is in; else its download's size.
    pub ready: bool,
    pub download_bytes: u64,
    /// The folder recordings go to unless another is chosen.
    pub folder: String,
}

/// Where a stream goes: the service's server address and the stream key, and its bit rate.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamOptions {
    pub server: String,
    pub key: String,
    pub kbps: u32,
}

/// A recording or a stream, as the page asks for it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartOptions {
    pub source: Source,
    pub fps: u32,
    /// The height to scale down to (1080, 720), or None for the source's own.
    pub scale_to: Option<u32>,
    pub quality: Quality,
    pub cursor: bool,
    #[serde(default)]
    pub audio: Vec<AudioSource>,
    /// Whether to keep a file (always, without a stream).
    pub record: bool,
    pub folder: Option<String>,
    pub stream: Option<StreamOptions>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    #[default]
    Idle,
    Starting,
    Live,
    Paused,
    Finishing,
}

/// A finished recording.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Saved {
    pub path: String,
    pub bytes: u64,
    pub seconds: f64,
}

/// Where the recorder stands, for the page and the recording controls.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureState {
    pub phase: Phase,
    pub recording: bool,
    pub streaming: bool,
    /// Time recorded (pauses left out).
    pub seconds: f64,
    pub bytes: u64,
    pub fps: f64,
    pub kbps: f64,
    pub dropped: u64,
    /// What encodes: "NVIDIA graphics card".
    pub encoder: Option<String>,
    pub width: u32,
    pub height: u32,
    /// The screen being recorded (its handle), for where the controls go.
    pub monitor: Option<u64>,
    /// Why the last recording ended or did not start.
    pub error: Option<String>,
    /// The stream failed, while the recording goes on.
    pub stream_error: Option<String>,
    pub saved: Option<Saved>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Progress {
    seconds: f64,
    bytes: u64,
}

/// A running FFmpeg: one part of a recording, or a stream.
struct Segment {
    stdin: Option<tokio::process::ChildStdin>,
    /// Set when it has ended, with the end of what it printed.
    exited: tokio::sync::watch::Receiver<Option<String>>,
    /// Set when it was told to end, so its end is not taken for a failure.
    asked: Arc<AtomicBool>,
    kill: CancellationToken,
    part: Option<PathBuf>,
    progress: Arc<Mutex<Progress>>,
    /// What feeds FFmpeg its picture on a Mac (the capture and its writer), kept until FFmpeg ends.
    feed: Option<Box<dyn std::any::Any + Send>>,
}

struct Session {
    options: StartOptions,
    url: Option<String>,
    key: String,
    video: Video,
    /// The encoders still to try, the one in use first.
    encoders: Vec<Encoder>,
    mixer: Option<Mixer>,
    ffmpeg: PathBuf,
    folder: PathBuf,
    base: String,
    parts: Vec<PathBuf>,
    segment: Option<Segment>,
    /// What the finished parts hold.
    before: Progress,
}

pub struct CaptureService {
    runtime: Option<Arc<RuntimeManager>>,
    settings: Arc<Settings>,
    /// FFmpeg to use instead of the runtime's (tests).
    ffmpeg: Option<PathBuf>,
    state: Mutex<CaptureState>,
    session: tokio::sync::Mutex<Option<Session>>,
    /// The sound's meters while nothing records.
    meter: Mutex<Option<Mixer>>,
    encoders: tokio::sync::Mutex<Option<Vec<Encoder>>>,
    install: Mutex<Option<Install>>,
    install_cancel: Mutex<Option<CancellationToken>>,
    /// Recordings made in this run, which the page may open.
    saved: Mutex<Vec<PathBuf>>,
    stopping: CancellationToken,
    #[cfg_attr(not(windows), allow(dead_code))]
    next: AtomicU64,
    me: Weak<CaptureService>,
}

impl CaptureService {
    pub fn new(runtime: Arc<RuntimeManager>, settings: Arc<Settings>) -> Arc<CaptureService> {
        Self::build(Some(runtime), settings, None)
    }

    /// A recorder that runs `ffmpeg` (tests).
    pub fn with_ffmpeg(ffmpeg: PathBuf, settings: Arc<Settings>) -> Arc<CaptureService> {
        Self::build(None, settings, Some(ffmpeg))
    }

    fn build(
        runtime: Option<Arc<RuntimeManager>>,
        settings: Arc<Settings>,
        ffmpeg: Option<PathBuf>,
    ) -> Arc<CaptureService> {
        Arc::new_cyclic(|me| CaptureService {
            runtime,
            settings,
            ffmpeg,
            state: Mutex::new(CaptureState::default()),
            session: tokio::sync::Mutex::new(None),
            meter: Mutex::new(None),
            encoders: tokio::sync::Mutex::new(None),
            install: Mutex::new(None),
            install_cancel: Mutex::new(None),
            saved: Mutex::new(Vec::new()),
            stopping: CancellationToken::new(),
            next: AtomicU64::new(1),
            me: me.clone(),
        })
    }

    // ------------------------------------------------------------------ what there is

    /// The screens, windows and sound devices, and whether FFmpeg is in. Call it on the blocking
    /// pool: listing the sound devices takes a moment.
    pub fn sources(&self) -> Sources {
        Sources {
            screens: sources::screens(),
            windows: sources::windows(),
            microphones: audio::microphones(),
            speakers: audio::speakers(),
            ready: self.ffmpeg_path().is_some(),
            download_bytes: self.download_bytes(),
            folder: default_folder().display().to_string(),
        }
    }

    fn ffmpeg_path(&self) -> Option<PathBuf> {
        if let Some(f) = &self.ffmpeg {
            return f.is_file().then(|| f.clone());
        }
        let runtime = self.runtime.as_ref()?;
        let packages = runtime.packages();
        if !packages.is_installed(EngineComponent::Ffmpeg, Backend::Cpu) {
            return None;
        }
        let exe = packages.executable(
            EngineComponent::Ffmpeg,
            Backend::Cpu,
            EngineComponent::Ffmpeg.executables(),
        );
        exe.is_file().then_some(exe)
    }

    fn download_bytes(&self) -> u64 {
        self.runtime
            .as_ref()
            .and_then(|r| {
                r.packages()
                    .package_for(EngineComponent::Ffmpeg, Backend::Cpu)
                    .ok()
                    .map(|p| p.total_bytes())
            })
            .unwrap_or(0)
    }

    /// The size of what `source` captures before cropping: its screen's, or the window's.
    fn captured_size(source: &Source) -> Result<(u32, u32)> {
        let screen = |handle: u64| {
            sources::screens()
                .into_iter()
                .find(|s| s.handle == handle)
                .map(|s| (s.width, s.height))
                .ok_or_else(|| anyhow!("That screen is no longer connected."))
        };
        match source {
            Source::Screen { handle } => screen(*handle),
            Source::Area {
                screen: s,
                x,
                y,
                width,
                height,
            } => {
                let (w, h) = screen(*s)?;
                if *width < 16 || *height < 16 || x + width > w || y + height > h {
                    bail!("That area is not on its screen: choose it again.");
                }
                Ok((w, h))
            }
            Source::Window { handle } => sources::windows()
                .into_iter()
                .find(|w| w.handle == *handle)
                .map(|w| (w.width, w.height))
                .ok_or_else(|| {
                    anyhow!("That window is closed or minimised: choose another, or restore it.")
                }),
        }
    }

    /// One frame of `source`, as a PNG about 360 lines high, to show what it records.
    pub async fn preview(&self, source: &Source) -> Result<Vec<u8>> {
        let ffmpeg = self
            .ffmpeg_path()
            .or_else(|| cfg!(target_os = "macos").then(PathBuf::new))
            .ok_or_else(|| anyhow!("FFmpeg is not installed yet."))?;
        let captured = Self::captured_size(source)?;
        let video = Video {
            source: source.clone(),
            captured,
            fps: 5,
            scale_to: Some(360),
            cursor: false,
            encoder: Encoder::Nvenc,
            kbps: 1_000,
        };
        // A Mac's picture comes from ScreenCaptureKit, with no FFmpeg in between.
        if cfg!(target_os = "macos") {
            let _ = ffmpeg;
            return Self::mac_preview(source.clone(), video.size()).await;
        }
        let filter = format!("{},hwdownload,format=bgra", plan::capture_filter(&video));
        let mut cmd = crate::process::command(&ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg(filter)
            .args(["-frames:v", "1", "-f", "image2pipe", "-c:v", "png", "-"]);
        let out = tokio::time::timeout(Duration::from_secs(10), cmd.output())
            .await
            .map_err(|_| anyhow!("The picture did not come in time."))??;
        if !out.status.success() || out.stdout.is_empty() {
            bail!(
                "No picture came: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(out.stdout)
    }

    #[cfg(target_os = "macos")]
    async fn mac_preview(source: Source, size: (u32, u32)) -> Result<Vec<u8>> {
        tokio::task::spawn_blocking(move || super::mac::preview(&source, size))
            .await
            .map_err(|e| anyhow!("{e}"))?
    }

    #[cfg(not(target_os = "macos"))]
    async fn mac_preview(_source: Source, _size: (u32, u32)) -> Result<Vec<u8>> {
        bail!("No picture came.")
    }

    /// [`CaptureService::preview`] as a `data:` address the page shows as it is.
    pub async fn preview_url(&self, source: &Source) -> Result<String> {
        use base64::Engine as _;
        let png = self.preview(source).await?;
        Ok(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png)
        ))
    }

    // ------------------------------------------------------------------ the sound's meters

    /// Opens `audio` to show how loud each is (`{"levels": [..]}`), until
    /// [`CaptureService::stop_listening`] or a recording; a recording shows its own. Blocks while
    /// the devices open.
    pub fn listen(&self, audio: &[AudioSource]) -> Result<()> {
        self.stop_listening();
        if audio.is_empty() || self.state.lock().phase != Phase::Idle {
            return Ok(());
        }
        let mixer = Mixer::start(audio, |levels| {
            events::emit(topic::CAPTURE, json!({ "levels": levels }))
        })?;
        *self.meter.lock() = Some(mixer);
        Ok(())
    }

    pub fn stop_listening(&self) {
        if let Some(m) = self.meter.lock().take() {
            m.stop();
        }
    }

    // ------------------------------------------------------------------ the state

    pub fn state(&self) -> CaptureState {
        self.state.lock().clone()
    }

    fn set(&self, f: impl FnOnce(&mut CaptureState)) {
        let state = {
            let mut s = self.state.lock();
            f(&mut s);
            s.clone()
        };
        events::emit(topic::CAPTURE, json!({ "state": state }));
    }

    /// Whether `path` is a recording made in this run (what the page may open or show).
    pub fn is_saved(&self, path: &Path) -> bool {
        self.saved.lock().iter().any(|p| p == path)
    }

    // ------------------------------------------------------------------ recording

    /// Starts recording or streaming as `options` say. The meters stop; the recording's own
    /// sound reports its levels.
    pub async fn start(&self, options: StartOptions) -> Result<CaptureState> {
        if self.stopping.is_cancelled() {
            bail!("Nook is closing.");
        }
        let mut guard = self.session.lock().await;
        if guard.is_some() {
            bail!("A recording is already running.");
        }
        if !cfg!(windows) && !cfg!(target_os = "macos") {
            bail!("Screen recording works on Windows and macOS.");
        }
        if !options.record && options.stream.is_none() {
            bail!("Choose to record, to stream, or both.");
        }
        let ffmpeg = self
            .ffmpeg_path()
            .ok_or_else(|| anyhow!("FFmpeg is not installed yet: download it first."))?;
        self.stop_listening();
        self.set(|s| {
            *s = CaptureState {
                phase: Phase::Starting,
                ..CaptureState::default()
            }
        });
        let session = match self.open_session(options, ffmpeg).await {
            Ok(s) => s,
            Err(e) => {
                let why = format!("{e:#}");
                self.set(|s| {
                    s.phase = Phase::Idle;
                    s.error = Some(why.clone());
                });
                return Err(e);
            }
        };
        *guard = Some(session);
        Ok(self.state())
    }

    async fn open_session(&self, options: StartOptions, ffmpeg: PathBuf) -> Result<Session> {
        let captured = Self::captured_size(&options.source)?;
        let fps = options.fps.clamp(10, 60);
        let encoders = self.working_encoders(&ffmpeg).await;
        if encoders.is_empty() {
            bail!("No H.264 encoder works on this computer.");
        }
        let (url, key) = match &options.stream {
            Some(s) => {
                if !s.server.trim().starts_with("rtmp://")
                    && !s.server.trim().starts_with("rtmps://")
                    && !s.server.trim().starts_with("srt://")
                {
                    bail!("The stream's server address starts with rtmp://, rtmps:// or srt://.");
                }
                // A Mac's FFmpeg is built without SRT (it would need a library of its own).
                if cfg!(target_os = "macos") && s.server.trim().starts_with("srt://") {
                    bail!("The Mac streams over RTMP or RTMPS: SRT is not there yet.");
                }
                (
                    Some(plan::stream_url(&s.server, &s.key)),
                    s.key.trim().to_string(),
                )
            }
            None => (None, String::new()),
        };
        let mut video = Video {
            source: options.source.clone(),
            captured,
            fps,
            scale_to: options.scale_to,
            cursor: options.cursor,
            encoder: encoders[0],
            kbps: 0,
        };
        let (w, h) = video.size();
        video.kbps = match &options.stream {
            Some(s) => s.kbps.clamp(500, 50_000),
            None => plan::bitrate_kbps(w, h, fps, options.quality),
        };
        let folder = match &options.folder {
            Some(f) if !f.trim().is_empty() => PathBuf::from(f.trim()),
            _ => default_folder(),
        };
        if options.record {
            std::fs::create_dir_all(&folder)
                .with_context(|| format!("Could not create {}", folder.display()))?;
        }
        let mixer = if options.audio.is_empty() {
            None
        } else {
            let a = options.audio.clone();
            Some(
                tokio::task::spawn_blocking(move || {
                    Mixer::start(&a, |levels| {
                        events::emit(topic::CAPTURE, json!({ "levels": levels }))
                    })
                })
                .await
                .map_err(|e| anyhow!("{e}"))??,
            )
        };
        let monitor = match &options.source {
            Source::Screen { handle } => Some(*handle),
            Source::Area { screen, .. } => Some(*screen),
            Source::Window { handle } => sources::monitor_of_window(*handle),
        };
        let base = chrono::Local::now()
            .format(if options.stream.is_some() {
                "Stream %Y-%m-%d at %H.%M.%S"
            } else {
                "Screen recording %Y-%m-%d at %H.%M.%S"
            })
            .to_string();
        let mut session = Session {
            url,
            key,
            video,
            encoders,
            mixer,
            ffmpeg,
            folder,
            base,
            parts: Vec::new(),
            segment: None,
            before: Progress::default(),
            options,
        };
        self.start_segment(&mut session).await?;
        let (w, h) = session.video.size();
        let (recording, streaming) = (session.options.record, session.url.is_some());
        let encoder = session.video.encoder.name().to_string();
        self.set(|s| {
            s.phase = Phase::Live;
            s.recording = recording;
            s.streaming = streaming;
            s.encoder = Some(encoder);
            s.width = w;
            s.height = h;
            s.monitor = monitor;
        });
        Ok(session)
    }

    /// The encoders that work here, the best first, found once by encoding a few frames with each.
    async fn working_encoders(&self, ffmpeg: &Path) -> Vec<Encoder> {
        let mut known = self.encoders.lock().await;
        if let Some(k) = known.as_ref() {
            return k.clone();
        }
        let probes: Vec<_> = Encoder::ALL
            .iter()
            .map(|&e| {
                let ffmpeg = ffmpeg.to_path_buf();
                tokio::spawn(async move { (e, probe(&ffmpeg, e).await) })
            })
            .collect();
        let mut works = Vec::new();
        for p in probes {
            if let Ok((e, true)) = p.await {
                works.push(e);
            }
        }
        works.sort_by_key(|e| Encoder::ALL.iter().position(|a| a == e));
        tracing::info!("Screen recording encoders that work here: {works:?}");
        *known = Some(works.clone());
        works
    }

    /// Starts FFmpeg for the next part (or the stream), trying the next encoder when one does not
    /// start.
    async fn start_segment(&self, session: &mut Session) -> Result<()> {
        loop {
            session.video.encoder = session.encoders[0];
            match self.try_segment(session).await {
                Ok(segment) => {
                    session.segment = Some(segment);
                    return Ok(());
                }
                Err(Failed::Encoder(why)) if session.encoders.len() > 1 => {
                    tracing::warn!(
                        "Recording with {} did not start ({why}); trying {}",
                        session.encoders[0].name(),
                        session.encoders[1].name()
                    );
                    session.encoders.remove(0);
                    self.encoders.lock().await.replace(session.encoders.clone());
                }
                Err(Failed::Encoder(why)) | Err(Failed::Other(why)) => bail!(why),
            }
        }
    }

    async fn try_segment(&self, session: &mut Session) -> std::result::Result<Segment, Failed> {
        let part = session.options.record.then(|| {
            session.folder.join(format!(
                "{} (part {}).mkv",
                session.base,
                session.parts.len() + 1
            ))
        });
        let target = match (&part, &session.url) {
            (Some(p), Some(u)) => Target::Both(p.clone(), u.clone()),
            (Some(p), None) => Target::File(p.clone()),
            (None, Some(u)) => Target::Stream(u.clone()),
            (None, None) => return Err(Failed::Other("Nothing to record into.".into())),
        };
        let pipe = match &session.mixer {
            Some(m) => Some(
                self.audio_pipe(m)
                    .map_err(|e| Failed::Other(format!("{e:#}")))?,
            ),
            None => None,
        };
        #[cfg(not(target_os = "macos"))]
        let (args, feed) = (
            plan::args(&session.video, pipe.as_deref(), &target),
            None::<Box<dyn std::any::Any + Send>>,
        );
        #[cfg(target_os = "macos")]
        let (args, feed) = {
            let video = session.video.clone();
            let (fifo, feed) = tokio::task::spawn_blocking(move || {
                super::mac::video_feed(&video, &std::env::temp_dir(), START_TIMEOUT)
            })
            .await
            .map_err(|e| Failed::Other(e.to_string()))?
            .map_err(|e| Failed::Other(format!("{e:#}")))?;
            let input = plan::raw_input(&session.video, &fifo.display().to_string());
            (
                plan::args_from(input, &session.video, pipe.as_deref(), &target),
                Some(Box::new(feed) as Box<dyn std::any::Any + Send>),
            )
        };
        let mut cmd = crate::process::command(&session.ffmpeg);
        cmd.args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| Failed::Other(format!("FFmpeg did not start: {e}")))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let progress = Arc::new(Mutex::new(Progress::default()));
        let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        // Its progress: the first block says it is capturing.
        if let Some(out) = stdout {
            let (me, progress) = (self.me.clone(), progress.clone());
            let before = session.before;
            tokio::spawn(async move {
                let mut started = Some(started_tx);
                let mut block = ProgressBlock::default();
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if !block.read(&line) {
                        continue;
                    }
                    if let Some(s) = started.take() {
                        let _ = s.send(());
                    }
                    *progress.lock() = Progress {
                        seconds: block.seconds,
                        bytes: block.bytes,
                    };
                    if let Some(me) = me.upgrade() {
                        let b = block.clone();
                        me.set(|s| {
                            s.seconds = before.seconds + b.seconds;
                            s.bytes = before.bytes + b.bytes;
                            s.fps = b.fps;
                            s.kbps = b.kbps;
                            s.dropped = b.dropped;
                        });
                    }
                }
            });
        }
        // What it says, the last lines kept; a stream that fails while the file goes on.
        if let Some(err) = stderr {
            let (me, tail, key) = (self.me.clone(), tail.clone(), session.key.clone());
            let streaming_too = matches!(target, Target::Both(..));
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let line = plan::hidden(line.trim(), &key);
                    if line.is_empty() {
                        continue;
                    }
                    tracing::debug!("FFmpeg: {line}");
                    if streaming_too && stream_failed(&line) {
                        if let Some(me) = me.upgrade() {
                            let l = line.clone();
                            me.set(|s| {
                                s.streaming = false;
                                s.stream_error = Some(format!(
                                    "The stream stopped; the recording goes on. ({l})"
                                ));
                            });
                        }
                    }
                    let mut t = tail.lock();
                    t.push_back(line);
                    if t.len() > 12 {
                        t.pop_front();
                    }
                }
            });
        }
        let (exited_tx, exited_rx) = tokio::sync::watch::channel::<Option<String>>(None);
        let asked = Arc::new(AtomicBool::new(false));
        let kill = CancellationToken::new();
        {
            let (me, asked, kill, tail) =
                (self.me.clone(), asked.clone(), kill.clone(), tail.clone());
            tokio::spawn(async move {
                let status = tokio::select! {
                    s = child.wait() => s.ok(),
                    _ = kill.cancelled() => {
                        let _ = child.kill().await;
                        None
                    }
                };
                // What it printed last comes in just after it ends.
                tokio::time::sleep(Duration::from_millis(150)).await;
                let said = tail.lock().iter().cloned().collect::<Vec<_>>().join("\n");
                let _ = exited_tx.send(Some(said.clone()));
                if !asked.load(Ordering::SeqCst) {
                    tracing::warn!("FFmpeg ended by itself ({status:?}): {said}");
                    if let Some(me) = me.upgrade() {
                        me.ended(said).await;
                    }
                }
            });
        }
        let mut exited = exited_rx.clone();
        tokio::select! {
            _ = started_rx => Ok(Segment {
                stdin,
                exited: exited_rx,
                asked,
                kill,
                part,
                progress,
                feed,
            }),
            ended = exited.wait_for(|e| e.is_some()) => {
                let said = ended.map(|e| e.clone().unwrap_or_default()).unwrap_or_default();
                if let Some(p) = &part {
                    let _ = std::fs::remove_file(p);
                }
                Err(classify(&said))
            }
            _ = tokio::time::sleep(START_TIMEOUT) => {
                asked.store(true, Ordering::SeqCst);
                kill.cancel();
                Err(Failed::Other("FFmpeg did not start capturing in time.".into()))
            }
        }
    }

    /// A named pipe for the sound, its writer waiting for FFmpeg to open it: the mixer is sent
    /// there from then (not before, so the sound starts with the picture).
    #[cfg(windows)]
    fn audio_pipe(&self, mixer: &Mixer) -> Result<String> {
        use tokio::net::windows::named_pipe::ServerOptions;
        let name = format!(
            r"\\.\pipe\nook-capture-{}-{}",
            std::process::id(),
            self.next.fetch_add(1, Ordering::SeqCst)
        );
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .access_inbound(false)
            .access_outbound(true)
            .out_buffer_size(1 << 20)
            .create(&name)
            .context("Could not make the sound's pipe")?;
        let attacher = mixer.attacher();
        tokio::spawn(async move {
            let mut server = server;
            if tokio::time::timeout(START_TIMEOUT, server.connect())
                .await
                .map_or(true, |r| r.is_err())
            {
                return;
            }
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<f32>>(512);
            attacher.attach(tx);
            while let Some(chunk) = rx.recv().await {
                if server.write_all(&audio::as_bytes(&chunk)).await.is_err() {
                    break;
                }
            }
            let _ = server.flush().await;
        });
        Ok(name)
    }

    /// A Mac's pipe for the sound: a FIFO, its writer waiting on a thread of its own for FFmpeg
    /// to open it, the mixer sent there from then.
    #[cfg(target_os = "macos")]
    fn audio_pipe(&self, mixer: &Mixer) -> Result<String> {
        use std::io::Write;
        let fifo = super::mac::fifo(&std::env::temp_dir(), "f32")?;
        let attacher = mixer.attacher();
        let path = fifo.clone();
        std::thread::Builder::new()
            .name("nook-capture-sound-pipe".into())
            .spawn(move || {
                let never = AtomicBool::new(false);
                let opened = super::mac::open_writer(&path, START_TIMEOUT, &never);
                let _ = std::fs::remove_file(&path);
                let Some(mut pipe) = opened else {
                    return;
                };
                let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<f32>>(512);
                attacher.attach(tx);
                while let Some(chunk) = rx.blocking_recv() {
                    if pipe.write_all(&audio::as_bytes(&chunk)).is_err() {
                        break;
                    }
                }
            })
            .context("Could not start the sound's pipe")?;
        Ok(fifo.display().to_string())
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn audio_pipe(&self, _mixer: &Mixer) -> Result<String> {
        bail!("Screen recording works on Windows and macOS.")
    }

    /// Ends the running part: `q` on FFmpeg's input, which finishes its file; killed if it does
    /// not end in time. Returns what the part holds.
    async fn end_segment(segment: &mut Segment, mixer: Option<&Mixer>) -> Progress {
        segment.asked.store(true, Ordering::SeqCst);
        if let Some(mut stdin) = segment.stdin.take() {
            let _ = stdin.write_all(b"q").await;
            let _ = stdin.flush().await;
        }
        if let Some(m) = mixer {
            m.detach();
        }
        let mut exited = segment.exited.clone();
        if tokio::time::timeout(STOP_TIMEOUT, exited.wait_for(|e| e.is_some()))
            .await
            .is_err()
        {
            tracing::warn!("FFmpeg did not end after q: stopped");
            segment.kill.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(5), exited.wait_for(|e| e.is_some()))
                .await;
        }
        // The picture's capture ends once FFmpeg has all it will take.
        if let Some(feed) = segment.feed.take() {
            let _ = tokio::task::spawn_blocking(move || drop(feed)).await;
        }
        let p = *segment.progress.lock();
        p
    }

    /// Pauses a recording (not a stream): its part ends; resuming starts the next.
    pub async fn pause(&self) -> Result<CaptureState> {
        let mut guard = self.session.lock().await;
        let session = guard
            .as_mut()
            .ok_or_else(|| anyhow!("Nothing is recording."))?;
        if session.url.is_some() {
            bail!("A stream cannot pause: stop it instead.");
        }
        if let Some(mut seg) = session.segment.take() {
            let done = Self::end_segment(&mut seg, session.mixer.as_ref()).await;
            session.before.seconds += done.seconds;
            session.before.bytes += done.bytes;
            if let Some(p) = seg.part {
                session.parts.push(p);
            }
        }
        let before = session.before;
        self.set(|s| {
            s.phase = Phase::Paused;
            s.seconds = before.seconds;
            s.bytes = before.bytes;
            s.fps = 0.0;
            s.kbps = 0.0;
        });
        Ok(self.state())
    }

    pub async fn resume(&self) -> Result<CaptureState> {
        let mut guard = self.session.lock().await;
        let session = guard
            .as_mut()
            .ok_or_else(|| anyhow!("Nothing is recording."))?;
        if session.segment.is_some() {
            return Ok(self.state());
        }
        if let Err(e) = self.start_segment(session).await {
            let why = format!("{e:#}");
            self.set(|s| s.error = Some(why));
            return Err(e);
        }
        self.set(|s| {
            s.phase = Phase::Live;
            s.error = None;
        });
        Ok(self.state())
    }

    /// Stops recording or streaming; a recording's parts become one MP4.
    pub async fn stop(&self) -> Result<CaptureState> {
        let session = self.session.lock().await.take();
        let Some(session) = session else {
            return Ok(self.state());
        };
        self.finish(session, None).await;
        Ok(self.state())
    }

    /// FFmpeg ended by itself: the window closed, the stream was cut, the disk filled. What was
    /// recorded is kept.
    async fn ended(&self, said: String) {
        let session = self.session.lock().await.take();
        if let Some(session) = session {
            let why = if said.trim().is_empty() {
                "The recording stopped by itself.".to_string()
            } else {
                format!("The recording stopped: {}", last_line(&said))
            };
            self.finish(session, Some(why)).await;
        }
    }

    async fn finish(&self, mut session: Session, why: Option<String>) {
        self.set(|s| s.phase = Phase::Finishing);
        if let Some(mut seg) = session.segment.take() {
            let done = Self::end_segment(&mut seg, session.mixer.as_ref()).await;
            session.before.seconds += done.seconds;
            session.before.bytes += done.bytes;
            if let Some(p) = seg.part {
                session.parts.push(p);
            }
        }
        if let Some(m) = session.mixer.take() {
            let _ = tokio::task::spawn_blocking(move || m.stop()).await;
        }
        let parts: Vec<PathBuf> = session
            .parts
            .iter()
            .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() > 0))
            .cloned()
            .collect();
        let mut error = why;
        let mut saved = None;
        if session.options.record && !parts.is_empty() {
            match join_parts(&session.ffmpeg, &parts, &session.folder, &session.base).await {
                Ok(path) => {
                    let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
                    self.saved.lock().push(path.clone());
                    saved = Some(Saved {
                        path: path.display().to_string(),
                        bytes,
                        seconds: session.before.seconds,
                    });
                }
                Err(e) => {
                    error = Some(format!(
                        "{e:#} The recording is in {}, in parts.",
                        session.folder.display()
                    ));
                }
            }
        }
        let seconds = session.before.seconds;
        self.set(|s| {
            *s = CaptureState {
                seconds,
                error,
                saved,
                ..CaptureState::default()
            }
        });
    }

    // ------------------------------------------------------------------ stream keys

    /// The stream key kept for `service` ("twitch", "youtube", "custom"), if one is.
    pub fn stream_key(&self, service: &str) -> Option<String> {
        let name = key_setting(service)?;
        let sealed = self.settings.get(&name).filter(|s| !s.trim().is_empty())?;
        secret::read(&name, &sealed)
            .map_err(|e| tracing::warn!("The kept stream key for {service} is unreadable: {e:#}"))
            .ok()
    }

    /// Keeps `key` for `service`, encrypted for this Windows account (in a Mac's keychain); None
    /// forgets it.
    pub fn keep_stream_key(&self, service: &str, key: Option<&str>) -> Result<()> {
        let name = key_setting(service).ok_or_else(|| anyhow!("No such streaming service."))?;
        match key.map(str::trim).filter(|k| !k.is_empty()) {
            Some(k) => self.settings.set(&name, secret::keep(&name, k)?),
            None => {
                secret::forget(&name);
                self.settings.set(&name, "")
            }
        }
    }

    // ------------------------------------------------------------------ FFmpeg's download

    pub fn install_state(&self) -> Option<Install> {
        self.install.lock().clone()
    }

    fn install_changed(&self) {
        events::emit(topic::CAPTURE, json!({ "install": self.install_state() }));
    }

    /// Downloads FFmpeg in the background, when it is not in.
    pub fn start_install(&self) -> Result<(), String> {
        if self.stopping.is_cancelled() {
            return Err("Nook is closing.".into());
        }
        if self.ffmpeg_path().is_some() || self.install_state().is_some_and(|i| i.error.is_none()) {
            return Ok(());
        }
        let Some(runtime) = self.runtime.clone() else {
            return Err("Nothing to download FFmpeg with.".into());
        };
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "Downloads need the app's async runtime.".to_string())?;
        let total = self.download_bytes();
        let cancel = self.stopping.child_token();
        *self.install_cancel.lock() = Some(cancel.clone());
        *self.install.lock() = Some(Install {
            what: "FFmpeg".into(),
            done: 0,
            total,
            error: None,
        });
        self.install_changed();
        let me = self.me.clone();
        handle.spawn(async move {
            let progress: StagedProgress = {
                let me = me.clone();
                Arc::new(move |_stage: &str, done, _of| {
                    let Some(me) = me.upgrade() else { return };
                    if let Some(i) = me.install.lock().as_mut().filter(|i| i.error.is_none()) {
                        i.done = done.min(i.total);
                    }
                    me.install_changed();
                })
            };
            let outcome = runtime
                .ensure_component(EngineComponent::Ffmpeg, Some(progress), &cancel)
                .await;
            let Some(me) = me.upgrade() else { return };
            let done = me.install_state().map_or(0, |i| i.done);
            *me.install.lock() = match outcome {
                Ok(true) => None,
                Ok(false) => Some(Install {
                    what: "FFmpeg".into(),
                    done,
                    total,
                    error: Some("The download was stopped.".into()),
                }),
                Err(e) => {
                    tracing::warn!("FFmpeg did not install: {e:#}");
                    Some(Install {
                        what: "FFmpeg".into(),
                        done,
                        total,
                        error: Some(format!("The download failed: {e:#}")),
                    })
                }
            };
            me.install_cancel.lock().take();
            me.install_changed();
        });
        Ok(())
    }

    pub fn cancel_install(&self) {
        if let Some(c) = self.install_cancel.lock().as_ref() {
            c.cancel();
        }
    }

    pub fn clear_install_error(&self) {
        let cleared = {
            let mut i = self.install.lock();
            let had = i.as_ref().is_some_and(|i| i.error.is_some());
            if had {
                *i = None;
            }
            had
        };
        if cleared {
            self.install_changed();
        }
    }

    /// Stops what records (keeping the file) and the meters, before Nook exits.
    pub async fn shutdown(&self) {
        self.stopping.cancel();
        self.stop_listening();
        let _ = tokio::time::timeout(Duration::from_secs(30), self.stop()).await;
    }
}

impl BusyWork for CaptureService {
    fn busy_with(&self) -> Option<String> {
        let state = self.state.lock().clone();
        if state.phase != Phase::Idle {
            return Some(if state.streaming {
                "a stream is live".to_string()
            } else {
                "a screen recording is running".to_string()
            });
        }
        self.install_state()
            .filter(|i| i.error.is_none())
            .map(|_| "FFmpeg is downloading".to_string())
    }
}

/// Why a part did not start: the encoder (the next may work), or anything else.
#[derive(Debug)]
enum Failed {
    Encoder(String),
    Other(String),
}

/// What FFmpeg's last words say about why it did not start.
fn classify(said: &str) -> Failed {
    let lower = said.to_lowercase();
    let line = if said.trim().is_empty() {
        "FFmpeg ended before it recorded anything.".to_string()
    } else {
        format!("FFmpeg could not start: {}", last_line(said))
    };
    let stream = [
        "rtmp",
        "connection",
        "server",
        "handshake",
        "i/o error",
        "tls",
    ];
    if stream.iter().any(|w| lower.contains(w)) {
        return Failed::Other(format!(
            "The streaming service did not take the stream: check its server address and stream key. ({})",
            last_line(said)
        ));
    }
    let source = ["gfxcapture", "window", "monitor", "capture"];
    let encoder = [
        "encoder", "nvenc", "amf", "qsv", "mfx", "cuda", "device", "mft", "openh264",
    ];
    if encoder.iter().any(|w| lower.contains(w)) || !source.iter().any(|w| lower.contains(w)) {
        Failed::Encoder(line)
    } else {
        Failed::Other(line)
    }
}

/// Whether one of FFmpeg's lines says a `tee` output (the stream) failed while the file goes on.
fn stream_failed(line: &str) -> bool {
    let l = line.to_lowercase();
    (l.contains("slave") || l.contains("tee")) && (l.contains("fail") || l.contains("error"))
}

fn last_line(said: &str) -> String {
    said.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn key_setting(service: &str) -> Option<String> {
    let ok = !service.is_empty()
        && service.len() <= 24
        && service
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    ok.then(|| format!("capture.streamKey.{service}"))
}

/// One block of FFmpeg's `-progress` output, read line by line.
#[derive(Clone, Debug, Default, PartialEq)]
struct ProgressBlock {
    seconds: f64,
    bytes: u64,
    fps: f64,
    kbps: f64,
    dropped: u64,
}

impl ProgressBlock {
    /// Takes in one `key=value` line; true when it ends a block.
    fn read(&mut self, line: &str) -> bool {
        let Some((key, value)) = line.trim().split_once('=') else {
            return false;
        };
        let value = value.trim();
        match key {
            "out_time_us" | "out_time_ms" => {
                // Both are in microseconds (FFmpeg's out_time_ms has always been).
                if let Ok(us) = value.parse::<i64>() {
                    self.seconds = us.max(0) as f64 / 1_000_000.0;
                }
            }
            "total_size" => self.bytes = value.parse().unwrap_or(self.bytes),
            "fps" => self.fps = value.parse().unwrap_or(self.fps),
            "bitrate" => {
                self.kbps = value
                    .trim_end_matches("kbits/s")
                    .trim()
                    .parse()
                    .unwrap_or(self.kbps)
            }
            "drop_frames" => self.dropped = value.parse().unwrap_or(self.dropped),
            "progress" => return true,
            _ => {}
        }
        false
    }
}

/// Joins `parts` into one MP4 in `folder` named after `base`, and removes them; one part that
/// cannot be made MP4 is kept as a Matroska file under that name.
async fn join_parts(
    ffmpeg: &Path,
    parts: &[PathBuf],
    folder: &Path,
    base: &str,
) -> Result<PathBuf> {
    let out = free_path(folder, base, "mp4");
    let list = folder.join(format!("{base} parts.txt"));
    let args = plan::finish_args(parts, &list, &out)?;
    let mut cmd = crate::process::command(ffmpeg);
    cmd.args(&args);
    let done = tokio::time::timeout(Duration::from_secs(600), cmd.output()).await;
    let _ = std::fs::remove_file(&list);
    match done {
        Ok(Ok(o)) if o.status.success() && out.is_file() => {
            for p in parts {
                let _ = std::fs::remove_file(p);
            }
            Ok(out)
        }
        other => {
            let said = match other {
                Ok(Ok(o)) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                Ok(Err(e)) => e.to_string(),
                Err(_) => "it took too long".into(),
            };
            let _ = std::fs::remove_file(&out);
            tracing::warn!("The recording's parts did not become an MP4: {said}");
            if let [only] = parts {
                let kept = free_path(folder, base, "mkv");
                std::fs::rename(only, &kept).context("Could not name the recording")?;
                return Ok(kept);
            }
            bail!(
                "The recording's parts could not be joined ({}).",
                last_line(&said)
            )
        }
    }
}

/// Where recordings go by default: the person's Videos folder, in "Nook".
pub fn default_folder() -> PathBuf {
    sources::videos_folder()
        .unwrap_or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .join("Videos")
        })
        .join("Nook")
}

/// Encodes a few black frames with `encoder`: whether it works on this computer.
async fn probe(ffmpeg: &Path, encoder: Encoder) -> bool {
    let mut cmd = crate::process::command(ffmpeg);
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=1280x720:r=30",
        "-frames:v",
        "5",
        "-pix_fmt",
        "yuv420p",
        "-c:v",
        encoder.codec(),
    ])
    .args(encoder.options(4_000, 60))
    .args(["-f", "null", "-"]);
    matches!(
        tokio::time::timeout(Duration::from_secs(20), cmd.output()).await,
        Ok(Ok(o)) if o.status.success()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_blocks_are_read() {
        let mut b = ProgressBlock::default();
        let lines = [
            "frame=150",
            "fps=30.02",
            "bitrate=4321.5kbits/s",
            "total_size=2703360",
            "out_time_us=5000000",
            "drop_frames=2",
            "speed=1.01x",
        ];
        for l in lines {
            assert!(!b.read(l));
        }
        assert!(b.read("progress=continue"));
        assert_eq!(
            b,
            ProgressBlock {
                seconds: 5.0,
                bytes: 2_703_360,
                fps: 30.02,
                kbps: 4321.5,
                dropped: 2,
            }
        );
        // "N/A" while nothing is out yet leaves what was known.
        assert!(!b.read("bitrate=N/A"));
        assert_eq!(b.kbps, 4321.5);
    }

    #[test]
    fn failures_are_told_apart() {
        assert!(matches!(
            classify("[h264_nvenc @ 0x1] OpenEncodeSessionEx failed: unsupported device (2)"),
            Failed::Encoder(_)
        ));
        assert!(matches!(
            classify("[rtmp @ 0x2] Server error: Invalid stream key"),
            Failed::Other(m) if m.contains("stream key")
        ));
        assert!(matches!(
            classify("[Parsed_gfxcapture_0 @ 0x3] Failed to capture window: the window was closed"),
            Failed::Other(_)
        ));
        assert!(stream_failed(
            "[tee @ 0x4] Slave muxer #1 failed: Broken pipe, continuing"
        ));
        assert!(!stream_failed("[matroska @ 0x5] Starting new cluster"));
    }

    #[test]
    fn stream_keys_are_kept_per_service_and_sealed() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Arc::new(Settings::load(dir.path().join("settings.json")).unwrap());
        let capture = CaptureService::with_ffmpeg(dir.path().join("none.exe"), settings.clone());
        assert_eq!(capture.stream_key("twitch"), None);
        if cfg!(windows) {
            capture
                .keep_stream_key("twitch", Some(" live_42_secret "))
                .unwrap();
            assert_eq!(
                capture.stream_key("twitch").as_deref(),
                Some("live_42_secret")
            );
            let file = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
            assert!(!file.contains("live_42_secret"), "kept sealed");
            assert_eq!(capture.stream_key("youtube"), None);
            capture.keep_stream_key("twitch", None).unwrap();
            assert_eq!(capture.stream_key("twitch"), None);
        }
        assert!(capture.keep_stream_key("../x", Some("k")).is_err());
    }

    /// What ffprobe says of a file's streams and length: (video codec, width, height, audio
    /// codec, seconds).
    fn probed(ffprobe: &Path, file: &Path) -> (String, u32, u32, String, f64) {
        let out = std::process::Command::new(ffprobe)
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type,codec_name,width,height:format=duration",
                "-of",
                "json",
            ])
            .arg(file)
            .output()
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let streams = v["streams"].as_array().cloned().unwrap_or_default();
        let of = |kind: &str| streams.iter().find(|s| s["codec_type"] == kind).cloned();
        let video = of("video").unwrap_or_default();
        let audio = of("audio").unwrap_or_default();
        (
            video["codec_name"].as_str().unwrap_or("").to_string(),
            video["width"].as_u64().unwrap_or(0) as u32,
            video["height"].as_u64().unwrap_or(0) as u32,
            audio["codec_name"].as_str().unwrap_or("").to_string(),
            v["format"]["duration"]
                .as_str()
                .and_then(|d| d.parse().ok())
                .unwrap_or(0.0),
        )
    }

    /// Records this computer's screen, a window and an area with its microphone and sound, and
    /// streams to an RTMP listener of FFmpeg's own on 127.0.0.1, all with the real FFmpeg:
    /// `NOOK_TEST_FFMPEG` names ffmpeg.exe (ffprobe.exe beside it), `NOOK_TEST_OUT` a folder:
    /// `cargo test -p nook-core records_and_streams_for_real -- --ignored --nocapture`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs FFmpeg, a screen, a microphone and speakers"]
    async fn records_and_streams_for_real() {
        let ffmpeg = PathBuf::from(std::env::var("NOOK_TEST_FFMPEG").expect("NOOK_TEST_FFMPEG"));
        let ffprobe = ffmpeg.with_file_name("ffprobe.exe");
        let out = PathBuf::from(std::env::var("NOOK_TEST_OUT").expect("NOOK_TEST_OUT"));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let settings = Arc::new(Settings::load(out.join("settings.json")).unwrap());
        let capture = CaptureService::with_ffmpeg(ffmpeg.clone(), settings);
        let sources = tokio::task::spawn_blocking({
            let c = capture.clone();
            move || c.sources()
        })
        .await
        .unwrap();
        assert!(sources.ready);
        let screen = sources.screens.iter().find(|s| s.primary).unwrap().clone();
        let sound = vec![
            AudioSource::Microphone { device: None },
            AudioSource::System { device: None },
        ];
        let options = |source: Source, audio: Vec<AudioSource>| StartOptions {
            source,
            fps: 30,
            scale_to: Some(720),
            quality: Quality::Standard,
            cursor: true,
            audio,
            record: true,
            folder: Some(out.display().to_string()),
            stream: None,
        };

        // The screen, with sound: three seconds, a pause, two more.
        let preview = capture
            .preview(&Source::Screen {
                handle: screen.handle,
            })
            .await
            .unwrap();
        assert_eq!(&preview[1..4], b"PNG");
        let started = capture
            .start(options(
                Source::Screen {
                    handle: screen.handle,
                },
                sound.clone(),
            ))
            .await
            .unwrap();
        println!(
            "recording with {:?} at {}x{}",
            started.encoder, started.width, started.height
        );
        assert_eq!(started.phase, Phase::Live);
        tokio::time::sleep(Duration::from_secs(3)).await;
        let paused = capture.pause().await.unwrap();
        assert_eq!(paused.phase, Phase::Paused);
        println!("paused at {:.1} s, {} bytes", paused.seconds, paused.bytes);
        tokio::time::sleep(Duration::from_secs(1)).await;
        capture.resume().await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        let done = capture.stop().await.unwrap();
        assert_eq!(done.phase, Phase::Idle, "{:?}", done.error);
        let saved = done.saved.expect("a recording");
        let file = PathBuf::from(&saved.path);
        assert_eq!(file.extension().unwrap(), "mp4");
        let (v, w, h, a, seconds) = probed(&ffprobe, &file);
        println!(
            "{}: {v} {w}x{h} {a}, {seconds:.2} s, {} bytes",
            saved.path, saved.bytes
        );
        assert_eq!((v.as_str(), h, a.as_str()), ("h264", 720, "aac"));
        assert!(
            (4.0..=6.5).contains(&seconds),
            "two parts, about five seconds: {seconds}"
        );
        assert!(capture.is_saved(&file));
        let leftovers: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".mkv") || n.ends_with(".txt"))
            .collect();
        assert!(leftovers.is_empty(), "parts removed: {leftovers:?}");

        // A stream and its recording at once, to a listener of FFmpeg's own.
        let received = out.join("received.flv");
        let mut listener = std::process::Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-listen",
                "1",
                "-i",
                "rtmp://127.0.0.1:19350/live/test-key-123",
                "-c",
                "copy",
            ])
            .arg(&received)
            .spawn()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(800)).await;
        let mut streamed = options(
            Source::Screen {
                handle: screen.handle,
            },
            vec![AudioSource::System { device: None }],
        );
        streamed.stream = Some(StreamOptions {
            server: "rtmp://127.0.0.1:19350/live".into(),
            key: "test-key-123".into(),
            kbps: 2_500,
        });
        let live = capture.start(streamed).await.unwrap();
        assert!(live.streaming && live.recording);
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(capture.pause().await.is_err(), "a stream does not pause");
        let done = capture.stop().await.unwrap();
        assert_eq!(done.error, None);
        let _ = tokio::task::spawn_blocking(move || {
            // The listener ends once the stream does; given ten seconds, then stopped.
            let started = std::time::Instant::now();
            while started.elapsed() < Duration::from_secs(10)
                && listener.try_wait().ok().flatten().is_none()
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = listener.kill();
            let _ = listener.wait();
        })
        .await;
        let (v, _, h, a, seconds) = probed(&ffprobe, &received);
        println!("received: {v} {h} lines {a}, {seconds:.2} s");
        assert_eq!((v.as_str(), a.as_str()), ("h264", "aac"));
        assert!(seconds > 2.0, "{seconds}");
        let also = PathBuf::from(done.saved.expect("recorded too").path);
        assert!(probed(&ffprobe, &also).4 > 2.0);

        // A window, without sound; then an area.
        if let Some(window) = sources.windows.first() {
            capture
                .start(options(
                    Source::Window {
                        handle: window.handle,
                    },
                    Vec::new(),
                ))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
            let done = capture.stop().await.unwrap();
            let (v, w, h, a, seconds) = probed(&ffprobe, Path::new(&done.saved.unwrap().path));
            println!(
                "window \"{}\": {v} {w}x{h} [{a}] {seconds:.2} s",
                window.app
            );
            assert!(v == "h264" && a.is_empty() && seconds > 1.0);
        }
        let area = Source::Area {
            screen: screen.handle,
            x: 100,
            y: 100,
            width: 641,
            height: 361,
        };
        capture.start(options(area, Vec::new())).await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        let done = capture.stop().await.unwrap();
        let (_, w, h, _, _) = probed(&ffprobe, Path::new(&done.saved.unwrap().path));
        assert_eq!((w, h), (640, 360), "the area, made even");
    }

    #[test]
    fn nothing_recording_is_not_busy() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Arc::new(Settings::load(dir.path().join("settings.json")).unwrap());
        let capture = CaptureService::with_ffmpeg(dir.path().join("none.exe"), settings);
        assert_eq!(capture.busy_with(), None);
        assert_eq!(capture.state().phase, Phase::Idle);
    }
}

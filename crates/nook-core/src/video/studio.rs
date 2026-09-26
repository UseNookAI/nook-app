//! Ports `video/VideoStudio.java`.
//!
//! The video tool: clips asked for with a prompt, made one at a time on the local runtime, and
//! kept in the videos folder as `<id>.avi` beside a `<id>.json` that remembers the prompt and
//! settings. Finished clips outlive the app; a clip that failed or was stopped is shown for the
//! rest of the session only.
//!
//! The original was a Spring service with a single-thread executor and `Runnable` listeners. Here
//! the queue is one tokio task fed by a channel (started with the first clip, so the studio can be
//! built outside the async runtime), and every change goes out on [`topic::VIDEO`] with the clips,
//! newest first, as the payload (the page re-reads `video_clips` either way).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::busy::BusyWork;
use crate::events::{self, topic};
use crate::runtime::manager::{RuntimeManager, VideoRuntime};
use crate::runtime::video_engine::{Stage, Stopped, VideoProgress};

/// How often, at most, progress inside one stage reaches the listeners.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// How long closing the app waits for a render in progress to stop.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(15);

/// Where a clip is (`VideoStudio.Status`). Serialized as the Java constant name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Status {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

/// One clip, asked for, being made or made (`VideoStudio.Clip`). The studio replaces it as it
/// moves on. The UI's `Clip` in `ui/src/api/video.ts` mirrors it: instants are epoch milliseconds,
/// `file` an absolute path.
///
/// - `stage`: what the engine is doing while RUNNING, else None
/// - `done`: units finished in the stage (sampling steps), with `total`; 0 when unknown
/// - `file`: the finished video while DONE, else None
/// - `started_at`: when the engine took the clip, None while it waits
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    pub id: String,
    pub prompt: String,
    pub model_id: Option<String>,
    pub status: Status,
    pub stage: Option<Stage>,
    pub done: u32,
    pub total: u32,
    pub file: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    pub fps: u32,
    pub seed: u64,
    pub elapsed_ms: u64,
    pub error: Option<String>,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub created_at: DateTime<Utc>,
    #[serde(with = "chrono::serde::ts_milliseconds_option")]
    pub started_at: Option<DateTime<Utc>>,
}

impl Clip {
    /// A clip that waits for its turn.
    fn queued(id: String, prompt: String, model_id: Option<String>) -> Clip {
        Clip {
            id,
            prompt,
            model_id,
            status: Status::Queued,
            stage: None,
            done: 0,
            total: 0,
            file: None,
            width: 0,
            height: 0,
            frames: 0,
            fps: 0,
            seed: 0,
            elapsed_ms: 0,
            error: None,
            created_at: Utc::now(),
            started_at: None,
        }
    }

    pub fn seconds(&self) -> f64 {
        if self.fps > 0 {
            self.frames as f64 / self.fps as f64
        } else {
            0.0
        }
    }

    pub fn finished(&self) -> bool {
        matches!(
            self.status,
            Status::Done | Status::Failed | Status::Cancelled
        )
    }

    /// Rough share of the work done, 0 to 1, for a progress bar. The shares are a Wan 2.1 clip
    /// on an RTX 4060 (2026-09-24): weights and prompt 27 s, 20 sampling steps 202 s, decoding
    /// the frames tile by tile 80 s.
    pub fn progress(&self) -> f64 {
        if self.status == Status::Done {
            return 1.0;
        }
        let Some(stage) = self.stage.filter(|_| self.status == Status::Running) else {
            return 0.0;
        };
        let in_stage = if self.total > 0 {
            (self.done as f64 / self.total as f64).min(1.0)
        } else {
            0.0
        };
        match stage {
            Stage::Loading => 0.09 * in_stage,
            Stage::Sampling => 0.09 + 0.65 * in_stage,
            Stage::Decoding => 0.74 + 0.25 * in_stage,
            Stage::Saving => 0.99,
        }
    }

    /// The same clip with another status and stage.
    pub fn with(&self, status: Status, stage: Option<Stage>, done: u32, total: u32) -> Clip {
        Clip {
            status,
            stage,
            done,
            total,
            ..self.clone()
        }
    }

    fn started(&self) -> Clip {
        Clip {
            status: Status::Running,
            stage: Some(Stage::Loading),
            done: 0,
            total: 0,
            started_at: Some(Utc::now()),
            ..self.clone()
        }
    }

    /// A clip that stopped (`VideoStudio.failed`), without its render details.
    fn stopped(&self, status: Status, error: Option<String>) -> Clip {
        Clip {
            status,
            stage: None,
            done: 0,
            total: 0,
            file: None,
            width: 0,
            height: 0,
            frames: 0,
            fps: 0,
            seed: 0,
            elapsed_ms: 0,
            error,
            ..self.clone()
        }
    }
}

/// Why a clip was not queued. The message is written for the person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitError {
    /// The prompt is empty (the original's `IllegalArgumentException`).
    EmptyPrompt,
    /// No video model or engine is installed, or the studio is closing (`IllegalStateException`).
    Unavailable(String),
}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubmitError::EmptyPrompt => f.write_str("Write what the video should show."),
            SubmitError::Unavailable(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for SubmitError {}

/// The clip queue. Build it once with [`VideoStudio::new`] (or [`VideoStudio::for_runtime`]) and
/// share the `Arc`; call [`VideoStudio::shutdown`] before exit.
pub struct VideoStudio {
    runtime: Arc<dyn VideoRuntime>,
    dir: PathBuf,
    clips: Mutex<HashMap<String, Clip>>,
    /// The original's `stopRequested`: a token per queued or running clip, cancelled to stop it.
    stops: Mutex<HashMap<String, CancellationToken>>,
    queue: mpsc::UnboundedSender<String>,
    receiver: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    stopping: CancellationToken,
    me: Weak<VideoStudio>,
}

impl VideoStudio {
    /// A studio on the app's runtime, keeping clips in the home's videos folder.
    pub fn for_runtime(runtime: Arc<RuntimeManager>) -> Arc<VideoStudio> {
        let dir = runtime.config().home.videos_dir();
        VideoStudio::new(runtime, dir)
    }

    /// A studio rendering through `runtime` into `dir`. Reads the clips finished in earlier
    /// sessions; starts nothing until the first clip is asked for.
    pub fn new(runtime: Arc<dyn VideoRuntime>, dir: PathBuf) -> Arc<VideoStudio> {
        let (queue, receiver) = mpsc::unbounded_channel();
        let clips = load_finished(&dir);
        Arc::new_cyclic(|me| VideoStudio {
            runtime,
            dir,
            clips: Mutex::new(clips),
            stops: Mutex::new(HashMap::new()),
            queue,
            receiver: Mutex::new(Some(receiver)),
            worker: Mutex::new(None),
            stopping: CancellationToken::new(),
            me: me.clone(),
        })
    }

    fn changed(&self) {
        events::emit(topic::VIDEO, self.clips());
    }

    /// Every clip, newest first.
    pub fn clips(&self) -> Vec<Clip> {
        let mut all: Vec<Clip> = self.clips.lock().values().cloned().collect();
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        all
    }

    pub fn clip(&self, id: &str) -> Option<Clip> {
        self.clips.lock().get(id).cloned()
    }

    /// None when a clip can be made now, otherwise why not, written for the person.
    pub fn problem(&self) -> Option<String> {
        self.runtime.video_problem()
    }

    /// Where finished clips are kept.
    pub fn folder(&self) -> &Path {
        &self.dir
    }

    /// Queues a clip. It starts when the clips before it are done. `model_id` None means the
    /// preferred installed video model.
    pub fn submit(&self, prompt: &str, model_id: Option<&str>) -> Result<Clip, SubmitError> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(SubmitError::EmptyPrompt);
        }
        if let Some(problem) = self.runtime.video_problem() {
            return Err(SubmitError::Unavailable(problem));
        }
        self.ensure_worker()?;
        let model = self
            .runtime
            .video_model(model_id)
            .map(|m| m.id)
            .or_else(|| model_id.map(str::to_string));
        let clip = Clip::queued(new_id(), prompt.to_string(), model);
        self.stops
            .lock()
            .insert(clip.id.clone(), CancellationToken::new());
        self.clips.lock().insert(clip.id.clone(), clip.clone());
        self.changed();
        if self.queue.send(clip.id.clone()).is_err() {
            tracing::warn!("The video queue is closed; clip {} will not start", clip.id);
        }
        Ok(clip)
    }

    /// Starts the queue's worker with the first clip.
    fn ensure_worker(&self) -> Result<(), SubmitError> {
        if self.stopping.is_cancelled() {
            return Err(SubmitError::Unavailable("Nook is closing.".into()));
        }
        let mut worker = self.worker.lock();
        if worker.is_some() {
            return Ok(());
        }
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| SubmitError::Unavailable("Clips need the app's async runtime.".into()))?;
        let Some(receiver) = self.receiver.lock().take() else {
            return Err(SubmitError::Unavailable(
                "The video queue is closed.".into(),
            ));
        };
        *worker = Some(handle.spawn(work(self.me.clone(), receiver, self.stopping.clone())));
        Ok(())
    }

    /// Stops a queued or running clip. A finished clip is left as it is.
    pub fn cancel(&self, id: &str) {
        match self.clip(id) {
            Some(c) if !c.finished() => {}
            _ => return,
        }
        self.stops
            .lock()
            .entry(id.to_string())
            .or_default()
            .cancel();
        // Atomic with the worker taking the clip: a queued clip is either cancelled here or
        // stopped there.
        let cancelled = {
            let mut clips = self.clips.lock();
            match clips.get_mut(id) {
                Some(now) if now.status == Status::Queued => {
                    *now = now.with(Status::Cancelled, None, 0, 0);
                    true
                }
                _ => false,
            }
        };
        if cancelled {
            self.changed();
        }
    }

    /// Removes a finished clip and its files. Returns false for an unknown or unfinished clip.
    pub fn delete(&self, id: &str) -> bool {
        {
            let mut clips = self.clips.lock();
            match clips.get(id) {
                Some(c) if c.finished() => {}
                _ => return false,
            }
            clips.remove(id);
        }
        for file in [
            self.dir.join(format!("{id}.avi")),
            self.dir.join(format!("{id}.json")),
        ] {
            if let Err(e) = std::fs::remove_file(&file) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("Could not delete clip {id}: {e}");
                }
            }
        }
        self.changed();
        true
    }

    /// Stops a render in progress when the app closes, so no engine process is left behind.
    /// Waits up to fifteen seconds for it to stop.
    pub async fn shutdown(&self) {
        self.stopping.cancel();
        {
            let clips = self.clips.lock();
            let mut stops = self.stops.lock();
            for c in clips.values().filter(|c| !c.finished()) {
                stops.entry(c.id.clone()).or_default().cancel();
            }
        }
        let worker = self.worker.lock().take();
        if let Some(mut worker) = worker {
            if tokio::time::timeout(SHUTDOWN_WAIT, &mut worker)
                .await
                .is_err()
            {
                worker.abort();
            }
        }
    }

    // ------------------------------------------------------------------ the worker

    async fn render(&self, id: &str) {
        let running = {
            let mut clips = self.clips.lock();
            match clips.get_mut(id) {
                Some(c) => {
                    if c.status == Status::Queued {
                        *c = c.started();
                    }
                    Some(c.clone())
                }
                None => None,
            }
        };
        let Some(running) = running.filter(|c| c.status == Status::Running) else {
            self.stops.lock().remove(id);
            return;
        };
        self.changed();
        let cancel = self.stops.lock().entry(id.to_string()).or_default().clone();
        let progress = self.progress_for(id);
        let out = self.dir.join(format!("{id}.avi"));
        let result = self
            .runtime
            .generate_video(
                running.model_id.as_deref(),
                &running.prompt,
                &out,
                Some(progress),
                &cancel,
            )
            .await
            .and_then(|r| {
                let req = &r.request;
                let done = Clip {
                    status: Status::Done,
                    stage: None,
                    done: 0,
                    total: 0,
                    file: Some(r.file.clone()),
                    width: req.width,
                    height: req.height,
                    frames: req.frames,
                    fps: req.fps,
                    seed: req.seed,
                    elapsed_ms: r.elapsed_ms,
                    error: None,
                    ..running.clone()
                };
                self.write_sidecar(&done)?;
                Ok(done)
            });
        let after = match result {
            Ok(done) => done,
            Err(e) if e.chain().any(|c| c.is::<Stopped>()) => {
                running.stopped(Status::Cancelled, None)
            }
            Err(e) => {
                tracing::warn!("Clip {id} failed: {e:#}");
                running.stopped(Status::Failed, Some(format!("{e:#}")))
            }
        };
        self.clips.lock().insert(id.to_string(), after);
        self.stops.lock().remove(id);
        self.changed();
    }

    /// The engine's progress, kept on the clip; the listeners hear of a new stage, the end of
    /// one, and otherwise at most every [`PROGRESS_INTERVAL`].
    fn progress_for(&self, id: &str) -> VideoProgress {
        let me = self.me.clone();
        let id = id.to_string();
        let last_report: Mutex<Option<Instant>> = Mutex::new(None);
        Arc::new(move |stage, done, total| {
            let Some(me) = me.upgrade() else { return };
            let new_stage = {
                let mut clips = me.clips.lock();
                let Some(now) = clips.get_mut(&id).filter(|c| c.status == Status::Running) else {
                    return;
                };
                let new_stage = now.stage != Some(stage);
                *now = now.with(Status::Running, Some(stage), done, total);
                new_stage
            };
            let due = {
                let mut last = last_report.lock();
                let due = new_stage
                    || done == total
                    || last.is_none_or(|t| t.elapsed() >= PROGRESS_INTERVAL);
                if due {
                    *last = Some(Instant::now());
                }
                due
            };
            if due {
                me.changed();
            }
        })
    }

    // ------------------------------------------------------------------ the folder

    fn write_sidecar(&self, c: &Clip) -> Result<()> {
        let sidecar = Sidecar {
            id: c.id.clone(),
            prompt: c.prompt.clone(),
            model: c.model_id.clone(),
            width: c.width,
            height: c.height,
            frames: c.frames,
            fps: c.fps,
            seed: c.seed,
            elapsed_ms: c.elapsed_ms,
            created_at: c.created_at.to_rfc3339_opts(SecondsFormat::AutoSi, true),
        };
        let file = self.dir.join(format!("{}.json", c.id));
        let json = serde_json::to_string_pretty(&sidecar)?;
        std::fs::create_dir_all(&self.dir)
            .and_then(|_| std::fs::write(&file, json))
            .with_context(|| format!("Could not write {}", file.display()))
    }
}

impl BusyWork for VideoStudio {
    fn busy_with(&self) -> Option<String> {
        self.clips
            .lock()
            .values()
            .any(|c| matches!(c.status, Status::Queued | Status::Running))
            .then(|| "a clip is rendering".to_string())
    }
}

/// The queue: one clip at a time, in the order they were asked for, until the studio closes or
/// is dropped.
async fn work(
    me: Weak<VideoStudio>,
    mut queue: mpsc::UnboundedReceiver<String>,
    stopping: CancellationToken,
) {
    loop {
        let id = tokio::select! {
            biased;
            _ = stopping.cancelled() => break,
            id = queue.recv() => match id {
                Some(id) => id,
                None => break,
            },
        };
        let Some(studio) = me.upgrade() else { break };
        studio.render(&id).await;
    }
}

/// What `<id>.json` remembers, in the original's field order.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Sidecar {
    id: String,
    prompt: String,
    model: Option<String>,
    width: u32,
    height: u32,
    frames: u32,
    fps: u32,
    seed: u64,
    elapsed_ms: u64,
    created_at: String,
}

/// Clips finished in earlier sessions: every AVI with a readable sidecar.
fn load_finished(dir: &Path) -> HashMap<String, Clip> {
    let mut clips = HashMap::new();
    if !dir.is_dir() {
        return clips;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("Cannot read the videos folder {}: {e}", dir.display());
            return clips;
        }
    };
    for entry in entries.flatten() {
        let json = entry.path();
        if !json.to_string_lossy().ends_with(".json") {
            continue;
        }
        match read_sidecar(dir, &json) {
            Ok(Some(clip)) => {
                clips.insert(clip.id.clone(), clip);
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(
                "Skipping unreadable clip {}: {e:#}",
                json.file_name().unwrap_or_default().to_string_lossy()
            ),
        }
    }
    clips
}

/// One finished clip from its sidecar, read as leniently as Jackson's `path(..).asText/asInt`
/// did; None when the sidecar names no clip or its AVI is gone.
fn read_sidecar(dir: &Path, json: &Path) -> Result<Option<Clip>> {
    let n: Value = serde_json::from_slice(&std::fs::read(json)?)?;
    let Some(id) = text(n.get("id")) else {
        return Ok(None);
    };
    // The id names the files: it must be a plain name inside the folder.
    if id.is_empty() || id.contains(['/', '\\', ':']) || id.contains("..") {
        return Ok(None);
    }
    let avi = dir.join(format!("{id}.avi"));
    if !avi.is_file() {
        return Ok(None);
    }
    let created_at = match text(n.get("createdAt")) {
        Some(t) => DateTime::parse_from_rfc3339(&t)
            .with_context(|| format!("bad createdAt {t}"))?
            .with_timezone(&Utc),
        None => DateTime::<Utc>::UNIX_EPOCH,
    };
    let int = |key: &str| number(n.get(key)).clamp(0, u32::MAX as i64) as u32;
    let long = |key: &str| number(n.get(key)).max(0) as u64;
    Ok(Some(Clip {
        id: id.clone(),
        prompt: text(n.get("prompt")).unwrap_or_default(),
        model_id: text(n.get("model")),
        status: Status::Done,
        stage: None,
        done: 0,
        total: 0,
        file: Some(avi),
        width: int("width"),
        height: int("height"),
        frames: int("frames"),
        fps: int("fps"),
        seed: long("seed"),
        elapsed_ms: long("elapsedMs"),
        error: None,
        created_at,
        started_at: None,
    }))
}

/// Jackson's `asText(null)`: a value's text, None for a missing value or JSON null, "" for an
/// object or array.
pub(crate) fn text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(_) | Value::Object(_) => Some(String::new()),
    }
}

/// Jackson's `asLong()`: a number, or text that reads as one; 0 otherwise.
fn number(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0),
        Some(Value::Bool(true)) => 1,
        _ => 0,
    }
}

/// Sortable by time and unique: the creation instant, then a short random tail.
fn new_id() -> String {
    let millis = Utc::now().timestamp_millis().max(0) as u64;
    let tail = uuid::Uuid::new_v4().simple().to_string();
    format!("vid_{}{}", base36(millis), &tail[..6])
}

fn base36(mut n: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// A fake runtime for the studio's and the video routes' tests.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::runtime::model_registry::LocalModel;
    use crate::runtime::video_engine::{VideoRequest, VideoResult};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// What the fake engine does with a clip.
    #[derive(Clone, Debug)]
    pub(crate) enum Mode {
        /// Reports a sampling step, writes the file, answers (seed 7, 90 s).
        Renders,
        /// Waits for [`FakeRuntime::release`] (up to five seconds), then renders.
        WaitsForRelease,
        /// Runs until stopped, then fails as a stopped clip does.
        RunsUntilStopped,
        /// Reports sampling step 5 of 20, then runs until stopped.
        SamplesUntilStopped,
        /// Fails with the engine's reason.
        Fails(String),
    }

    pub(crate) struct FakeRuntime {
        pub problem: Mutex<Option<String>>,
        pub mode: Mutex<Mode>,
        pub model: Mutex<Option<LocalModel>>,
        pub calls: AtomicUsize,
        pub release: tokio::sync::Semaphore,
    }

    impl FakeRuntime {
        pub(crate) fn new(mode: Mode) -> Arc<FakeRuntime> {
            Arc::new(FakeRuntime {
                problem: Mutex::new(None),
                mode: Mutex::new(mode),
                model: Mutex::new(None),
                calls: AtomicUsize::new(0),
                release: tokio::sync::Semaphore::new(0),
            })
        }

        pub(crate) fn release(&self) {
            self.release.add_permits(1);
        }

        pub(crate) fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    fn rendered(prompt: &str, out: &Path, seed: u64, elapsed_ms: u64) -> Result<VideoResult> {
        std::fs::create_dir_all(out.parent().unwrap())?;
        std::fs::write(out, [1u8, 2, 3])?;
        Ok(VideoResult {
            file: out.to_path_buf(),
            request: VideoRequest {
                prompt: prompt.to_string(),
                negative_prompt: String::new(),
                width: 832,
                height: 480,
                frames: 33,
                fps: 16,
                steps: 20,
                cfg_scale: 6.0,
                flow_shift: 3.0,
                seed,
                sampler: "euler".into(),
                t5xxl: None,
                vae: None,
                flags: String::new(),
            },
            elapsed_ms,
        })
    }

    #[async_trait]
    impl VideoRuntime for FakeRuntime {
        fn video_problem(&self) -> Option<String> {
            self.problem.lock().clone()
        }

        fn video_model(&self, _model_id: Option<&str>) -> Option<LocalModel> {
            self.model.lock().clone()
        }

        async fn generate_video(
            &self,
            _model_id: Option<&str>,
            prompt: &str,
            out: &Path,
            progress: Option<VideoProgress>,
            cancel: &CancellationToken,
        ) -> Result<VideoResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mode = self.mode.lock().clone();
            match mode {
                Mode::Renders => {
                    if let Some(p) = &progress {
                        p(Stage::Sampling, 10, 20);
                    }
                    rendered(prompt, out, 7, 90_000)
                }
                Mode::WaitsForRelease => {
                    let waited =
                        tokio::time::timeout(Duration::from_secs(5), self.release.acquire()).await;
                    if let Ok(Ok(permit)) = waited {
                        permit.forget();
                    }
                    rendered(prompt, out, 1, 1_000)
                }
                Mode::RunsUntilStopped => {
                    let _ = tokio::time::timeout(Duration::from_secs(5), cancel.cancelled()).await;
                    Err(anyhow::Error::new(Stopped))
                }
                Mode::SamplesUntilStopped => {
                    if let Some(p) = &progress {
                        p(Stage::Sampling, 5, 20);
                    }
                    let _ = tokio::time::timeout(Duration::from_secs(10), cancel.cancelled()).await;
                    Err(anyhow::Error::new(Stopped))
                }
                Mode::Fails(why) => Err(anyhow::anyhow!(why)),
            }
        }
    }

    /// Waits up to ten seconds for a condition.
    pub(crate) async fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(10);
        while !ok() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn studio(runtime: &Arc<FakeRuntime>, dir: &Path) -> Arc<VideoStudio> {
        VideoStudio::new(runtime.clone(), dir.join("videos"))
    }

    fn status_of(studio: &VideoStudio, id: &str) -> Status {
        studio.clip(id).unwrap().status
    }

    #[tokio::test]
    async fn a_clip_runs_to_the_end_and_outlives_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = FakeRuntime::new(Mode::Renders);
        let studio = studio(&runtime, dir.path());
        let queued = studio.submit("  a fox in the snow ", None).unwrap();
        assert_eq!(queued.prompt, "a fox in the snow");
        assert_eq!(queued.status, Status::Queued);
        wait_until("the clip to finish", || {
            status_of(&studio, &queued.id) == Status::Done
        })
        .await;

        let done = studio.clip(&queued.id).unwrap();
        assert!(done.file.as_ref().unwrap().is_file());
        assert_eq!(done.width, 832);
        assert_eq!(done.seed, 7);
        assert_eq!(done.elapsed_ms, 90_000);
        assert_eq!(done.progress(), 1.0);
        assert!(done.started_at.is_some());
        let sidecar = studio.folder().join(format!("{}.json", queued.id));
        assert!(sidecar.is_file());
        let json: Value =
            serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
        assert_eq!(json["prompt"], "a fox in the snow");
        assert_eq!(json["elapsedMs"], 90_000);
        assert!(json["model"].is_null());

        let reopened = VideoStudio::new(runtime.clone(), studio.folder().to_path_buf());
        let again = reopened.clip(&queued.id).unwrap();
        assert_eq!(again.status, Status::Done);
        assert_eq!(again.prompt, "a fox in the snow");
        assert_eq!(again.frames, 33);
        assert_eq!(again.file, done.file);
        assert_eq!(again.created_at, done.created_at);
    }

    #[tokio::test]
    async fn a_queued_clip_can_be_cancelled_before_it_starts() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = FakeRuntime::new(Mode::WaitsForRelease);
        let studio = studio(&runtime, dir.path());
        let first = studio.submit("first", None).unwrap();
        let second = studio.submit("second", None).unwrap();
        wait_until("the first clip to start", || {
            status_of(&studio, &first.id) == Status::Running
        })
        .await;

        studio.cancel(&second.id);
        assert_eq!(status_of(&studio, &second.id), Status::Cancelled);
        runtime.release();
        wait_until("the first clip to finish", || {
            status_of(&studio, &first.id) == Status::Done
        })
        .await;
        // The worker has passed over the cancelled clip once the queue is idle.
        wait_until("the queue to settle", || studio.busy_with().is_none()).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(runtime.calls(), 1);
        assert_eq!(status_of(&studio, &second.id), Status::Cancelled);
        assert_eq!(
            studio
                .clips()
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            vec![second.id.clone(), first.id.clone()],
            "newest first"
        );
    }

    #[tokio::test]
    async fn the_studio_is_busy_while_a_clip_is_queued_or_rendering() {
        // the dev channel waits for this before its installer closes the app (VersionUpdateService)
        let dir = tempfile::tempdir().unwrap();
        let runtime = FakeRuntime::new(Mode::WaitsForRelease);
        let studio = studio(&runtime, dir.path());
        assert_eq!(studio.busy_with(), None);
        let clip = studio.submit("a slow one", None).unwrap();
        assert_eq!(studio.busy_with().as_deref(), Some("a clip is rendering"));
        wait_until("the clip to start", || {
            status_of(&studio, &clip.id) == Status::Running
        })
        .await;
        assert_eq!(studio.busy_with().as_deref(), Some("a clip is rendering"));

        runtime.release();
        wait_until("the clip to finish", || {
            status_of(&studio, &clip.id) == Status::Done
        })
        .await;
        assert_eq!(studio.busy_with(), None);
    }

    #[tokio::test]
    async fn a_running_clip_stops_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = FakeRuntime::new(Mode::RunsUntilStopped);
        let studio = studio(&runtime, dir.path());
        let clip = studio.submit("a long one", None).unwrap();
        wait_until("the clip to start", || {
            status_of(&studio, &clip.id) == Status::Running
        })
        .await;
        assert!(!studio.delete(&clip.id), "a running clip cannot be deleted");

        studio.cancel(&clip.id);
        wait_until("the clip to stop", || {
            status_of(&studio, &clip.id) == Status::Cancelled
        })
        .await;
        let stopped = studio.clip(&clip.id).unwrap();
        assert!(stopped.started_at.is_some());
        assert_eq!(stopped.error, None);
    }

    #[tokio::test]
    async fn a_failure_keeps_the_engines_reason() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = FakeRuntime::new(Mode::Fails("Video engine failed (exit 1).".into()));
        let studio = studio(&runtime, dir.path());
        let clip = studio.submit("anything", None).unwrap();
        wait_until("the clip to fail", || {
            status_of(&studio, &clip.id) == Status::Failed
        })
        .await;
        assert_eq!(
            studio.clip(&clip.id).unwrap().error.as_deref(),
            Some("Video engine failed (exit 1).")
        );

        assert!(studio.delete(&clip.id));
        assert!(studio.clip(&clip.id).is_none());
        assert!(
            VideoStudio::new(runtime.clone(), studio.folder().to_path_buf())
                .clips()
                .is_empty(),
            "failed clips are not kept"
        );
    }

    #[tokio::test]
    async fn deleting_a_finished_clip_removes_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = FakeRuntime::new(Mode::Renders);
        let studio = studio(&runtime, dir.path());
        let clip = studio.submit("gone soon", None).unwrap();
        wait_until("the clip to finish", || {
            status_of(&studio, &clip.id) == Status::Done
        })
        .await;
        let file = studio.clip(&clip.id).unwrap().file.unwrap();

        assert!(studio.delete(&clip.id));
        assert!(!file.exists());
        assert!(!studio.folder().join(format!("{}.json", clip.id)).exists());
        assert!(!studio.delete(&clip.id), "an unknown clip");
    }

    #[tokio::test]
    async fn an_empty_prompt_or_a_missing_model_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = FakeRuntime::new(Mode::Renders);
        let studio = studio(&runtime, dir.path());
        let empty = studio.submit("   ", None).unwrap_err();
        assert_eq!(empty, SubmitError::EmptyPrompt);
        assert_eq!(empty.to_string(), "Write what the video should show.");
        *runtime.problem.lock() = Some("No video model is downloaded yet.".into());
        let e = studio.submit("a cat", None).unwrap_err();
        assert_eq!(e.to_string(), "No video model is downloaded yet.");
        assert_eq!(
            studio.problem().as_deref(),
            Some("No video model is downloaded yet.")
        );
        assert!(studio.clips().is_empty());
    }

    #[test]
    fn progress_rises_through_the_stages() {
        let base = Clip::queued("v".into(), "p".into(), None).started();
        let loading = base.progress();
        let halfway = base
            .with(Status::Running, Some(Stage::Sampling), 10, 20)
            .progress();
        let decoding = base
            .with(Status::Running, Some(Stage::Decoding), 0, 0)
            .progress();
        let saving = base
            .with(Status::Running, Some(Stage::Saving), 0, 0)
            .progress();
        assert!(loading < halfway && halfway < decoding && decoding < saving && saving < 1.0);
        assert_eq!(base.with(Status::Queued, None, 0, 0).progress(), 0.0);
    }

    /// The shape `ui/src/api/video.ts` reads: camelCase, the Java constant names, epoch ms.
    #[test]
    fn a_clip_serializes_as_the_page_reads_it() {
        let mut clip = Clip::queued("vid_1".into(), "a fox".into(), Some("wan".into()));
        clip.created_at = DateTime::from_timestamp_millis(1_790_000_000_123).unwrap();
        let running = clip
            .started()
            .with(Status::Running, Some(Stage::Sampling), 5, 20);
        let json = serde_json::to_value(&running).unwrap();
        assert_eq!(json["status"], "RUNNING");
        assert_eq!(json["stage"], "SAMPLING");
        assert_eq!(json["modelId"], "wan");
        assert_eq!(json["createdAt"], 1_790_000_000_123i64);
        assert!(json["startedAt"].is_i64());
        assert!(json["file"].is_null() && json["error"].is_null());
        for key in [
            "done",
            "total",
            "width",
            "height",
            "frames",
            "fps",
            "seed",
            "elapsedMs",
        ] {
            assert!(json[key].is_number(), "{key}");
        }
        let queued = serde_json::to_value(&clip).unwrap();
        assert_eq!(queued["status"], "QUEUED");
        assert!(queued["stage"].is_null() && queued["startedAt"].is_null());
        let back: Clip = serde_json::from_value(json).unwrap();
        assert_eq!(back.stage, Some(Stage::Sampling));
    }

    #[tokio::test]
    async fn every_change_is_an_event_and_the_model_is_the_installed_one() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = FakeRuntime::new(Mode::Renders);
        let wan = dir.path().join("Wan2.1-T2V-1.3B-Q8_0.gguf");
        *runtime.model.lock() = Some(LocalModelFixture::video("wan2.1-t2v-1.3b", &wan));
        let studio = studio(&runtime, dir.path());
        let mut events = events::subscribe();
        let clip = studio.submit("a fox", None).unwrap();
        assert_eq!(clip.model_id.as_deref(), Some("wan2.1-t2v-1.3b"));
        wait_until("the clip to finish", || {
            status_of(&studio, &clip.id) == Status::Done
        })
        .await;
        let mut seen = Vec::new();
        while let Ok(e) = events.try_recv() {
            if e.topic != topic::VIDEO {
                continue;
            }
            // Other tests' studios share the bus: keep this clip's states only.
            if let Some(c) = e.payload.as_array().and_then(|clips| {
                clips
                    .iter()
                    .find(|c| c["id"] == Value::String(clip.id.clone()))
            }) {
                seen.push(c["status"].as_str().unwrap_or_default().to_string());
            }
        }
        assert_eq!(seen.first().map(String::as_str), Some("QUEUED"));
        assert!(seen.iter().any(|s| s == "RUNNING"), "{seen:?}");
        assert_eq!(seen.last().map(String::as_str), Some("DONE"));
    }

    #[test]
    fn ids_sort_by_time() {
        let id = new_id();
        assert!(id.starts_with("vid_") && id.len() > 10, "{id}");
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
        assert!(base36(1_790_000_000_000) < base36(1_790_000_000_001));
    }

    #[test]
    fn a_sidecar_that_names_no_clip_or_leaves_the_folder_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let videos = dir.path();
        std::fs::write(videos.join("vid_a.avi"), b"RIFF").unwrap();
        std::fs::write(
            videos.join("vid_a.json"),
            r#"{"id":"vid_a","prompt":"a fox","model":"wan","width":"832","height":480,"frames":33,"fps":16,"seed":42,"elapsedMs":95000,"createdAt":"2026-09-24T10:15:30.123456Z"}"#,
        )
        .unwrap();
        std::fs::write(videos.join("vid_b.json"), r#"{"id":"vid_b"}"#).unwrap();
        std::fs::write(videos.join("escape.json"), r#"{"id":"../vid_a"}"#).unwrap();
        std::fs::write(videos.join("broken.json"), "{").unwrap();
        let clips = load_finished(videos);
        assert_eq!(clips.len(), 1);
        let a = &clips["vid_a"];
        assert_eq!((a.width, a.height, a.seed), (832, 480, 42));
        assert_eq!(a.created_at.timestamp_millis(), 1_790_244_930_123);
        assert_eq!(
            a.created_at.to_rfc3339_opts(SecondsFormat::AutoSi, true),
            "2026-09-24T10:15:30.123456Z",
            "written back as Java's Instant.toString"
        );
    }

    /// The studio on the real runtime with the fake sd engine: a clip renders into the videos
    /// folder, and closing the app stops one in progress.
    #[cfg(windows)]
    #[tokio::test]
    async fn clips_render_on_the_runtime_and_stop_when_the_app_closes() {
        use crate::runtime::engine_component::EngineComponent;
        use crate::runtime::manager::testing::{install, rig, sidecar_model, RigSpec};
        let rig = rig(RigSpec::default());
        install(&rig, EngineComponent::Sd);
        sidecar_model(
            &rig,
            "wan",
            "Wan2.1-T2V-1.3B-Q8_0.gguf",
            "wan2.1-t2v-1.3b",
            "video",
        );
        let studio = VideoStudio::for_runtime(rig.manager.clone());
        assert_eq!(studio.folder(), rig.home.videos_dir());

        let clip = studio.submit("a fox", None).unwrap();
        assert_eq!(clip.model_id.as_deref(), Some("wan2.1-t2v-1.3b"));
        wait_until("the clip to render", || {
            studio.clip(&clip.id).unwrap().finished()
        })
        .await;
        let done = studio.clip(&clip.id).unwrap();
        assert_eq!(done.status, Status::Done, "{:?}", done.error);
        assert_eq!(
            done.file,
            Some(rig.home.videos_dir().join(format!("{}.avi", clip.id)))
        );
        assert_eq!(
            (done.width, done.height, done.frames, done.fps),
            (832, 480, 33, 16)
        );

        let slow = studio.submit("slow", None).unwrap();
        wait_until("the slow clip to start", || {
            studio.clip(&slow.id).unwrap().status == Status::Running && rig.manager.is_video_busy()
        })
        .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        tokio::time::timeout(Duration::from_secs(20), studio.shutdown())
            .await
            .unwrap();
        assert_eq!(studio.clip(&slow.id).unwrap().status, Status::Cancelled);
        assert!(!rig.manager.is_video_busy());
        assert!(matches!(
            studio.submit("after", None),
            Err(SubmitError::Unavailable(_))
        ));
        rig.manager.shutdown().await;
    }

    /// Minimal installed models for the fakes.
    pub(crate) struct LocalModelFixture;

    impl LocalModelFixture {
        pub(crate) fn video(id: &str, file: &Path) -> crate::runtime::LocalModel {
            crate::runtime::LocalModel {
                id: id.to_string(),
                display_name: id.to_string(),
                family: "wan".into(),
                task: "video".into(),
                file: file.to_path_buf(),
                bytes: 1,
                sha256: None,
                source: "catalog".into(),
                downloaded_at: None,
                metadata: None,
                shared: false,
                unsupported: None,
            }
        }
    }
}

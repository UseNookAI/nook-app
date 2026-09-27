//! Ports `flow/FlowService.java`: the runs of the flows and the downloads they need.
//!
//! One run at a time, in the order they were asked for, each through its stages (preparing,
//! listening, translating, speaking, putting the track together, saving) with what it is doing
//! kept on the run. Finished runs are kept under `<home>\flows\<id>` with a `run.json` and come
//! back after a restart; a failed or stopped run is shown for the session only.
//!
//! The original was a Spring service with a single-thread executor and `Runnable` listeners. Here,
//! as in the video studio, the queue is one tokio task fed by a channel (started with the first
//! run), and changes go out on [`topic::FLOWS`]. New in this port: a run can start from the
//! microphone ([`FlowService::submit_recording`]); its recording is kept in the run's folder and
//! the translation is laid out line after line ([`Layout::Compact`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::audio::{self, CHUNK_SECONDS};
use super::dub::{self, Layout};
use super::languages;
use super::reader::Reader;
use super::reference;
use super::runtime::{Facts, FlowRuntime};
use super::subtitles::{self, Segment};
use super::summarize::Length;
use super::translator::{self, Chat};
use super::voice_engine::{AudioEngine, Line, Request, Speaker};
use super::voices::{Choice, Voice, Voices};
use super::Stopped;
use crate::busy::BusyWork;
use crate::events::{self, topic};
use crate::runtime::{Downloader, EngineComponent, Outcome, Progress, RuntimeManager};

/// The flow that translates speech.
pub const TRANSLATE_AUDIO: &str = "translate-audio";
/// The Nooklet that writes down a recording.
pub const TRANSCRIBE: &str = "transcribe";
/// The Nooklet that summarizes a document.
pub const SUMMARIZE: &str = "summarize";
/// The Nooklet that reads a document aloud.
pub const READ_ALOUD: &str = "read-aloud";
/// How long closing the app waits for a run in progress to stop.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(15);

/// Where a run is (`FlowService.Status`). Serialized as the Java constant name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Status {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

/// What a running translation is doing (`FlowService.Stage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Stage {
    Preparing,
    /// Reading a document's words (Summarize, Read aloud).
    Reading,
    Listening,
    Translating,
    /// The chat model writing a summary or notes.
    Summarizing,
    Speaking,
    Assembling,
    Saving,
}

/// Where a run's sound came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Source {
    /// A file the person dropped or chose.
    #[default]
    File,
    /// What the person said into the microphone; the recording is in the run's folder.
    Microphone,
    /// Text the person pasted; it is kept in the run's folder as `text.txt`.
    Text,
}

/// One run of a flow (`FlowService.Run`). The service replaces it as it moves on. The UI's `Run`
/// in `ui/src/api/flows.ts` mirrors it: instants are epoch milliseconds, paths absolute.
///
/// - `input`: the file the run works on; `input_name` what the card calls it
/// - `source_language`: the spoken language's code, or None to detect it
/// - `target_language`: the code of the language to translate into
/// - `model_id`: the chat model that translated, once known
/// - `keep_voice`: whether the person asked for the speaker's own voice
/// - `voice_name`, `cloned`: the voice that spoke, and whether in the speaker's own voice
/// - `note`: what the person should know: why no cloning, or why no sound
/// - `done`, `total`: units finished in the stage (parts listened to, lines translated or spoken)
/// - `detected_language`: what Whisper heard, as a code (or its name when Nook does not list it)
/// - `segments`: what was said, with the translation once made
/// - `audio`: the translated track, once made; `video`: the video with it, for a video input
///
/// The Nooklets on the same queue use these too, and their own:
///
/// - `flow`: [`TRANSLATE_AUDIO`], [`TRANSCRIBE`], [`SUMMARIZE`] or [`READ_ALOUD`]
/// - Transcribe: `source_language` the spoken language (None: Whisper tells), `notes` whether
///   the chat model writes notes (in `summary`), `length` theirs; `target_language` is empty
/// - Summarize: `target_language` the summary's language (empty: the document's), `length`,
///   `focus`, and `summary`
/// - Read aloud: `target_language` the text's language, `female` a woman's voice, `audio` the
///   track, `segments` each line with where it is heard in the track
/// - `words`: how many words were read or heard; `files` every file the run wrote
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Run {
    pub id: String,
    pub flow: String,
    pub input: String,
    pub input_name: String,
    pub source: Source,
    pub source_language: Option<String>,
    pub target_language: String,
    pub model_id: Option<String>,
    pub keep_voice: bool,
    pub voice_name: Option<String>,
    pub cloned: bool,
    pub note: Option<String>,
    pub status: Status,
    pub stage: Option<Stage>,
    pub done: u32,
    pub total: u32,
    pub detected_language: Option<String>,
    pub duration_seconds: f64,
    pub segments: Vec<Segment>,
    pub audio: Option<String>,
    pub video: Option<String>,
    pub elapsed_ms: u64,
    pub error: Option<String>,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub created_at: DateTime<Utc>,
    #[serde(with = "chrono::serde::ts_milliseconds_option")]
    pub started_at: Option<DateTime<Utc>>,
    pub notes: bool,
    pub length: Length,
    pub focus: Option<String>,
    pub female: bool,
    pub summary: Option<String>,
    pub words: u32,
    pub files: Vec<String>,
}

/// What the person asks of a Nooklet on the flows' queue (Transcribe, Summarize, Read aloud).
///
/// - `flow`: [`TRANSCRIBE`], [`SUMMARIZE`] or [`READ_ALOUD`]
/// - `language`: Transcribe, the spoken language (None: Whisper tells); Summarize, the language
///   to write in (None: the document's); Read aloud, the text's language
/// - `notes`: Transcribe, also write notes with the chat model
/// - `length`, `focus`: the summary's or the notes' length, and what to look at most
/// - `female`: Read aloud, a woman's voice (else a man's) where the voice has both
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Order {
    pub flow: String,
    pub language: Option<String>,
    pub notes: bool,
    pub length: Length,
    pub focus: Option<String>,
    pub female: bool,
}

impl Default for Run {
    fn default() -> Run {
        Run {
            id: String::new(),
            flow: TRANSLATE_AUDIO.into(),
            input: String::new(),
            input_name: String::new(),
            source: Source::File,
            source_language: None,
            target_language: String::new(),
            model_id: None,
            keep_voice: true,
            voice_name: None,
            cloned: false,
            note: None,
            status: Status::Done,
            stage: None,
            done: 0,
            total: 0,
            detected_language: None,
            duration_seconds: 0.0,
            segments: Vec::new(),
            audio: None,
            video: None,
            elapsed_ms: 0,
            error: None,
            created_at: DateTime::<Utc>::UNIX_EPOCH,
            started_at: None,
            notes: false,
            length: Length::Short,
            focus: None,
            female: true,
            summary: None,
            words: 0,
            files: Vec::new(),
        }
    }
}

impl Run {
    pub fn finished(&self) -> bool {
        matches!(
            self.status,
            Status::Done | Status::Failed | Status::Cancelled
        )
    }

    /// Rough share of the work done, 0 to 1: converting is quick, listening about a quarter of
    /// the rest on a GPU, translating another, speaking the other half.
    pub fn progress(&self) -> f64 {
        if self.status == Status::Done {
            return 1.0;
        }
        let Some(stage) = self.stage.filter(|_| self.status == Status::Running) else {
            return 0.0;
        };
        let within = if self.total > 0 {
            (self.done as f64 / self.total as f64).min(1.0)
        } else {
            0.0
        };
        match (self.flow.as_str(), stage) {
            // Written down, then the notes when asked for.
            (TRANSCRIBE, Stage::Listening) if self.notes => 0.05 + 0.65 * within,
            (TRANSCRIBE, Stage::Listening) => 0.05 + 0.90 * within,
            (TRANSCRIBE, Stage::Summarizing) => 0.70 + 0.27 * within,
            (SUMMARIZE, Stage::Reading) => 0.05,
            (SUMMARIZE, Stage::Summarizing) => 0.10 + 0.87 * within,
            (READ_ALOUD, Stage::Reading) => 0.03,
            (READ_ALOUD, Stage::Speaking) => 0.06 + 0.86 * within,
            (READ_ALOUD, Stage::Assembling) => 0.94,
            (_, Stage::Preparing) => 0.03,
            (_, Stage::Reading) => 0.05,
            (_, Stage::Listening) => 0.05 + 0.25 * within,
            (_, Stage::Translating) => 0.30 + 0.25 * within,
            (_, Stage::Summarizing) => 0.30 + 0.60 * within,
            (_, Stage::Speaking) => 0.55 + 0.40 * within,
            (_, Stage::Assembling) => 0.96,
            (_, Stage::Saving) => 0.99,
        }
    }

    fn with(&self, status: Status, stage: Option<Stage>, done: u32, total: u32) -> Run {
        Run {
            status,
            stage,
            done,
            total,
            ..self.clone()
        }
    }

    fn started(&self) -> Run {
        Run {
            status: Status::Running,
            stage: Some(Stage::Preparing),
            done: 0,
            total: 0,
            started_at: Some(Utc::now()),
            ..self.clone()
        }
    }

    fn ended(&self, status: Status, error: Option<String>) -> Run {
        Run {
            status,
            stage: None,
            done: 0,
            total: 0,
            elapsed_ms: elapsed_since(self.started_at),
            error,
            ..self.clone()
        }
    }
}

fn elapsed_since(started: Option<DateTime<Utc>>) -> u64 {
    started
        .map(|t| (Utc::now() - t).num_milliseconds().max(0) as u64)
        .unwrap_or(0)
}

/// What a plan is for: a file, the microphone, text pasted in (Summarize, Read aloud), or nothing
/// chosen yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanInput {
    File(PathBuf),
    Microphone,
    Text(String),
    Nothing,
}

/// A download a run still needs: what it is, in words, and its size.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Need {
    pub what: String,
    pub bytes: u64,
    #[serde(skip)]
    kind: NeedKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NeedKind {
    SpeechModel(String),
    SpeechEngine,
    Ffmpeg,
    VoiceEngine,
    Voice(String),
    /// An engine the document reader needs (Pandoc, PDFium, LibreOffice).
    Component(EngineComponent),
}

/// What a run with these inputs would do (`FlowService.Plan`): which voice speaks, said in a
/// sentence (`spoken_with`, a warning when `no_voice`), the downloads still missing, what else
/// stands in the way, and the model that translates.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub voice_name: Option<String>,
    pub cloned: bool,
    pub spoken_with: String,
    pub no_voice: bool,
    pub needs: Vec<Need>,
    pub total_bytes: u64,
    pub problem: Option<String>,
    pub ready: bool,
    pub model_id: Option<String>,
    pub model_name: Option<String>,
}

/// The downloads while they run (`FlowService.Install`): which one now, bytes so far of the
/// whole, or what went wrong.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Install {
    pub what: String,
    pub done: u64,
    pub total: u64,
    pub error: Option<String>,
}

/// The runs and the downloads. Build it once and share the `Arc`; call
/// [`FlowService::shutdown`] before exit.
pub struct FlowService {
    runtime: Arc<dyn FlowRuntime>,
    dir: PathBuf,
    voices_dir: PathBuf,
    temp: PathBuf,
    voices: Voices,
    /// A stand-in for the audio engine (tests); None speaks with the engine.
    speaker: Option<Arc<dyn Speaker>>,
    downloader: Downloader,
    runs: Mutex<HashMap<String, Run>>,
    /// The original's `stopRequested`: a token per queued or running run, cancelled to stop it.
    stops: Mutex<HashMap<String, CancellationToken>>,
    install: Mutex<Option<Install>>,
    install_cancel: Mutex<Option<CancellationToken>>,
    queue: mpsc::UnboundedSender<String>,
    receiver: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    stopping: CancellationToken,
    /// What reads documents (the converter), given once it is built.
    reader: std::sync::OnceLock<Arc<dyn Reader>>,
    me: Weak<FlowService>,
}

impl FlowService {
    /// The flows on the app's runtime: runs in the home's flows folder, voices in its voices
    /// folder, scratch work in `tmp\flows`.
    pub fn for_runtime(runtime: Arc<RuntimeManager>) -> Arc<FlowService> {
        let home = runtime.config().home.clone();
        FlowService::new(
            runtime,
            home.flows_dir(),
            home.voices_dir(),
            home.temp_dir().join("flows"),
            Voices::bundled(),
            None,
        )
    }

    /// Reads the runs finished in earlier sessions; starts nothing until the first run.
    pub fn new(
        runtime: Arc<dyn FlowRuntime>,
        dir: PathBuf,
        voices_dir: PathBuf,
        temp: PathBuf,
        voices: Voices,
        speaker: Option<Arc<dyn Speaker>>,
    ) -> Arc<FlowService> {
        let (queue, receiver) = mpsc::unbounded_channel();
        let runs = load_finished(&dir);
        Arc::new_cyclic(|me| FlowService {
            runtime,
            dir,
            voices_dir,
            temp,
            voices,
            speaker,
            downloader: Downloader::new(),
            runs: Mutex::new(runs),
            stops: Mutex::new(HashMap::new()),
            install: Mutex::new(None),
            install_cancel: Mutex::new(None),
            queue,
            receiver: Mutex::new(Some(receiver)),
            worker: Mutex::new(None),
            stopping: CancellationToken::new(),
            reader: std::sync::OnceLock::new(),
            me: me.clone(),
        })
    }

    /// Gives the service what reads documents, for Summarize and Read aloud (the converter,
    /// which is built after it). Only the first call counts.
    pub fn set_reader(&self, reader: Arc<dyn Reader>) {
        let _ = self.reader.set(reader);
    }

    fn changed(&self, id: &str) {
        if let Some(run) = self.run(id) {
            events::emit(topic::FLOWS, json!({ "run": run }));
        }
    }

    fn install_changed(&self) {
        let install = self.install.lock().clone();
        events::emit(topic::FLOWS, json!({ "install": install }));
    }

    // ------------------------------------------------------------------ reading

    /// Every run, newest first.
    pub fn runs(&self) -> Vec<Run> {
        let mut all: Vec<Run> = self.runs.lock().values().cloned().collect();
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        all
    }

    pub fn run(&self, id: &str) -> Option<Run> {
        self.runs.lock().get(id).cloned()
    }

    /// Where a run's files are: the track, the text and the subtitles it wrote.
    pub fn folder(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }

    /// Where every run's folder is.
    pub fn root(&self) -> &Path {
        &self.dir
    }

    pub fn voices(&self) -> &Voices {
        &self.voices
    }

    /// Where a voice's file lives, installed or not.
    pub fn voice_file(&self, v: &Voice) -> PathBuf {
        self.voices_dir.join(&v.file.name)
    }

    pub fn voice_installed(&self, v: &Voice) -> bool {
        std::fs::metadata(self.voice_file(v))
            .is_ok_and(|m| m.is_file() && (v.file.bytes == 0 || m.len() == v.file.bytes))
    }

    /// The downloads while they run, else None.
    pub fn install(&self) -> Option<Install> {
        self.install.lock().clone()
    }

    // ------------------------------------------------------------------ planning

    /// What a run with these inputs would do and still needs. `target` is the language's code;
    /// `keep_voice` asks for the speaker's own voice. Opens a chosen file's headers, so call it
    /// on the blocking pool.
    pub fn plan(&self, input: &PlanInput, target: &str, keep_voice: bool) -> Plan {
        let facts = self.runtime.facts();
        self.plan_with(&facts, input, target, keep_voice).0
    }

    fn plan_with(
        &self,
        facts: &Facts,
        input: &PlanInput,
        target: &str,
        keep_voice: bool,
    ) -> (Plan, Option<Choice>, Vec<NeedKind>) {
        let mut problem = match input {
            PlanInput::Nothing | PlanInput::Text(_) => {
                Some("Choose an audio or video file.".to_string())
            }
            PlanInput::File(p) if !p.is_file() => {
                Some(format!("{} is not there any more.", audio::display_name(p)))
            }
            _ => None,
        };
        if problem.is_none() && facts.translator.is_none() {
            problem = Some(
                "No model to translate with is installed. Download one in Settings > Models."
                    .into(),
            );
        }
        let code = languages::code_of(Some(target)).unwrap_or_default();
        let choice = (!code.is_empty())
            .then(|| self.voices.pick(&code, keep_voice))
            .flatten();

        let mut needs = Vec::new();
        speech_needs(facts, &mut needs);
        if let PlanInput::File(p) = input {
            if facts.ffmpeg.is_none() && p.is_file() {
                if audio::is_video(p) {
                    needs.push(Need {
                        what: "FFmpeg, to put the video back together".into(),
                        bytes: facts.ffmpeg_bytes,
                        kind: NeedKind::Ffmpeg,
                    });
                } else if !audio::readable(p) {
                    needs.push(Need {
                        what: format!("FFmpeg, to read .{} files", audio::extension(p)),
                        bytes: facts.ffmpeg_bytes,
                        kind: NeedKind::Ffmpeg,
                    });
                }
            }
        }
        if let Some(c) = &choice {
            if facts.voice_engine.is_none() && self.speaker.is_none() {
                needs.push(Need {
                    what: "the voice engine".into(),
                    bytes: facts.voice_engine_bytes,
                    kind: NeedKind::VoiceEngine,
                });
            }
            if !self.voice_installed(&c.voice) {
                needs.push(Need {
                    what: format!("the {} voice", c.voice.name),
                    bytes: c.voice.file.bytes,
                    kind: NeedKind::Voice(c.voice.id.clone()),
                });
            }
        }

        let name = languages::name_of(target);
        let whose = if *input == PlanInput::Microphone {
            "your own voice"
        } else {
            "the speaker's own voice"
        };
        let mut spoken_with = match &choice {
            None => format!(
                "Nook has no voice for {name} yet, so this run gives the text and subtitles."
            ),
            Some(c) if c.cloned => format!("{name} will be spoken by {} in {whose}.", c.voice.name),
            Some(c) => format!(
                "{name} will be spoken in a standard voice by {}.",
                c.voice.name
            ),
        };
        if let Some(note) = choice.as_ref().and_then(|c| c.note.as_ref()) {
            spoken_with = format!("{note} {spoken_with}");
        }
        let kinds = needs.iter().map(|n| n.kind.clone()).collect();
        let total_bytes = needs.iter().map(|n| n.bytes).sum();
        let ready = problem.is_none() && needs.is_empty();
        let plan = Plan {
            voice_name: choice.as_ref().map(|c| c.voice.name.clone()),
            cloned: choice.as_ref().is_some_and(|c| c.cloned),
            spoken_with,
            no_voice: choice.is_none(),
            needs,
            total_bytes,
            problem,
            ready,
            model_id: facts.translator.as_ref().map(|t| t.0.clone()),
            model_name: facts.translator.as_ref().map(|t| t.1.clone()),
        };
        (plan, choice, kinds)
    }

    // ------------------------------------------------------------------ downloads

    /// Downloads everything [`plan`](Self::plan) says is missing for these inputs, one after the
    /// other, in the background; [`install`](Self::install) follows it as one whole. Needs the
    /// tokio runtime. Opens a chosen file's headers: call it on the blocking pool.
    pub fn start_install(
        &self,
        input: &PlanInput,
        target: &str,
        keep_voice: bool,
    ) -> Result<(), String> {
        if self.stopping.is_cancelled() {
            return Err("Nook is closing.".into());
        }
        {
            let install = self.install.lock();
            if install.as_ref().is_some_and(|i| i.error.is_none()) {
                return Ok(());
            }
        }
        let facts = self.runtime.facts();
        let (plan, choice, kinds) = self.plan_with(&facts, input, target, keep_voice);
        self.begin_install(plan, choice.map(|c| c.voice), kinds)
    }

    /// Starts downloading what `plan` needs, in the background.
    fn begin_install(
        &self,
        plan: Plan,
        voice: Option<Voice>,
        kinds: Vec<NeedKind>,
    ) -> Result<(), String> {
        if plan.needs.is_empty() {
            return Ok(());
        }
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "Downloads need the app's async runtime.".to_string())?;
        let cancel = self.stopping.child_token();
        *self.install_cancel.lock() = Some(cancel.clone());
        *self.install.lock() = Some(Install {
            what: plan.needs[0].what.clone(),
            done: 0,
            total: plan.total_bytes,
            error: None,
        });
        self.install_changed();
        let me = self.me.clone();
        handle.spawn(async move {
            let Some(me) = me.upgrade() else { return };
            let outcome = me.run_install(&plan, &kinds, voice.as_ref(), &cancel).await;
            *me.install.lock() = match outcome {
                Ok(true) => None,
                Ok(false) => Some(Install {
                    error: Some("The download was stopped.".into()),
                    ..me.install().unwrap_or(Install {
                        what: String::new(),
                        done: 0,
                        total: plan.total_bytes,
                        error: None,
                    })
                }),
                Err(e) => {
                    tracing::warn!("A flow's download failed: {e:#}");
                    Some(Install {
                        what: String::new(),
                        done: me.install().map_or(0, |i| i.done),
                        total: plan.total_bytes,
                        error: Some(format!("The download failed: {e:#}")),
                    })
                }
            };
            me.install_cancel.lock().take();
            me.install_changed();
        });
        Ok(())
    }

    /// The needs, one after the other; false when stopped.
    async fn run_install(
        &self,
        plan: &Plan,
        kinds: &[NeedKind],
        voice: Option<&Voice>,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        let total = plan.total_bytes;
        let mut before = 0u64;
        for (need, kind) in plan.needs.iter().zip(kinds) {
            {
                *self.install.lock() = Some(Install {
                    what: need.what.clone(),
                    done: before,
                    total,
                    error: None,
                });
            }
            self.install_changed();
            let me = self.me.clone();
            let what = need.what.clone();
            let cap = need.bytes;
            let base = before;
            let progress: Progress = Arc::new(move |done, _| {
                let Some(me) = me.upgrade() else { return };
                *me.install.lock() = Some(Install {
                    what: what.clone(),
                    done: base + done.min(cap.max(done)),
                    total,
                    error: None,
                });
                me.install_changed();
            });
            let ok = match kind {
                NeedKind::SpeechModel(id) => {
                    self.runtime.download_model(id, progress, cancel).await?
                }
                NeedKind::SpeechEngine => {
                    self.runtime
                        .install_component(EngineComponent::Whisper, progress, cancel)
                        .await?
                }
                NeedKind::Ffmpeg => {
                    self.runtime
                        .install_component(EngineComponent::Ffmpeg, progress, cancel)
                        .await?
                }
                NeedKind::VoiceEngine => {
                    self.runtime
                        .install_component(EngineComponent::Audio, progress, cancel)
                        .await?
                }
                NeedKind::Component(c) => {
                    self.runtime.install_component(*c, progress, cancel).await?
                }
                NeedKind::Voice(id) => {
                    let v = voice
                        .filter(|v| &v.id == id)
                        .or_else(|| self.voices.by_id(id))
                        .ok_or_else(|| anyhow!("Unknown voice {id}"))?;
                    let target = self.voice_file(v);
                    let outcome = self
                        .downloader
                        .download(
                            &v.file.url,
                            &target,
                            v.file.sha256.as_deref(),
                            v.file.bytes,
                            Some(&progress),
                            cancel,
                        )
                        .await?;
                    outcome != Outcome::Cancelled
                }
            };
            if !ok || cancel.is_cancelled() {
                return Ok(false);
            }
            before += need.bytes;
        }
        Ok(true)
    }

    /// Stops the downloads; what came down is kept for next time.
    pub fn cancel_install(&self) {
        if let Some(c) = self.install_cancel.lock().as_ref() {
            c.cancel();
        }
    }

    /// Forgets a failed download, so the button comes back.
    pub fn clear_install_error(&self) {
        let cleared = {
            let mut install = self.install.lock();
            if install.as_ref().is_some_and(|i| i.error.is_some()) {
                *install = None;
                true
            } else {
                false
            }
        };
        if cleared {
            self.install_changed();
        }
    }

    // ------------------------------------------------------------------ runs

    /// Queues the translation of a file. It starts when the runs before it are done.
    ///
    /// `source` is the spoken language's code, or None to let Whisper tell; `target` the code of
    /// the language to translate into. Fails with what stands in the way (see [`plan`]).
    ///
    /// [`plan`]: Self::plan
    pub fn submit_file(
        &self,
        input: &Path,
        source: Option<&str>,
        target: &str,
        keep_voice: bool,
    ) -> Result<Run, String> {
        self.check(&PlanInput::File(input.to_path_buf()), target, keep_voice)?;
        let input = std::path::absolute(input).unwrap_or_else(|_| input.to_path_buf());
        let id = new_id();
        let run = self.new_run(
            id,
            &input,
            audio::display_name(&input),
            Source::File,
            source,
            target,
            keep_voice,
        )?;
        self.queue_run(run)
    }

    /// Queues the translation of what was just said into the microphone: `recording` (the
    /// recorder's WAV) moves into the run's folder, where it stays with the run.
    pub fn submit_recording(
        &self,
        recording: &Path,
        source: Option<&str>,
        target: &str,
        keep_voice: bool,
    ) -> Result<Run, String> {
        self.check(&PlanInput::Microphone, target, keep_voice)?;
        let id = new_id();
        let kept = self.folder(&id).join("recording.wav");
        move_file(recording, &kept).map_err(|e| format!("{e:#}"))?;
        let run = self.new_run(
            id,
            &kept,
            "Recording".into(),
            Source::Microphone,
            source,
            target,
            keep_voice,
        )?;
        self.queue_run(run)
    }

    /// The same run again (Run again, Try again): a file is read afresh, a recording copied into
    /// the new run's folder.
    pub fn again(&self, id: &str) -> Result<Run, String> {
        let old = self
            .run(id)
            .ok_or_else(|| "That run is gone.".to_string())?;
        if old.flow != TRANSLATE_AUDIO {
            return self.again_nooklet(&old);
        }
        match old.source {
            Source::File => self.submit_file(
                Path::new(&old.input),
                old.source_language.as_deref(),
                &old.target_language,
                old.keep_voice,
            ),
            Source::Microphone => {
                self.check(&PlanInput::Microphone, &old.target_language, old.keep_voice)?;
                let from = PathBuf::from(&old.input);
                if !from.is_file() {
                    return Err("The recording is not there any more.".into());
                }
                let new = new_id();
                let kept = self.folder(&new).join("recording.wav");
                std::fs::create_dir_all(self.folder(&new))
                    .and_then(|_| std::fs::copy(&from, &kept))
                    .map_err(|e| format!("Could not copy the recording: {e}"))?;
                let run = self.new_run(
                    new,
                    &kept,
                    old.input_name.clone(),
                    Source::Microphone,
                    old.source_language.as_deref(),
                    &old.target_language,
                    old.keep_voice,
                )?;
                self.queue_run(run)
            }
            Source::Text => Err("Only a recording can be translated.".into()),
        }
    }

    /// What stands in the way of a run, as the message the person reads.
    fn check(&self, input: &PlanInput, target: &str, keep_voice: bool) -> Result<(), String> {
        if self.stopping.is_cancelled() {
            return Err("Nook is closing.".into());
        }
        let plan = self.plan(input, target, keep_voice);
        if let Some(problem) = plan.problem {
            return Err(problem);
        }
        if let Some(need) = plan.needs.first() {
            return Err(format!("This run needs a download first: {}.", need.what));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn new_run(
        &self,
        id: String,
        input: &Path,
        input_name: String,
        source: Source,
        source_language: Option<&str>,
        target: &str,
        keep_voice: bool,
    ) -> Result<Run, String> {
        let target = languages::code_of(Some(target))
            .ok_or_else(|| "Choose the language to translate into.".to_string())?;
        Ok(Run {
            id,
            flow: TRANSLATE_AUDIO.into(),
            input: input.display().to_string(),
            input_name,
            source,
            source_language: languages::code_of(source_language),
            target_language: target,
            model_id: self.runtime.facts().translator.map(|t| t.0),
            keep_voice,
            status: Status::Queued,
            created_at: Utc::now(),
            ..Run::default()
        })
    }

    fn queue_run(&self, run: Run) -> Result<Run, String> {
        self.ensure_worker()?;
        self.stops
            .lock()
            .insert(run.id.clone(), CancellationToken::new());
        self.runs.lock().insert(run.id.clone(), run.clone());
        self.changed(&run.id);
        if self.queue.send(run.id.clone()).is_err() {
            tracing::warn!("The flow queue is closed; run {} will not start", run.id);
        }
        Ok(run)
    }

    /// Starts the queue's worker with the first run.
    fn ensure_worker(&self) -> Result<(), String> {
        let mut worker = self.worker.lock();
        if worker.is_some() {
            return Ok(());
        }
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "Flows need the app's async runtime.".to_string())?;
        let Some(receiver) = self.receiver.lock().take() else {
            return Err("The flow queue is closed.".into());
        };
        *worker = Some(handle.spawn(work(self.me.clone(), receiver, self.stopping.clone())));
        Ok(())
    }

    /// Stops a queued or running run. A finished run is left as it is.
    pub fn cancel(&self, id: &str) {
        match self.run(id) {
            Some(r) if !r.finished() => {}
            _ => return,
        }
        self.stops
            .lock()
            .entry(id.to_string())
            .or_default()
            .cancel();
        let cancelled = {
            let mut runs = self.runs.lock();
            match runs.get_mut(id) {
                Some(now) if now.status == Status::Queued => {
                    *now = now.with(Status::Cancelled, None, 0, 0);
                    true
                }
                _ => false,
            }
        };
        if cancelled {
            self.changed(id);
        }
    }

    /// Removes a finished run and its files. Returns false for an unknown or unfinished run.
    pub fn delete(&self, id: &str) -> bool {
        {
            let mut runs = self.runs.lock();
            match runs.get(id) {
                Some(r) if r.finished() => {}
                _ => return false,
            }
            runs.remove(id);
        }
        if plain_id(id) {
            if let Err(e) = std::fs::remove_dir_all(self.folder(id)) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("Could not delete the files of run {id}: {e}");
                }
            }
        }
        events::emit(topic::FLOWS, json!({ "removed": id }));
        true
    }

    /// Stops a run in progress and the downloads when the app closes, so no engine is left
    /// behind. Waits up to fifteen seconds for the run to stop.
    pub async fn shutdown(&self) {
        self.stopping.cancel();
        self.cancel_install();
        {
            let runs = self.runs.lock();
            let mut stops = self.stops.lock();
            for r in runs.values().filter(|r| !r.finished()) {
                stops.entry(r.id.clone()).or_default().cancel();
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

    async fn execute(&self, id: &str) {
        let running = {
            let mut runs = self.runs.lock();
            match runs.get_mut(id) {
                Some(r) => {
                    if r.status == Status::Queued {
                        *r = r.started();
                    }
                    Some(r.clone())
                }
                None => None,
            }
        };
        let Some(running) = running.filter(|r| r.status == Status::Running) else {
            self.stops.lock().remove(id);
            return;
        };
        self.changed(id);
        let cancel = self.stops.lock().entry(id.to_string()).or_default().clone();
        let work = self.temp.join(id);
        // Stop ends the run at once: whatever it waits on (a batch from the chat model, which can
        // take minutes, Whisper, the voice engine, killed with it) is dropped, not waited for.
        let result = tokio::select! {
            r = self.perform(&running, &work, &cancel) => r,
            _ = cancel.cancelled() => Err(Stopped.into()),
        };
        let after = match result {
            Ok(done) => done,
            Err(e) if e.chain().any(|c| c.is::<Stopped>()) || cancel.is_cancelled() => {
                self.current(id).ended(Status::Cancelled, None)
            }
            Err(e) => {
                tracing::warn!("Flow run {id} failed: {e:#}");
                self.current(id)
                    .ended(Status::Failed, Some(format!("{e:#}")))
            }
        };
        // The scratch work goes before the run reads as ended, so whoever sees it ended finds
        // it gone.
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(work)).await;
        self.runs.lock().insert(id.to_string(), after);
        self.stops.lock().remove(id);
        self.changed(id);
    }

    fn current(&self, id: &str) -> Run {
        self.run(id).unwrap_or_default()
    }

    /// Moves a running run on to `stage`.
    fn stage(&self, id: &str, stage: Stage, done: usize, total: usize) {
        {
            let mut runs = self.runs.lock();
            let Some(now) = runs.get_mut(id).filter(|r| r.status == Status::Running) else {
                return;
            };
            *now = now.with(Status::Running, Some(stage), done as u32, total as u32);
        }
        self.changed(id);
    }

    /// Changes a running run's details.
    fn update(&self, id: &str, change: impl FnOnce(&mut Run)) {
        {
            let mut runs = self.runs.lock();
            let Some(now) = runs.get_mut(id) else { return };
            change(now);
        }
        self.changed(id);
    }

    /// The run's work, by its flow.
    async fn perform(&self, run: &Run, work: &Path, cancel: &CancellationToken) -> Result<Run> {
        match run.flow.as_str() {
            TRANSCRIBE => self.transcribe(run, work, cancel).await,
            SUMMARIZE => self.summarize(run, work, cancel).await,
            READ_ALOUD => self.read_aloud(run, work, cancel).await,
            _ => self.translate(run, work, cancel).await,
        }
    }

    async fn translate(&self, run: &Run, work: &Path, cancel: &CancellationToken) -> Result<Run> {
        let id = run.id.as_str();
        let facts = self.runtime.facts();
        let input = PathBuf::from(&run.input);
        let target = run.target_language.clone();

        // Prepare, and listen.
        let (wav, duration) = self
            .prepare_audio(run, &input, work, &facts, cancel)
            .await?;
        let (heard, detected) = self.listen(run, &wav, work, &facts, cancel).await?;
        let from = detected.clone().or_else(|| run.source_language.clone());
        {
            let heard = heard.clone();
            let detected = detected.clone();
            self.update(id, move |r| {
                r.detected_language = detected;
                r.duration_seconds = duration;
                r.segments = heard;
            });
        }

        // Translate.
        let mut model_id = run.model_id.clone();
        let translated: Vec<Segment> = if from.as_deref() == Some(target.as_str()) {
            heard
                .iter()
                .map(|s| s.with_translation(s.text.clone()))
                .collect()
        } else {
            let (model, _) = facts
                .translator
                .clone()
                .ok_or_else(|| anyhow!("No model to translate with is installed."))?;
            model_id = Some(model.clone());
            let lines: Vec<String> = heard.iter().map(|s| s.text.clone()).collect();
            let source_name = from.as_deref().and_then(languages::by_code).map(|l| l.name);
            let chat = RunChat {
                runtime: self.runtime.as_ref(),
                model,
                system: translator::system(source_name, &languages::name_of(&target)),
            };
            let total = lines.len();
            self.stage(id, Stage::Translating, 0, total);
            let out = translator::translate(&lines, &chat, cancel, &|n| {
                self.stage(id, Stage::Translating, n, total)
            })
            .await?;
            heard
                .iter()
                .zip(out)
                .map(|(s, t)| s.with_translation(t))
                .collect()
        };
        {
            let (segments, model_id) = (translated.clone(), model_id.clone());
            self.update(id, move |r| {
                r.segments = segments;
                r.model_id = model_id;
            });
        }
        stop_if(cancel)?;

        // Speak.
        let folder = self.folder(id);
        tokio::fs::create_dir_all(&folder)
            .await
            .with_context(|| format!("Could not create {}", folder.display()))?;
        let base = base_name(run);
        let mut note: Option<String>;
        let mut voice_name = None;
        let mut cloned = false;
        let mut audio_out = None;
        let mut video_out = None;
        let facts = self.runtime.facts();
        match self.voices.pick(&target, run.keep_voice) {
            None => {
                note = Some(format!(
                    "Nook has no voice for {} yet, so this run gives the text and subtitles.",
                    languages::name_of(&target)
                ))
            }
            Some(choice) if !self.can_speak(&facts, &choice.voice) => {
                note = Some(
                    "The voice was not installed when the run started, so this run gives the text and subtitles."
                        .into(),
                )
            }
            Some(choice) => {
                let spoken = self
                    .speak(run, &choice, &translated, &wav, duration, work, cancel, &facts)
                    .await?;
                note = spoken.note.or_else(|| choice.note.clone());
                voice_name = Some(spoken.choice.voice.name.clone());
                cloned = spoken.choice.cloned;
                stop_if(cancel)?;
                self.stage(id, Stage::Assembling, 0, 0);
                let layout = match run.source {
                    Source::Microphone | Source::Text => Layout::Compact,
                    Source::File => Layout::Timeline,
                };
                let out = folder.join(format!("{base}.{target}.wav"));
                {
                    let (lines, clips, out) = (translated.clone(), spoken.clips, out.clone());
                    blocking(move || dub::assemble(&lines, &clips, layout, duration, &out)).await?;
                }
                if audio::is_video(&input) {
                    if let Some(ffmpeg) = &facts.ffmpeg {
                        match dub::mux(ffmpeg, &input, &out, &folder.join(format!("{base}.{target}"))).await {
                            Ok(v) => video_out = Some(v.display().to_string()),
                            Err(e) => {
                                tracing::warn!("Run {id}: the video could not be put together: {e:#}");
                                let why = format!("The video could not be put together: {e:#}");
                                note = Some(match note {
                                    Some(n) => format!("{n} {why}"),
                                    None => why,
                                });
                            }
                        }
                    }
                }
                audio_out = Some(out.display().to_string());
            }
        }
        stop_if(cancel)?;

        // Save.
        self.stage(id, Stage::Saving, 0, 0);
        let now = self.current(id);
        let done = Run {
            model_id,
            voice_name,
            cloned,
            note,
            status: Status::Done,
            stage: None,
            done: 0,
            total: 0,
            detected_language: detected,
            duration_seconds: duration,
            segments: translated,
            audio: audio_out,
            video: video_out,
            elapsed_ms: elapsed_since(now.started_at),
            error: None,
            started_at: now.started_at,
            ..run.clone()
        };
        {
            let (done, folder) = (done.clone(), folder.clone());
            blocking(move || write_outputs(&done, &folder, &base)).await?;
        }
        Ok(done)
    }

    /// The run's sound as 16 kHz WAV in `work`, and how long it is.
    async fn prepare_audio(
        &self,
        run: &Run,
        input: &Path,
        work: &Path,
        facts: &Facts,
        cancel: &CancellationToken,
    ) -> Result<(PathBuf, f64)> {
        self.stage(&run.id, Stage::Preparing, 0, 0);
        tokio::fs::create_dir_all(work)
            .await
            .with_context(|| format!("Could not create {}", work.display()))?;
        let wav = work.join("audio.wav");
        let duration = {
            let (input, wav, ffmpeg, cancel) = (
                input.to_path_buf(),
                wav.clone(),
                facts.ffmpeg.clone(),
                cancel.clone(),
            );
            blocking(move || audio::to_speech_wav(&input, &wav, ffmpeg.as_deref(), &cancel)).await?
        };
        stop_if(cancel)?;
        Ok((wav, duration))
    }

    /// What is said in `wav`, in parts: the lines with their times, and the language Whisper
    /// heard (a code, or its name when Nook does not list it). Fails when no speech was heard.
    async fn listen(
        &self,
        run: &Run,
        wav: &Path,
        work: &Path,
        facts: &Facts,
        cancel: &CancellationToken,
    ) -> Result<(Vec<Segment>, Option<String>)> {
        let id = run.id.as_str();
        let parts = {
            let (wav, dir) = (wav.to_path_buf(), work.join("parts"));
            blocking(move || audio::split(&wav, &dir, CHUNK_SECONDS)).await?
        };
        let speech_model = facts
            .speech_model
            .clone()
            .ok_or_else(|| anyhow!("No speech model is downloaded."))?;
        let mut language = run.source_language.clone();
        let mut detected: Option<String> = None;
        let mut heard: Vec<Segment> = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            stop_if(cancel)?;
            self.stage(id, Stage::Listening, i, parts.len());
            let reply = self
                .runtime
                .transcribe(&part.file, &speech_model, language.as_deref())
                .await?;
            if detected.is_none() {
                detected = languages::code_of(reply.get("language").and_then(Value::as_str));
                // The parts after the first keep to the language the first one was heard in.
                if language.is_none() {
                    language = detected.clone().filter(|d| languages::by_code(d).is_some());
                }
            }
            heard.extend(segments_of(&reply, part.offset_seconds));
        }
        if heard.is_empty() {
            bail!(match run.source {
                Source::Microphone => "Nook heard no speech in the recording.".to_string(),
                _ => format!("Nook heard no speech in {}.", run.input_name),
            });
        }
        Ok((heard, detected))
    }

    fn can_speak(&self, facts: &Facts, voice: &Voice) -> bool {
        (self.speaker.is_some() || facts.voice_engine.is_some()) && self.voice_installed(voice)
    }

    /// Speaks the translated lines: in the speaker's voice cloned from the best few seconds of
    /// the track when the voice clones; when that fails, in a standard voice when one is in.
    #[allow(clippy::too_many_arguments)]
    async fn speak(
        &self,
        run: &Run,
        choice: &Choice,
        lines: &[Segment],
        wav: &Path,
        duration: f64,
        work: &Path,
        cancel: &CancellationToken,
        facts: &Facts,
    ) -> Result<Spoken> {
        let stretch =
            reference::pick(lines).unwrap_or_else(|| reference::fallback(lines, duration));
        let reference_wav = work.join("reference.wav");
        let female = {
            let (wav, out) = (wav.to_path_buf(), reference_wav.clone());
            let (start, end) = (stretch.start, stretch.end);
            blocking(move || {
                reference::cut(&wav, start, end, &out)?;
                let pitch = audio::pitch_hz(&audio::read_wav(&out)?);
                Ok(pitch.is_some_and(|hz| hz > audio::FEMALE_ABOVE_HZ))
            })
            .await?
        };
        let mut to_speak = Vec::new();
        let mut line_of = Vec::new();
        for (i, s) in lines.iter().enumerate() {
            let Some(t) = s.translation.as_deref().filter(|t| !t.trim().is_empty()) else {
                continue;
            };
            to_speak.push(Line {
                id: format!("line-{i}"),
                text: t.to_string(),
            });
            line_of.push(i);
        }
        let request = |c: &Choice, out: &str| Request {
            voice: c.voice.clone(),
            model: self.voice_file(&c.voice),
            language: run.target_language.clone(),
            lines: to_speak.clone(),
            cloned: c.cloned,
            reference_wav: c.cloned.then(|| reference_wav.clone()),
            reference_text: c.cloned.then(|| stretch.text.clone()),
            female_speaker: female,
            out_dir: work.join(out),
        };
        let first = self
            .speak_with(&run.id, &request(choice, "spoken"), facts, cancel)
            .await;
        let (spoken, used, note) = match first {
            Ok(s) => (s, choice.clone(), None),
            Err(e) if e.is::<Stopped>() => return Err(e),
            Err(e) => {
                let Some(standard) = self
                    .voices
                    .fallback(&run.target_language, choice)
                    .filter(|c| self.can_speak(facts, &c.voice))
                else {
                    return Err(e);
                };
                tracing::warn!(
                    "Run {}: {} could not clone the voice ({e:#}); speaking with {}",
                    run.id,
                    choice.voice.name,
                    standard.voice.name
                );
                let spoken = self
                    .speak_with(
                        &run.id,
                        &request(&standard, "spoken-standard"),
                        facts,
                        cancel,
                    )
                    .await?;
                let note = format!(
                    "{} could not speak in the speaker's voice, so it is spoken in a standard voice by {}.",
                    choice.voice.name, standard.voice.name
                );
                (spoken, standard, Some(note))
            }
        };
        let mut clips = vec![None; lines.len()];
        for (j, clip) in spoken.into_iter().enumerate() {
            clips[line_of[j]] = clip;
        }
        Ok(Spoken {
            clips,
            choice: used,
            note,
        })
    }

    async fn speak_with(
        &self,
        id: &str,
        request: &Request,
        facts: &Facts,
        cancel: &CancellationToken,
    ) -> Result<Vec<Option<PathBuf>>> {
        let speaker: Arc<dyn Speaker> = match &self.speaker {
            Some(s) => s.clone(),
            None => {
                let cli = facts
                    .voice_engine
                    .clone()
                    .ok_or_else(|| anyhow!("The voice engine is not installed."))?;
                let threads = std::thread::available_parallelism()
                    .map(|n| (n.get() / 2).max(2))
                    .unwrap_or(2);
                Arc::new(AudioEngine::new(cli, &facts.voice_backend, threads))
            }
        };
        // Qwen3-TTS (a 1.9 GB file) peaked at 3.4 GB on an RTX 4060 with CUDA, 3.9 GB with
        // Vulkan (2026-09-26).
        let need = request.voice.file.bytes * 3 / 2 + (1 << 30);
        let Some(_turn) = self.runtime.gpu_turn(need, cancel).await else {
            return Err(Stopped.into());
        };
        let total = request.lines.len();
        self.stage(id, Stage::Speaking, 0, total);
        speaker
            .speak(
                request,
                &|d, t| self.stage(id, Stage::Speaking, d, t),
                cancel,
            )
            .await
    }
}

/// What speaking gave: a clip per line (None where none), the voice that spoke, and why it is not
/// the one asked for.
struct Spoken {
    clips: Vec<Option<PathBuf>>,
    choice: Choice,
    note: Option<String>,
}

/// The run's chat model as the translator asks it.
struct RunChat<'a> {
    runtime: &'a dyn FlowRuntime,
    model: String,
    system: String,
}

#[async_trait]
impl Chat for RunChat<'_> {
    async fn reply(&self, user: &str) -> Result<String> {
        self.runtime.chat(&self.model, &self.system, user).await
    }
}

impl BusyWork for FlowService {
    fn busy_with(&self) -> Option<String> {
        if self
            .install
            .lock()
            .as_ref()
            .is_some_and(|i| i.error.is_none())
        {
            return Some("a flow's download is running".into());
        }
        self.runs
            .lock()
            .values()
            .any(|r| matches!(r.status, Status::Queued | Status::Running))
            .then(|| "a flow is running".to_string())
    }
}

/// The queue: one run at a time, in the order they were asked for, until the service closes.
async fn work(
    me: Weak<FlowService>,
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
        let Some(service) = me.upgrade() else { break };
        service.execute(&id).await;
    }
}

/// What listening still needs: the speech model (with its engine), or the engine alone.
fn speech_needs(facts: &Facts, needs: &mut Vec<Need>) {
    match (&facts.speech_model, &facts.speech_download) {
        (None, Some((id, bytes))) => needs.push(Need {
            what: "the speech model".into(),
            bytes: *bytes,
            kind: NeedKind::SpeechModel(id.clone()),
        }),
        (Some(_), _) if !facts.speech_engine_installed => needs.push(Need {
            what: "the speech engine".into(),
            bytes: facts.speech_engine_bytes,
            kind: NeedKind::SpeechEngine,
        }),
        _ => {}
    }
}

fn stop_if(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(Stopped.into())
    } else {
        Ok(())
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| anyhow!("the work was interrupted: {e}"))?
}

/// Whisper's segments, placed at `offset` in the whole. What it writes for sounds instead of
/// words ("[Music]", "(applause)") is left out, so no voice reads it aloud.
fn segments_of(reply: &Value, offset: f64) -> Vec<Segment> {
    reply
        .get("segments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let text = s.get("text").and_then(Value::as_str)?.trim();
            let sound = (text.starts_with('[') && text.ends_with(']'))
                || (text.starts_with('(') && text.ends_with(')'))
                || text.chars().all(|c| !c.is_alphanumeric());
            if text.is_empty() || sound {
                return None;
            }
            let at = |key: &str| s.get(key).and_then(Value::as_f64).unwrap_or(0.0);
            Some(Segment::new(offset + at("start"), offset + at("end"), text))
        })
        .collect()
}

fn base_name(run: &Run) -> String {
    match run.source {
        Source::Microphone => return "recording".into(),
        Source::Text => return "text".into(),
        Source::File => {}
    }
    let name = run.input_name.as_str();
    match name.rfind('.') {
        Some(dot) if dot > 0 => name[..dot].to_string(),
        _ => name.to_string(),
    }
}

/// Moves a file, copying it when a rename cannot (another drive).
fn move_file(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Could not create {}", parent.display()))?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)
        .with_context(|| format!("Could not keep the recording in {}", to.display()))?;
    let _ = std::fs::remove_file(from);
    Ok(())
}

/// The run's folder: the transcript and the translation as text and as subtitles, and `run.json`
/// (the track is already there).
fn write_outputs(run: &Run, folder: &Path, base: &str) -> Result<()> {
    let from = run.detected_language.as_deref().unwrap_or("original");
    let to = run.target_language.as_str();
    let files = [
        (
            format!("{base}.{from}.txt"),
            subtitles::plain(&run.segments, false),
        ),
        (
            format!("{base}.{from}.srt"),
            subtitles::srt(&run.segments, false),
        ),
        (
            format!("{base}.{to}.txt"),
            subtitles::plain(&run.segments, true),
        ),
        (
            format!("{base}.{to}.srt"),
            subtitles::srt(&run.segments, true),
        ),
        ("run.json".to_string(), serde_json::to_string_pretty(run)?),
    ];
    for (name, text) in files {
        let path = folder.join(name);
        std::fs::write(&path, text)
            .with_context(|| format!("Could not write {}", path.display()))?;
    }
    Ok(())
}

/// An id that names a folder inside the flows folder.
fn plain_id(id: &str) -> bool {
    !id.is_empty() && !id.contains(['/', '\\', ':']) && !id.contains("..")
}

/// Runs finished in earlier sessions: every folder with a readable `run.json` naming it.
fn load_finished(dir: &Path) -> HashMap<String, Run> {
    let mut runs = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return runs;
    };
    for entry in entries.flatten() {
        let folder = entry.path();
        let json = folder.join("run.json");
        if !json.is_file() {
            continue;
        }
        let read = std::fs::read(&json)
            .map_err(anyhow::Error::from)
            .and_then(|b| serde_json::from_slice::<Run>(&b).map_err(anyhow::Error::from));
        match read {
            Ok(mut run) => {
                let name = folder
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                if !plain_id(&run.id) || run.id != name {
                    continue;
                }
                run.status = Status::Done;
                run.stage = None;
                run.error = None;
                let exists =
                    |p: &Option<String>| p.as_ref().is_some_and(|p| Path::new(p).is_file());
                if !exists(&run.audio) {
                    run.audio = None;
                }
                if !exists(&run.video) {
                    run.video = None;
                }
                run.files.retain(|f| Path::new(f).is_file());
                runs.insert(run.id.clone(), run);
            }
            Err(e) => tracing::warn!(
                "Skipping an unreadable flow run {}: {e:#}",
                folder.display()
            ),
        }
    }
    runs
}

/// Sortable by time and unique: the creation instant, then a short random tail.
fn new_id() -> String {
    let millis = Utc::now().timestamp_millis().max(0) as u64;
    let tail = uuid::Uuid::new_v4().simple().to_string();
    format!("flow_{}{}", base36(millis), &tail[..6])
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

#[path = "service_nooklets.rs"]
mod nooklets;
pub use nooklets::{Peek, PARAGRAPH_PAUSE, WORDS_A_MINUTE};

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "service_nooklets_tests.rs"]
mod nooklets_tests;

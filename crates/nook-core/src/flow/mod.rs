//! Flows: everyday jobs done in steps on the local runtime, one run at a time, with what each step
//! is doing shown as it goes. Ports `ai.nook.agent.flow` of the Kotlin Nook 0.4.3 (commit
//! 7ecf8f1, in the archive's bundle), which the 0.4.2 port left out, and adds the microphone.
//!
//! The first flow translates speech into another language, as speech: a file (a recording, a
//! podcast, a video) or what the person says into the microphone. The sound becomes 16 kHz WAV
//! ([`audio`]), Whisper writes down what is said in parts, the chat model translates the lines in
//! numbered batches ([`translator`]), the audio engine speaks them, in the speaker's own voice when
//! a cloning voice speaks the language ([`voice_engine`], [`voices`]), and the spoken lines are laid
//! over the original timeline ([`dub`]). The run's folder gets the track (and the video with it,
//! when the input was one), and the text and subtitles in both languages.
//!
//! - [`FlowService`] (`service.rs`): the runs and the downloads a run needs. Build it once,
//!   `FlowService::for_runtime(runtime.clone())` (runs in `Home::flows_dir()`, voices in
//!   `Home::voices_dir()`), register it as a [`BusyWork`](crate::busy::BusyWork), and call
//!   `flows.shutdown().await` before the runtime's at exit. Its calls: `runs()`, `run(id)`,
//!   `plan(input, target, keep_voice)` (file work: call it on the blocking pool), `install(..)`,
//!   `cancel_install()`, `clear_install_error()`, `submit_file(..)`, `submit_recording(..)`,
//!   `cancel(id)`, `delete(id)`, `folder(id)`.
//! - [`FlowRuntime`] (`runtime.rs`): the runtime as a run sees it, implemented by
//!   [`RuntimeManager`](crate::runtime::RuntimeManager) and by a scripted fake in the tests.
//!
//! Three more Nooklets run through the same queue, so no two of them want the card at once
//! (`service_nooklets.rs`, each queued with an [`Order`]):
//!
//! - *Transcribe*: a recording (a file, or the microphone) written down by Whisper, as text, a
//!   timed transcript and subtitles, with notes by the chat model when asked ([`summarize`]).
//! - *Summarize*: a document (read through the converter's engines, [`reader`]) or pasted text,
//!   summarized by the chat model in parts when it is long.
//! - *Read aloud*: a document or pasted text spoken by a standard voice line after line
//!   ([`aloud`]), as one track.
//!
//! Events on [`topic::FLOWS`](crate::events::topic::FLOWS): `{"run": Run}` when a run is added or
//! moves on, `{"removed": id}` when one is deleted, `{"install": Install | null}` while the
//! downloads run. The shapes are those of `ui/src/api/flows.ts`.

pub mod aloud;
pub mod audio;
pub mod dub;
pub mod languages;
pub mod reader;
pub mod reference;
pub mod runtime;
pub mod service;
pub mod subtitles;
pub mod summarize;
pub mod translator;
pub mod voice_engine;
pub mod voices;

pub use reader::{Reader, ReaderNeed, Sample};
pub use runtime::{Facts, FlowRuntime};
pub use service::{
    FlowService, Install, Need, Order, Peek, Plan, PlanInput, Run, Source, Stage, Status,
    READ_ALOUD, SUMMARIZE, TRANSCRIBE, TRANSLATE_AUDIO,
};
pub use subtitles::Segment;
pub use summarize::Length;

/// The error a stopped run ends with (the original's `CancellationException`); callers tell it
/// from a failure with `err.is::<Stopped>()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stopped;

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The run was stopped.")
    }
}

impl std::error::Error for Stopped {}

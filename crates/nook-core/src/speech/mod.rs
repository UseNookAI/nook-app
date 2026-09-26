//! The voice prompt: microphone capture to a 16 kHz mono WAV with live levels. Ports
//! `service/AudioRecorderService.kt` (transcription itself is the runtime's whisper engine).
//!
//! - [`recorder`]: the default microphone on a thread of its own; start, stop (the WAV), cancel.
//! - [`pipeline`]: downmix, resample to 16 kHz, 16-bit, the waveform's levels, the WAV file.
//!
//! Events on `speech` ([`SpeechEvent`]): `{"kind":"started"}`, `{"kind":"level","level":0.42}`
//! about 25 times a second, `{"kind":"stopped"}`, `{"kind":"cancelled"}`,
//! `{"kind":"error","message":...}`. The Code composer keeps the last 60 levels for its waveform.
//! `start` and `stop` block briefly (opening the device, joining the thread, writing the file):
//! call them from `spawn_blocking` in async commands.

pub mod pipeline;
pub mod recorder;

pub use recorder::{Recorder, SpeechEvent, MICROPHONE_NOT_SUPPORTED};

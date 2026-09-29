//! The screen recorder Nooklet: records or streams a screen, a window or part of a screen, with a
//! microphone and the computer's own sound, through FFmpeg.
//!
//! - [`sources`]: the screens and windows, as Windows lists them.
//! - [`audio`]: the sound devices, and the mixer that takes them into one stream.
//! - [`plan`]: FFmpeg's command line.
//! - [`secret`]: stream keys, kept encrypted for the person's account.
//! - [`service`]: the recordings themselves ([`CaptureService`]).
//! - `mac`: on a Mac, the capture itself (ScreenCaptureKit), which FFmpeg only encodes.

pub mod audio;
#[cfg(target_os = "macos")]
pub mod mac;
pub mod plan;
pub mod secret;
pub mod service;
pub mod sources;

pub use plan::{Encoder, Quality, Source};
pub use service::{CaptureService, CaptureState, Sources, StartOptions};

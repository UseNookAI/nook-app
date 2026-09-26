//! The Video page's renders. Ports `video/VideoStudio.java` (the engine itself is
//! `runtime::video_engine`) and the setup `ui/component/video/VideoScreen.kt` read for its page.
//!
//! - [`VideoStudio`] (`studio.rs`): the clip queue. Build it once when the app's services are
//!   built, `VideoStudio::for_runtime(runtime.clone())` (clips in `Home::videos_dir()`), share
//!   the `Arc`, register it as a [`BusyWork`](crate::busy::BusyWork), and call
//!   `studio.shutdown().await` before `runtime.shutdown().await` at exit. Its calls are
//!   `clips()` (newest first), `clip(id)`, `submit(prompt, model_id) -> Result<Clip,
//!   SubmitError>` (needs the tokio runtime: the queue's worker starts with the first clip),
//!   `cancel(id)`, `delete(id) -> bool`, `problem()`, `folder()`. Every change goes out on
//!   [`topic::VIDEO`](crate::events::topic::VIDEO) with `clips()` as the payload.
//! - [`VideoSetup`] (`setup.rs`): what `video_setup` returns,
//!   `VideoSetup::read(&runtime, &studio, model_id).await`; and [`DownloadState`], what
//!   `video_download_state` returns, `DownloadState::of(&downloads.state(), model_id)`.
//!
//! The shapes are those of `ui/src/api/video.ts`: `Clip` (camelCase, `status` and `stage` as the
//! Java constant names, instants in epoch milliseconds, `file` an absolute path), `VideoSetup`,
//! `VideoModel`, `DownloadState`.

pub mod setup;
pub mod studio;

pub use setup::{DownloadState, VideoModel, VideoSetup};
pub use studio::{Clip, Status, SubmitError, VideoStudio};

//! Code sessions. Ports `ai.nook.agent.code` (CodeService, CodeSession, CodeStore, CodeWorkspace)
//! and the snapshot `ui/component/code/CodeHub.kt` read from the service.
//!
//! - [`code_session`]: [`CodeSession`] and its entries ([`Task`], [`Run`], [`Note`], tagged
//!   `"kind"`), the JSON the UI types in `ui/src/api/code.ts` mirror.
//! - [`code_store`]: one JSON file per session under `<home>\code\sessions` (sessions holding the
//!   old Claude reviews are kept once in `<home>\code\with-reviews`).
//! - [`code_workspace`]: the scratch copies under `<home>\tmp\code\<session id>`: a git worktree
//!   of a repository with a commit, else a private copy with its git record beside it
//!   (`<id>.git`); diffs, stats, baseline trees, apply, reset, remove; [`RepositoryState`].
//! - [`code_service`]: [`CodeService`], the sessions and their turns (the worker loop from
//!   [`crate::worker`] over the runtime's worker model).
//!
//! # Constructing and starting it
//!
//! The app builds one service when it starts, after the runtime (the `RuntimeManager`, which
//! implements [`WorkerRuntime`](crate::runtime::api::WorkerRuntime)) and the worker's web access:
//!
//! ```ignore
//! let web = Arc::new(WebAccess::new(&home)?);                  // its switch is <home>\web.json
//! let code = CodeService::new(home.clone(), runtime.clone(), web.clone());
//! busy_work.push(Arc::new(code.clone()) as Arc<dyn BusyWork>); // the updater asks it
//! // on exit: code.shutdown();                                 // stops running turns
//! ```
//!
//! `new` reads the saved sessions (a run that was in flight when Nook closed is marked ended with
//! "Nook closed before this run finished."); nothing else needs starting. The service is cheap to
//! clone and every clone is the same service. Turns run as tokio tasks, so `start`/`send` (and
//! `delete`, which removes the scratch copy in the background) must be called inside the tokio
//! runtime, as Tauri's async commands are.
//!
//! Every change to a session or a phase is emitted as [`crate::events::topic::CODE`] with a
//! [`CodeChanged`] payload (`{"sessionId": "…"}` or `{"sessionId": null}`); the UI then calls
//! `code_snapshot` again. A model that finishes downloading changes the worker menu too: the
//! UI re-reads the snapshot on "downloads" events as the Kotlin hub did on installed models.
//!
//! # The UI's commands (`ui/src/api/code.ts`) and what they call
//!
//! | command | CodeService |
//! |---|---|
//! | `code_snapshot` | [`CodeService::snapshot`] (reads the models folder: call off the async threads) |
//! | `code_start { folder, text, verify, context }` | [`CodeService::start`]`(folder, text, verify, context)` → the new [`CodeSession`] |
//! | `code_send { id, text, verify, context }` | [`CodeService::send`] |
//! | `code_stop { id }` | [`CodeService::stop`] |
//! | `code_apply { id }` | [`CodeService::apply`] |
//! | `code_discard { id }` | [`CodeService::discard`] |
//! | `code_undo { id }` | [`CodeService::undo`] |
//! | `code_delete { id }` | [`CodeService::delete`] |
//! | `code_rename { id, title }` | [`CodeService::rename`] |
//! | `code_run_diff { sessionId, runId }` | [`CodeService::run_diff`] → `string \| null` |
//! | `code_next_context { id }` | [`CodeService::next_context`] → [`NextContext`] or null without a session or a worker |
//! | `code_repository_state { folder }` | [`CodeService::repository_state`] → [`RepositoryState`] (an error for a path the policy refuses) |
//! | `code_recent_repositories` | [`CodeService::recent_repositories`] |
//! | `code_set_worker { modelId }` | [`CodeService::set_worker`] |
//! | `code_speech_problem` | [`CodeService::speech_problem`] |
//! | `code_speech_model` | [`CodeService::speech_model`] |
//! | `speech_stop_and_transcribe` | the recorder in [`crate::speech`], then [`CodeService::transcribe`]`(wav)` |
//! | `speech_start`, `speech_cancel` | [`crate::speech`] only |
//! | `code_speech_download`, `code_speech_install` | the model downloads (`ModelDownloadService`'s port) for [`CodeService::speech_model`]'s id; not this service |
//!
//! Errors are `anyhow` errors whose message is the sentence the UI shows (the command returns
//! `e.to_string()`).

pub mod code_service;
pub mod code_session;
pub mod code_store;
pub mod code_workspace;

pub use code_service::{
    CodeChanged, CodeService, CodeSnapshot, EditorContext, NextContext, SpeechModel, WorkerChoice,
    THINKING,
};
pub use code_session::{Change, CodeSession, Entry, Note, Run, RunContext, Task};
pub use code_store::CodeStore;
pub use code_workspace::{Readiness, RepositoryState};

//! Code mode: the person asks, the local worker changes a scratch copy of their repository, and
//! they read the diff, ask for more, apply it or throw it away. The worker is [`WorkerLoop`]
//! (tools, gate, budget); what differs is that the scratch copy outlives a turn and the person,
//! not an agent, decides.
//!
//! State lives here, in memory and in [`CodeStore`]; every change goes out as a
//! [`topic::CODE`](crate::events::topic::CODE) event (the original's listeners), after which the
//! UI reads [`CodeService::snapshot`] again.
//!
//! Ports `code/CodeService.java` (and `CodeHub.read()` for the snapshot). Turns run as tokio tasks.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use futures::FutureExt;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::code_session::{
    now_millis, Change, CodeSession, Entry, Note, Origin, Run, Task, APPLIED, DISCARDED, UNDONE,
};
use super::code_store::CodeStore;
use super::code_workspace::{self, Created, RepositoryState};
use crate::busy::BusyWork;
use crate::events::{self, topic};
use crate::home::Home;
use crate::runtime::api::{Lease, Priority, WorkerRuntime};
use crate::runtime::manager::{AdmissionError, ModelLoadError};
use crate::runtime::model_registry::{LocalModel, CODE_WORKER};
use crate::runtime::thinking;
use crate::web::{Web, WebAccess};
use crate::worker::jdk_locator;
use crate::worker::path_policy::PathPolicy;
use crate::worker::verify_commands::VerifyCommands;
use crate::worker::web_tools::WebTools;
use crate::worker::worker_loop::{
    estimate_tokens, opening, strip_thinking, Budget, Engine, EngineUnavailable, WorkerLoop,
};
use crate::worker::worker_tools::WorkerTools;

/// How much of a run's diff is kept in the session record (characters); the scratch copy holds
/// all of it.
pub const MAX_STORED_DIFF: usize = 300_000;
/// A Code turn's budget: the person is waiting on it and can stop it.
pub const TURN_TOOL_CALLS: u32 = 40;
pub const TURN_SECONDS: u64 = 20 * 60;

/// The phase of a turn while the model writes its next move.
pub const THINKING: &str = "thinking";

pub const TALK_SYSTEM: &str = "You are the local model in Nook Code. In Nook Code the person picks a project folder; you read, search
and change a private copy of its files and can run the commands the project allows; every change is
shown to the person as a diff, and nothing reaches their files until they press Apply. Nothing is
committed.
Read the person's message. If it is a greeting, thanks, small talk, or a question about you (what you
are, what you can do, how to use you), reply with TALK: and then your answer in two or three friendly
sentences, without inventing features, and nothing after the answer. For anything else (a change to
make, something to look at or explain in the project, a question about its code), reply with the
single word WORK. A request to change, fix, add or address something is WORK even when it is polite
or thanks you. When unsure, reply WORK.";

/// Told to the triage call when the worker has the web tools, so "what can you do?" is answered
/// truly.
pub const TALK_WEB: &str = "\nWhen a request needs it, you can also search the web and read public pages over the person's own internet connection.";

static VERDICT_AFTER: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)\s*\n\s*(WORK|TALK)\s*[.!]?\s*$").expect("a valid pattern"));

/// An installed model the worker menu offers; `tested`: one the catalog marks as a worker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerChoice {
    pub id: String,
    pub name: String,
    pub tested: bool,
}

/// The speech model Nook installs for voice input: its catalog id, name and download size.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechModel {
    pub id: String,
    pub name: String,
    pub bytes: u64,
}

/// What the next request of a session starts with before the worker reads anything, estimated:
/// `tokens` of the `window` one request has, `recap` of them the earlier requests and what the
/// worker did, which every request of the session carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextContext {
    pub tokens: u32,
    pub recap: u32,
    pub window: u32,
}

/// What Code mode shows (`CodeHub.CodeSnapshot`), read again whenever a "code" event arrives.
/// `phases`: what each running turn is doing now, by run id ([`THINKING`] while the model thinks).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeSnapshot {
    pub sessions: Vec<CodeSession>,
    pub worker_name: Option<String>,
    pub worker_hint: String,
    pub workers: Vec<WorkerChoice>,
    pub worker_id: Option<String>,
    pub phases: BTreeMap<String, String>,
}

/// Which file is open in the Code page and what is selected, sent with a request from its Nook
/// panel (the original's `IdeWorkspace.decorate`): the file relative to the open folder, the
/// selected text, and its 1-based line range.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EditorContext {
    pub file: Option<String>,
    pub selection: Option<String>,
    pub lines: Option<(u32, u32)>,
}

/// The payload of a "code" event: the session that changed, or None when it is not one session
/// (the worker choice, a phase).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeChanged {
    pub session_id: Option<String>,
}

/// Code sessions and their turns. Cheap to clone: every clone is the same service.
#[derive(Clone)]
pub struct CodeService {
    inner: Arc<Inner>,
}

struct Inner {
    runtime: Arc<dyn WorkerRuntime>,
    home: Home,
    web: Arc<dyn Web>,
    web_enabled: Box<dyn Fn() -> bool + Send + Sync>,
    policy: PathPolicy,
    store: CodeStore,
    sessions: Mutex<HashMap<String, CodeSession>>,
    /// The running turn of each session: its run id and what stops it.
    stops: Mutex<HashMap<String, (String, CancellationToken)>>,
    /// What a running turn is doing now, for its live line: a step while it runs (copying,
    /// loading the model, a tool), THINKING while the model writes its next move. Steps alone
    /// cannot say it: the last one stays the last while the model spends a minute writing a file
    /// (2026-09-23).
    phases: Mutex<HashMap<String, String>>,
    /// Every edit of a session goes through here, from the turns and the UI alike (the original's
    /// `synchronized`).
    edits: Mutex<()>,
}

impl CodeService {
    /// The service over the runtime, with the worker's web through `web` (its Settings switch,
    /// `web.json`, read at each turn). Loads the saved sessions; a run that was in flight when Nook
    /// stopped is marked as ended.
    pub fn new(home: Home, runtime: Arc<dyn WorkerRuntime>, web: Arc<WebAccess>) -> CodeService {
        let switch = web.clone();
        CodeService::with_web(home, runtime, web, move || switch.enabled())
    }

    /// The same with any [`Web`] and a switch of its own (tests).
    pub fn with_web(
        home: Home,
        runtime: Arc<dyn WorkerRuntime>,
        web: Arc<dyn Web>,
        web_enabled: impl Fn() -> bool + Send + Sync + 'static,
    ) -> CodeService {
        let policy = PathPolicy::new(home.root());
        let store = CodeStore::new(home.code_dir().join("sessions"));
        let sessions = store
            .load_all()
            .into_iter()
            .map(|s| (s.id.clone(), settle(s)))
            .collect();
        CodeService {
            inner: Arc::new(Inner {
                runtime,
                home,
                web,
                web_enabled: Box::new(web_enabled),
                policy,
                store,
                sessions: Mutex::new(sessions),
                stops: Mutex::new(HashMap::new()),
                phases: Mutex::new(HashMap::new()),
                edits: Mutex::new(()),
            }),
        }
    }

    /// Stops every running turn (Nook is closing).
    pub fn shutdown(&self) {
        for (_, stop) in self.inner.stops.lock().values() {
            stop.cancel();
        }
    }

    // ------------------------------------------------------------------ reading

    /// Newest first.
    pub fn sessions(&self) -> Vec<CodeSession> {
        self.inner.sessions()
    }

    pub fn session(&self, id: &str) -> Option<CodeSession> {
        self.inner.session(id)
    }

    /// Everything Code mode shows (`CodeHub.read()`). Reads the models folder (cached for two
    /// seconds by the registry).
    pub fn snapshot(&self) -> CodeSnapshot {
        let worker = self.worker();
        CodeSnapshot {
            sessions: self.sessions(),
            worker_name: worker.as_ref().map(|w| w.display_name.clone()),
            worker_hint: self.worker_names(),
            workers: self.worker_choices(),
            worker_id: worker.map(|w| w.id),
            phases: self.phases(),
        }
    }

    /// The worker the next turn runs on (Settings > Models > Workers decides), if one is installed.
    pub fn worker(&self) -> Option<LocalModel> {
        self.inner.worker()
    }

    /// Every installed chat model can be the worker, the person's choice: the catalog's workers
    /// first (tested with Code's tools), in catalog order, then the rest as installed. Offering
    /// only the tested two hid four installed models from the menu (2026-09-24).
    pub fn worker_choices(&self) -> Vec<WorkerChoice> {
        let registry = self.inner.runtime.registry();
        let catalog = self.inner.runtime.catalog();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for c in catalog.worker_models() {
            if let Some(m) = registry.find(&c.id) {
                seen.insert(m.id.clone());
                out.push(WorkerChoice {
                    id: m.id,
                    name: m.display_name,
                    tested: true,
                });
            }
        }
        for m in registry.list() {
            if m.task == "chat" && seen.insert(m.id.clone()) {
                out.push(WorkerChoice {
                    id: m.id,
                    name: m.display_name,
                    tested: false,
                });
            }
        }
        out
    }

    /// Makes `model_id` the worker for the next request (Settings > Models > Workers shows the
    /// same choice).
    pub fn set_worker(&self, model_id: &str) -> Result<()> {
        if !self.worker_choices().iter().any(|w| w.id == model_id) {
            bail!("That model is not installed as a worker.");
        }
        self.inner
            .runtime
            .registry()
            .set_worker_preference(CODE_WORKER, Some(model_id))?;
        self.inner.changed(None);
        Ok(())
    }

    /// None when voice input can run, else what is missing (the speech model or its engine).
    pub fn speech_problem(&self) -> Option<String> {
        self.inner.runtime.speech_problem()
    }

    pub fn speech_model(&self) -> Option<SpeechModel> {
        let catalog = self.inner.runtime.catalog();
        let id = catalog.default_speech_model()?;
        let c = catalog.find(id)?;
        Some(SpeechModel {
            id: c.id.clone(),
            name: c.display_name.clone(),
            bytes: c.total_bytes(),
        })
    }

    /// Voice input: the recorded WAV as text, with the default speech model.
    pub async fn transcribe(&self, wav: &Path) -> Result<String> {
        Ok(self
            .inner
            .runtime
            .transcribe(wav, None)
            .await?
            .trim()
            .to_string())
    }

    /// Names of the worker models Nook knows, for a hint when none is installed.
    pub fn worker_names(&self) -> String {
        self.inner.worker_names()
    }

    /// How a session would work on `folder` (a git repository or a private copy), or why it
    /// cannot.
    pub async fn repository_state(&self, folder: &str) -> Result<RepositoryState> {
        let inside = self.inner.allowed(folder)?;
        Ok(code_workspace::state(&inside).await)
    }

    /// Repositories of earlier sessions, newest first.
    pub fn recent_repositories(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        self.sessions()
            .into_iter()
            .map(|s| s.repository)
            .filter(|r| seen.insert(r.clone()))
            .collect()
    }

    /// What a running turn is doing now.
    pub fn phase(&self, run_id: &str) -> Option<String> {
        self.inner.phases.lock().get(run_id).cloned()
    }

    /// Every running turn's phase, by run id: part of the UI's snapshot, so a new phase redraws it.
    pub fn phases(&self) -> BTreeMap<String, String> {
        self.inner
            .phases
            .lock()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    // ------------------------------------------------------------------ turns

    /// Starts a session on `folder` and runs its first turn: on the git repository at or above
    /// it, or on the folder itself through a private copy. `context`: the Code page's open file
    /// and selection, told to the worker with the request.
    pub async fn start(
        &self,
        folder: &str,
        text: &str,
        verify: Option<&str>,
        context: Option<&EditorContext>,
    ) -> Result<CodeSession> {
        self.start_from(Origin::Chat, folder, text, verify, context)
            .await
    }

    /// [`CodeService::start`] for a session started from `origin`: the Chat page, or the Nook
    /// panel beside the Code page's editor. The sidebar lists each page's own sessions.
    pub async fn start_from(
        &self,
        origin: Origin,
        folder: &str,
        text: &str,
        verify: Option<&str>,
        context: Option<&EditorContext>,
    ) -> Result<CodeSession> {
        let st = self.repository_state(folder).await?;
        if !st.usable() {
            bail!("{}", st.reason.unwrap_or_default());
        }
        let root = st
            .root
            .ok_or_else(|| anyhow!("{folder} is not there any more."))?;
        if self.inner.policy.is_denied(&root) {
            bail!("Nook may not work in {}.", root.display());
        }
        let now = now_millis();
        let id = short_id();
        let s = CodeSession {
            id: id.clone(),
            title: title_for(text),
            repository: root.to_string_lossy().into_owned(),
            created_at: now,
            updated_at: now,
            verify: blank_to_null(verify),
            origin,
            ..CodeSession::default()
        };
        self.inner.sessions.lock().insert(id.clone(), s);
        if let Err(e) = self.send(&id, text, verify, context).await {
            // Nothing ran: no session to keep.
            self.inner.sessions.lock().remove(&id);
            return Err(e);
        }
        self.session(&id)
            .ok_or_else(|| anyhow!("That session is gone."))
    }

    /// Asks the worker for the next change in session `id`. Returns once the turn is under way;
    /// it runs in the background and reports through "code" events.
    pub async fn send(
        &self,
        id: &str,
        text: &str,
        verify: Option<&str>,
        context: Option<&EditorContext>,
    ) -> Result<()> {
        let inner = &self.inner;
        let s = inner
            .session(id)
            .ok_or_else(|| anyhow!("That session is gone."))?;
        if s.running() {
            bail!("The worker is still busy with the last request. Stop it first or wait.");
        }
        if text.trim().is_empty() {
            bail!("Say what to change.");
        }
        let task = decorate(text, context).trim().to_string();
        let check = blank_to_null(verify);
        if let Some(check) = &check {
            let allow = VerifyCommands::for_repository(Path::new(&s.repository));
            if !allow.permits(check) {
                bail!(
                    "\"{check}\" is not a check this project allows. {}. Add it to nook.json in the project folder ({{\"verify\": [\"...\"]}}) or pick one of those.",
                    allow.describe()
                );
            }
        }
        let worker = inner.worker().ok_or_else(|| {
            anyhow!(
                "No worker model is installed. Download {} in Settings > Models.",
                inner.worker_names()
            )
        })?;
        let run_id = short_id();
        let task_entry = Entry::Task(Task::new(short_id(), now_millis(), task.clone()));
        let run = Entry::Run(Run::started(run_id.clone(), worker.display_name.clone()));
        let stop = CancellationToken::new();
        {
            // the check and the edit in one step, so two sends at once cannot both start a turn,
            // and a Stop pressed the moment the run shows finds its token
            let _edit = inner.edits.lock();
            let cur = inner
                .session(id)
                .ok_or_else(|| anyhow!("That session is gone."))?;
            if cur.running() {
                bail!("The worker is still busy with the last request. Stop it first or wait.");
            }
            let next = cur
                .with_verify(check.as_deref())
                .append(task_entry)
                .append(run);
            inner
                .stops
                .lock()
                .insert(id.to_string(), (run_id.clone(), stop.clone()));
            inner.sessions.lock().insert(id.to_string(), next.clone());
            inner.save(&next);
        }
        inner.changed(Some(id));
        tokio::spawn(
            inner
                .clone()
                .run_turn(id.to_string(), run_id, worker, task, check, stop),
        );
        Ok(())
    }

    /// Stops the session's running turn, if any.
    pub fn stop(&self, id: &str) {
        if let Some((_, stop)) = self.inner.stops.lock().get(id) {
            stop.cancel();
        }
    }

    /// The code one run wrote, as a diff from the tree before it to the tree after it; None when
    /// the run changed nothing, was undone, or ran before runs kept their end tree.
    pub async fn run_diff(&self, session_id: &str, run_id: &str) -> Option<String> {
        let s = self.session(session_id)?;
        let worktree = s.worktree.clone()?;
        let r = s.entries.iter().find_map(|e| match e {
            Entry::Run(r) if r.id == run_id => Some(r.clone()),
            _ => None,
        })?;
        if r.undone || r.running {
            return None;
        }
        let (before, after) = (r.before?, r.after?);
        if before == after {
            return None;
        }
        match code_workspace::git(
            Path::new(&worktree),
            60,
            &["diff", "--no-color", before.as_str(), after.as_str()],
        )
        .await
        {
            Ok(d) if !d.trim().is_empty() => Some(d),
            Ok(_) => None,
            Err(e) => {
                tracing::debug!("Could not read run {run_id}'s diff: {e}");
                None
            }
        }
    }

    /// The context the next request of session `id` would start with; None without a session or
    /// a worker.
    pub fn next_context(&self, id: &str) -> Option<NextContext> {
        let s = self.session(id)?;
        let worker = self.worker()?;
        let repo = PathBuf::from(&s.repository);
        let mut tools =
            WorkerTools::new(&repo, VerifyCommands::for_repository(&repo), HashMap::new());
        if (self.inner.web_enabled)() {
            tools = tools.with_web(WebTools::new(self.inner.web.clone()));
        }
        // The next request's brief: every request so far is an earlier one.
        let recap = brief(
            &s.append(Entry::Task(Task::new("next", now_millis(), ""))),
            "",
        );
        let tokens = estimate_tokens(&opening(
            tools.has_web(),
            &recap,
            Some(""),
            None,
            &tools.locked_patterns(),
            &repo,
        )) + estimate_tokens(&tools.definitions());
        Some(NextContext {
            tokens: tokens as u32,
            recap: (recap.chars().count() / 4) as u32,
            window: self.inner.window_for(&worker.id),
        })
    }

    // ------------------------------------------------------------------ apply, discard, delete

    /// Applies the change since the baseline to the person's working tree.
    pub async fn apply(&self, id: &str) -> Result<()> {
        let inner = &self.inner;
        let s = inner.require(id)?;
        if s.running() {
            bail!("Wait for the worker to finish.");
        }
        if s.worktree.is_none() {
            bail!("There is nothing to apply yet.");
        }
        // A copy that is gone (a cleaned temp folder, or a session imported from the Kotlin Nook,
        // whose copies stay in its own folder: crate::migrate) is made again first, with what was
        // applied and the pending change put back.
        let dir = inner.ensure_worktree(id).await?;
        let s = inner.require(id)?;
        let diff = code_workspace::diff(&dir, s.baseline.as_deref(), true).await?;
        if diff.trim().is_empty() {
            bail!("There is nothing new to apply.");
        }
        let files = code_workspace::files(&diff).len();
        let scratch = inner
            .home
            .temp_dir()
            .join("code")
            .join(format!("{id}-apply.patch"));
        if let Err(e) = code_workspace::apply(
            Path::new(&s.repository),
            &diff,
            &scratch,
            code_workspace::is_private_copy(&dir),
        )
        .await
        {
            inner.mutate(
                id,
                |cur| cur.append(note(&format!("Could not apply: {e}"), "error")),
                true,
            );
            return Err(e);
        }
        let tree = code_workspace::tree(&dir).await?;
        let text = format!(
            "Applied {files}{} to {}. Nothing was committed.",
            if files == 1 { " file" } else { " files" },
            s.repository
        );
        inner.mutate(
            id,
            |cur| {
                cur.with_baseline(Some(&tree))
                    .with_change(None)
                    .append(note(&text, APPLIED))
            },
            true,
        );
        Ok(())
    }

    /// Throws away what changed since the baseline.
    pub async fn discard(&self, id: &str) -> Result<()> {
        let inner = &self.inner;
        let s = inner.require(id)?;
        if s.running() {
            bail!("Stop the worker first.");
        }
        if let Some(w) = s.worktree.as_deref().filter(|w| Path::new(w).is_dir()) {
            code_workspace::reset(Path::new(w), s.baseline.as_deref()).await?;
        }
        let copy = s
            .worktree
            .as_deref()
            .is_some_and(|w| code_workspace::is_private_copy(Path::new(w)));
        let from = if s.baseline.is_some() {
            "what was last applied."
        } else if copy {
            "your files as they were when the session began."
        } else {
            "the repository's last commit."
        };
        let text = format!("Changes discarded. The next request starts from {from}");
        inner.mutate(
            id,
            |cur| cur.with_change(None).append(note(&text, DISCARDED)),
            true,
        );
        Ok(())
    }

    /// Takes back the last request: the scratch copy returns to the tree it held before that run.
    pub async fn undo(&self, id: &str) -> Result<()> {
        let inner = &self.inner;
        let s = inner.require(id)?;
        if s.running() {
            bail!("Stop the worker first.");
        }
        let (Some(r), true) = (s.undoable().cloned(), s.worktree.is_some()) else {
            bail!("There is no request to undo.");
        };
        // A copy that is gone is made again first, as for Apply.
        let dir = inner.ensure_worktree(id).await?;
        let s = inner.require(id)?;
        code_workspace::reset(&dir, r.before.as_deref()).await?;
        let change = change_of(&dir, s.baseline.as_deref()).await?;
        inner.mutate(
            id,
            |cur| {
                cur.with_change(change)
                    .edit_run(&r.id, Run::mark_undone)
                    .append(note(
                        "Undid the last request: the scratch copy is back to how it was before it.",
                        UNDONE,
                    ))
            },
            true,
        );
        Ok(())
    }

    /// Deletes a session: its record, and its scratch copy in the background.
    pub async fn delete(&self, id: &str) {
        let inner = &self.inner;
        let removed = {
            let _edit = inner.edits.lock();
            inner.sessions.lock().remove(id)
        };
        let Some(s) = removed else {
            return;
        };
        self.stop(id);
        if let Some(worktree) = s.worktree {
            let repo = s.repository;
            tokio::spawn(async move {
                code_workspace::remove(Path::new(&repo), Path::new(&worktree)).await;
            });
        }
        inner.store.delete(id);
        inner.changed(Some(id));
    }

    /// Gives a session another title; a blank one is ignored.
    pub fn rename(&self, id: &str, title: &str) {
        if title.trim().is_empty() {
            return;
        }
        self.inner
            .mutate(id, |cur| cur.with_title(title.trim()), true);
    }

    /// Marks the sessions `ids` as started beside the Code page's editor: those its Nook panel
    /// remembers (ide.json), from before sessions said where they were started.
    pub fn mark_editor_sessions<'a>(&self, ids: impl IntoIterator<Item = &'a str>) {
        for id in ids {
            if self.session(id).is_some_and(|s| s.origin != Origin::Editor) {
                self.inner.mutate(
                    id,
                    |cur| CodeSession {
                        origin: Origin::Editor,
                        ..cur.clone()
                    },
                    true,
                );
            }
        }
    }
}

impl BusyWork for CodeService {
    fn busy_with(&self) -> Option<String> {
        self.inner
            .sessions
            .lock()
            .values()
            .any(CodeSession::running)
            .then(|| "a Code session is working".to_string())
    }
}

impl Inner {
    fn sessions(&self) -> Vec<CodeSession> {
        let mut l: Vec<CodeSession> = self.sessions.lock().values().cloned().collect();
        l.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        l
    }

    fn session(&self, id: &str) -> Option<CodeSession> {
        self.sessions.lock().get(id).cloned()
    }

    fn require(&self, id: &str) -> Result<CodeSession> {
        self.session(id)
            .ok_or_else(|| anyhow!("That session is gone."))
    }

    fn worker(&self) -> Option<LocalModel> {
        self.runtime.registry().code_worker()
    }

    fn worker_names(&self) -> String {
        let names: Vec<String> = self
            .runtime
            .catalog()
            .worker_models()
            .iter()
            .map(|m| m.display_name.clone())
            .collect();
        if names.is_empty() {
            "a worker model".to_string()
        } else {
            names.join(" or ")
        }
    }

    fn allowed(&self, folder: &str) -> Result<PathBuf> {
        let inside = self.policy.check(folder, true)?;
        if self.policy.is_denied(&inside) {
            bail!("Nook may not work in {}.", inside.display());
        }
        Ok(inside)
    }

    /// The context one request gets: the engine's once the model runs, else what a load would
    /// plan (the catalog's default, else 8192).
    fn window_for(&self, model_id: &str) -> u32 {
        match self.runtime.context_per_request(model_id) {
            0 => self
                .runtime
                .catalog()
                .find(model_id)
                .map(|m| m.default_ctx())
                .unwrap_or(8192),
            n => n,
        }
    }

    fn changed(&self, session_id: Option<&str>) {
        events::emit(
            topic::CODE,
            CodeChanged {
                session_id: session_id.map(str::to_string),
            },
        );
    }

    fn save(&self, s: &CodeSession) {
        if let Err(e) = self.store.save(s) {
            tracing::warn!("Could not save code session {}: {e:#}", s.id);
        }
    }

    /// Edits session `id`: every change goes through here, from the turns and the UI alike.
    /// `persist` is false for the worker's progress steps, which the next saved change writes
    /// along.
    fn mutate(&self, id: &str, edit: impl FnOnce(&CodeSession) -> CodeSession, persist: bool) {
        {
            let _edit = self.edits.lock();
            let Some(cur) = self.session(id) else {
                return; // deleted meanwhile
            };
            let next = edit(&cur);
            self.sessions.lock().insert(id.to_string(), next.clone());
            if persist {
                self.save(&next);
            }
        }
        self.changed(Some(id));
    }

    /// A progress line of Nook's own, shown like the worker's.
    fn step(&self, id: &str, run_id: &str, text: &str) {
        self.phases
            .lock()
            .insert(run_id.to_string(), text.to_string());
        self.mutate(id, |cur| cur.edit_run(run_id, |r| r.with_step(text)), false);
    }

    fn thinking(&self, run_id: &str) {
        let before = self
            .phases
            .lock()
            .insert(run_id.to_string(), THINKING.to_string());
        if before.as_deref() != Some(THINKING) {
            self.changed(None);
        }
    }

    fn worktrees_dir(&self) -> PathBuf {
        self.home.temp_dir().join("code")
    }

    async fn run_turn(
        self: Arc<Self>,
        id: String,
        run_id: String,
        worker: LocalModel,
        text: String,
        verify: Option<String>,
        stop: CancellationToken,
    ) {
        let turn = self.turn(&id, &run_id, &worker, &text, verify.as_deref(), &stop);
        let failure = match AssertUnwindSafe(turn).catch_unwind().await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => {
                tracing::warn!("Code turn {run_id} in session {id} failed: {e:#}");
                Some(e.to_string())
            }
            Err(_) => {
                tracing::error!("Code turn {run_id} in session {id} panicked");
                Some("The turn failed unexpectedly.".to_string())
            }
        };
        if let Some(why) = failure {
            self.mutate(&id, |cur| cur.edit_run(&run_id, |r| r.failed(&why)), true);
        }
        {
            let mut stops = self.stops.lock();
            if stops.get(&id).is_some_and(|(r, _)| *r == run_id) {
                stops.remove(&id);
            }
        }
        if self.phases.lock().remove(&run_id).is_some() {
            self.changed(Some(&id));
        }
    }

    async fn turn(
        self: &Arc<Self>,
        id: &str,
        run_id: &str,
        worker: &LocalModel,
        text: &str,
        verify: Option<&str>,
        stop: &CancellationToken,
    ) -> Result<()> {
        let s = self.require(id)?;
        let repo = PathBuf::from(&s.repository);
        let started = Instant::now();
        // The first engine turn loads the model, which can take a minute; say so rather than look
        // stuck.
        if !self.runtime.is_loaded(&worker.id) {
            self.step(
                id,
                run_id,
                &format!("loading {} on the GPU", worker.display_name),
            );
        }
        // A greeting or a question about the assistant gets an answer, not a run: asked "what can
        // you do?", a worker with tools set off building files (2026-09-23).
        let answer = if may_be_talk(text) {
            tokio::select! {
                answer = self.talk_answer(worker, &repo, text, Some(run_id)) => answer,
                _ = stop.cancelled() => Ok(None),
            }
        } else {
            Ok(None)
        };
        let answer = match answer {
            Ok(answer) => answer,
            // The model did not load: the worker would only load it again. The run stops with
            // why, as the worker's loop stops for the same reason.
            Err(unavailable) => {
                let seconds = started.elapsed().as_secs();
                self.mutate(
                    id,
                    |cur| {
                        cur.edit_run(run_id, |r| Run {
                            running: false,
                            gave_up: stop_reason(Some(&unavailable.0)),
                            seconds,
                            ..r.clone()
                        })
                    },
                    true,
                );
                return Ok(());
            }
        };
        if let Some(answer) = answer {
            let seconds = started.elapsed().as_secs();
            self.mutate(
                id,
                |cur| {
                    cur.edit_run(run_id, |r| Run {
                        running: false,
                        summary: Some(answer),
                        stat: None,
                        verified: None,
                        verify_command: None,
                        verify_note: None,
                        verify_output: None,
                        gave_up: None,
                        tool_calls: 0,
                        seconds,
                        error: None,
                        undone: false,
                        after: None,
                        ..r.clone()
                    })
                },
                true,
            );
            return Ok(());
        }
        if s.worktree.is_none() {
            self.step(
                id,
                run_id,
                &format!("making a scratch copy of {}", file_name(&repo)),
            );
        }
        let dir = self.ensure_worktree(id).await?;
        // What Undo puts back if this turn makes things worse.
        let before = code_workspace::tree(&dir).await?;
        self.mutate(
            id,
            |cur| cur.edit_run(run_id, |r| r.with_before(&before)),
            true,
        );
        let s = self.require(id)?;
        let allow = VerifyCommands::for_repository(&repo);
        let env: HashMap<String, String> = jdk_locator::find_async(repo.clone())
            .await
            .map(|j| HashMap::from([("JAVA_HOME".to_string(), j.to_string_lossy().into_owned())]))
            .unwrap_or_default();
        let mut tools = WorkerTools::new(&dir, allow, env);
        let task = brief(&s, text);
        // Settings > Models > Workers > Web access, read each turn so a change applies to the next
        // request.
        if (self.web_enabled)() {
            let mut web_tools = WebTools::new(self.web.clone());
            // An address the person wrote may be opened; one in an earlier run's summary is the
            // worker's own words.
            for t in s.tasks() {
                web_tools.allow_from(&t.text);
            }
            tools = tools.with_web(web_tools);
            // So are the files of the change not applied yet: the worker wrote them.
            if let Some(c) = &s.change {
                tools = tools.written_before(code_workspace::files(&c.diff));
            }
        }
        // What a load would plan; once the model runs, the engine's own number
        // (Engine::context_tokens).
        let ctx = self.window_for(&worker.id);
        let effort = self
            .runtime
            .catalog()
            .find(&worker.id)
            .and_then(|m| m.defaults.get("reasoningEffort").cloned());
        let engine = TurnEngine {
            inner: self.clone(),
            worker_id: worker.id.clone(),
            worker_name: worker.display_name.clone(),
            run_id: run_id.to_string(),
            effort,
        };
        let out = WorkerLoop::new(
            &engine,
            &mut tools,
            Budget {
                max_tool_calls: TURN_TOOL_CALLS,
                max_seconds: TURN_SECONDS,
                context_tokens: ctx,
            },
        )
        .on_progress(|step| self.step(id, run_id, step))
        .cancelled_by(stop.clone())
        .on_context(|c| {
            self.mutate(
                id,
                |cur| {
                    cur.edit_run(run_id, |r| {
                        r.with_context(c.used, c.window, c.dropped, c.measured)
                    })
                },
                false,
            )
        })
        .run(&task, Some(""), Some(verify.unwrap_or("")), &dir)
        .await;
        let change = change_of(&dir, s.baseline.as_deref()).await?;
        let after = code_workspace::tree(&dir).await?;
        let verify_output = out.last_verification.as_deref().map(|tail| {
            let n = tail.chars().count();
            if n > 4000 {
                format!("…{}", tail.chars().skip(n - 4000).collect::<String>())
            } else {
                tail.to_string()
            }
        });
        self.mutate(
            id,
            |cur| {
                let stat = change.as_ref().map(|c| c.stat.clone());
                cur.with_change(change).edit_run(run_id, |r| Run {
                    running: false,
                    summary: Some(out.summary.clone()),
                    stat,
                    verified: verify.map(|_| out.verified),
                    verify_command: out.verify_command.clone(),
                    verify_note: Some(out.verify_note.clone()),
                    verify_output,
                    gave_up: stop_reason(out.gave_up.as_deref()),
                    tool_calls: out.tool_calls,
                    seconds: out.seconds,
                    error: None,
                    undone: false,
                    after: Some(after),
                    ..r.clone()
                })
            },
            true,
        );
        Ok(())
    }

    /// The answer to a greeting or a question about the assistant, or None when the message is
    /// work for the worker. One short call without tools, on the worker model; any failure means
    /// work, except the model not loading, which the worker would meet again: that is the error.
    async fn talk_answer(
        &self,
        worker: &LocalModel,
        repo: &Path,
        text: &str,
        run_id: Option<&str>,
    ) -> std::result::Result<Option<String>, EngineUnavailable> {
        let system = format!(
            "{TALK_SYSTEM}{}\nThe project folder is {}.",
            if (self.web_enabled)() { TALK_WEB } else { "" },
            file_name(repo)
        );
        let mut body = json!({
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": text },
            ],
            "temperature": 0.2,
            "max_tokens": 600,
        });
        thinking::apply(&mut body, &worker.id, false);
        let lease = match lease_for(self.runtime.as_ref(), &worker.id, &worker.display_name).await {
            Ok(lease) => lease,
            Err(e) => {
                tracing::debug!("Could not sort the message; it goes to the worker: {e}");
                return match e.downcast::<EngineUnavailable>() {
                    Ok(unavailable) => Err(unavailable),
                    Err(_) => Ok(None),
                };
            }
        };
        if let Some(r) = run_id {
            self.thinking(r);
        }
        match lease.client().chat_text(body).await {
            Ok(content) => Ok(talk_reply(Some(&content))),
            Err(e) => {
                tracing::debug!("Could not sort the message; it goes to the worker: {e}");
                Ok(None)
            }
        }
    }

    /// The scratch copy for session `id`, made on first use and made again if it vanished.
    async fn ensure_worktree(&self, id: &str) -> Result<PathBuf> {
        let s = self.require(id)?;
        if let Some(w) = s.worktree.as_deref().filter(|w| Path::new(w).is_dir()) {
            return Ok(PathBuf::from(w));
        }
        let repo = PathBuf::from(&s.repository);
        let c = code_workspace::create(&repo, &self.worktrees_dir().join(id)).await?;
        let mut next = s.with_worktree(&c.dir.to_string_lossy(), &c.head);
        let mut failure = None;
        if s.worktree.is_some() {
            let r = recover(
                &s,
                next,
                &c,
                &self.home.code_dir().join("unrecovered"),
                &self.home.temp_dir().join("code"),
            )
            .await?;
            next = r.session;
            failure = r.failure;
        }
        self.mutate(id, |_| next, true);
        if let Some(f) = failure {
            bail!("{f}");
        }
        Ok(c.dir)
    }
}

/// A lease on the worker model for one request, at interactive priority (the person is waiting,
/// so it goes ahead of agents' background work). A failure to load the model is an
/// [`EngineUnavailable`] with the reason: loading it again fails the same way. The model being
/// unloaded or reloaded while the request waited is not; the next try gets the engine there is.
async fn lease_for(
    runtime: &dyn WorkerRuntime,
    model_id: &str,
    name: &str,
) -> Result<Box<dyn Lease>> {
    match runtime.acquire(model_id, Priority::Interactive).await {
        Ok(lease) => Ok(lease),
        Err(e) if e.is::<AdmissionError>() => Err(e),
        Err(e) => Err(unavailable(name, &e).into()),
    }
}

/// Why the worker model did not load, for the person: the runtime's own sentence when it wrote
/// one ([`ModelLoadError`]), else the first line of what it said (an engine's log tail follows).
fn unavailable(name: &str, e: &anyhow::Error) -> EngineUnavailable {
    if let Some(load) = e.downcast_ref::<ModelLoadError>() {
        return EngineUnavailable(load.message.clone());
    }
    let said = e.to_string();
    let first = said.lines().next().unwrap_or("").trim();
    let mut chars = first.chars();
    let first = match (chars.next(), chars.next()) {
        // "Engine exited ..." reads on as "engine exited ..."; "CUDA ..." stays as it is.
        (Some(a), Some(b)) if a.is_uppercase() && b.is_lowercase() => {
            format!("{}{}", a.to_lowercase(), &first[a.len_utf8()..])
        }
        _ => first.to_string(),
    };
    EngineUnavailable(format!("{name} could not be loaded: {first}"))
}

/// The worker model as the loop's engine: each turn holds the model loaded for one request, at
/// interactive priority.
struct TurnEngine {
    inner: Arc<Inner>,
    worker_id: String,
    worker_name: String,
    run_id: String,
    effort: Option<String>,
}

#[async_trait]
impl Engine for TurnEngine {
    async fn chat(
        &self,
        messages: &[Value],
        tools: &[Value],
        temperature: f64,
        max_tokens: u32,
    ) -> Result<Value> {
        let mut body = json!({
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto",
            "temperature": temperature,
            "max_tokens": max_tokens,
        });
        if let Some(effort) = &self.effort {
            body["chat_template_kwargs"] = json!({ "reasoning_effort": effort });
        }
        // The person is waiting on this, so it goes ahead of agents' background work.
        let lease = lease_for(
            self.inner.runtime.as_ref(),
            &self.worker_id,
            &self.worker_name,
        )
        .await?;
        self.inner.thinking(&self.run_id);
        lease.client().chat(body).await
    }

    fn context_tokens(&self) -> u32 {
        if self.inner.runtime.is_loaded(&self.worker_id) {
            self.inner.runtime.context_per_request(&self.worker_id)
        } else {
            0
        }
    }
}

// ---------------------------------------------------------------------- the parts, testable

/// A request with what the Code page's editor shows added: the open file and, when there is
/// one, the selection with its lines (`IdeWorkspace.decorate`).
pub fn decorate(text: &str, context: Option<&EditorContext>) -> String {
    let Some(ctx) = context else {
        return text.to_string();
    };
    let Some(file) = ctx.file.as_deref().filter(|f| !f.trim().is_empty()) else {
        return text.to_string();
    };
    let mut sb = text.trim_end().to_string();
    sb.push_str(&format!(
        "\n\nContext from the editor: the open file is {}.",
        file.replace('\\', "/")
    ));
    let selection = ctx.selection.as_deref().filter(|s| !s.trim().is_empty());
    if let (Some((first, last)), Some(sel)) = (ctx.lines, selection) {
        let sel: String = sel.chars().take(4000).collect();
        sb.push_str(&format!(
            " The selected text, lines {first}–{last}:\n```\n{}\n```",
            sel.trim_end()
        ));
    }
    sb
}

/// Whether a message is worth the triage call: a greeting or a question about the assistant is
/// short.
pub fn may_be_talk(text: &str) -> bool {
    let t = text.trim();
    t.chars().count() <= 300 && t.lines().count() <= 3
}

/// The answer after "TALK:", or None for WORK or a reply in neither shape. Reasoning the engine
/// leaves inline goes first: Qwen3's `<think>` and gpt-oss's analysis channel, which gpt-oss sends
/// even at low effort, before the verdict.
pub fn talk_reply(content: Option<&str>) -> Option<String> {
    let c = strip_thinking(content?);
    let at = c.find("TALK:")?;
    if !c[..at].trim().is_empty() {
        return None;
    }
    let answer = c[at + "TALK:".len()..].trim();
    // A verdict word the model added after the answer is not part of it.
    let answer = VERDICT_AFTER.replace_all(answer, "").trim().to_string();
    (!answer.is_empty()).then_some(answer)
}

/// Why the worker stopped, in words for the person. The engine's own error can run to pages (a
/// gpt-oss tool call llama-server could not parse came back as 8 kB of escaped newlines on
/// 2026-09-23); the log keeps it whole. Replies the engine refused are one reason; a model that
/// did not load says its own ("X can't run in Nook: ..."), without the full stop the session adds.
pub fn stop_reason(gave_up: Option<&str>) -> Option<String> {
    let g = gave_up?;
    Some(if g == "cancelled" {
        "stopped".to_string()
    } else if g == "context" {
        "its context was full, even with the oldest file reads and command output dropped; ask for a smaller step".to_string()
    } else if g.starts_with("engine error") {
        "the local model's replies could not be read (the engine refused them several times in a row); ask again or undo this request".to_string()
    } else if g.chars().count() > 200 {
        format!("{}...", g.chars().take(197).collect::<String>())
    } else {
        g.strip_suffix('.').unwrap_or(g).to_string()
    })
}

/// What the worker is told. The first turn is the request; later ones recall the earlier
/// requests and answers, because each turn is a fresh conversation over the same scratch copy.
pub fn brief(s: &CodeSession, text: &str) -> String {
    let es = &s.entries;
    if s.tasks().len() <= 1 {
        return text.to_string();
    }
    let Some(current) = es.iter().rposition(|e| matches!(e, Entry::Task(_))) else {
        return text.to_string();
    };
    let discarded = discarded(es, current);
    let mut sb = String::from("This continues earlier work in the same copy of the repository; the changes from the earlier requests are already in the files.\n");
    for i in 0..current {
        let Entry::Task(t) = &es[i] else {
            continue;
        };
        // A request the person took back (Undo, or Discard since the last Apply) is not in the
        // files; it is not mentioned.
        let mut undone = discarded.contains(&i);
        for e in es.iter().take(current).skip(i + 1) {
            match e {
                Entry::Task(_) => break,
                Entry::Run(r) if r.undone => undone = true,
                _ => {}
            }
        }
        if undone {
            continue;
        }
        sb.push_str("\nEarlier request: ");
        sb.push_str(t.text.trim());
        for e in es.iter().take(current).skip(i + 1) {
            match e {
                Entry::Task(_) => break,
                Entry::Run(r) => {
                    if let Some(summary) = r.summary.as_deref().filter(|s| !s.trim().is_empty()) {
                        sb.push_str("\nWhat you did: ");
                        sb.push_str(summary.trim());
                    }
                }
                _ => {}
            }
        }
        sb.push('\n');
    }
    sb.push_str("\nNow: ");
    sb.push_str(text);
    sb
}

/// The requests (entry indexes before `end`) a Discard threw away: Discard puts the copy back to
/// what was last applied, so every request since the Apply before it is gone.
pub fn discarded(es: &[Entry], end: usize) -> HashSet<usize> {
    let mut gone = HashSet::new();
    let mut since = Vec::new();
    for (i, e) in es.iter().enumerate().take(end) {
        match e {
            Entry::Task(_) => since.push(i),
            Entry::Note(n) if n.tone.as_deref() == Some(APPLIED) => since.clear(),
            Entry::Note(n) if n.tone.as_deref() == Some(DISCARDED) => {
                gone.extend(since.drain(..));
            }
            _ => {}
        }
    }
    gone
}

/// The scratch copy's change against `baseline`, or None when it holds none.
pub async fn change_of(dir: &Path, baseline: Option<&str>) -> Result<Option<Change>> {
    let diff = code_workspace::diff(dir, baseline, false).await?;
    if diff.trim().is_empty() {
        return Ok(None);
    }
    let cut = diff.chars().count() > MAX_STORED_DIFF;
    let diff = if cut {
        diff.chars().take(MAX_STORED_DIFF).collect()
    } else {
        diff
    };
    Ok(Some(Change {
        diff,
        cut,
        stat: code_workspace::stat(dir, baseline).await?,
    }))
}

/// A lost copy made again: the session to keep, and why the turn must stop (None when the work
/// came back).
pub struct Recovery {
    pub session: CodeSession,
    pub failure: Option<String>,
}

/// Puts back what a lost scratch copy held (a cleaned temp folder): first what was last applied,
/// then the pending change on top. The applied part is the baseline tree itself when the
/// repository still has it; a private copy's record went with it, so there the person's files,
/// which Apply made that way, become the baseline. When the pending change cannot come back it is
/// saved to `unrecovered` and the turn stops, so no run goes on without it unnoticed.
pub async fn recover(
    old: &CodeSession,
    mut next: CodeSession,
    c: &Created,
    unrecovered: &Path,
    scratch: &Path,
) -> Result<Recovery> {
    let dir = &c.dir;
    let copy = code_workspace::is_private_copy(dir);
    let mut from = c.from.clone();
    if let Some(baseline) = old.baseline.as_deref() {
        if !copy && code_workspace::has_tree(dir, baseline).await {
            code_workspace::reset(dir, Some(baseline)).await?;
            next = next.with_baseline(Some(baseline));
            from = "what was last applied".to_string();
        } else if copy {
            let tree = code_workspace::tree(dir).await?;
            next = next.with_baseline(Some(&tree));
        } else {
            return lost(
                next,
                old.change.as_ref(),
                unrecovered,
                "what was last applied is no longer in the repository",
            );
        }
    }
    let Some(pending) = old.change.as_ref().filter(|p| !p.diff.trim().is_empty()) else {
        let text = format!("The scratch copy was gone; Nook made a new one from {from}.");
        return Ok(Recovery {
            session: next.append(note(&text, "info")),
            failure: None,
        });
    };
    if pending.cut {
        return lost(
            next,
            Some(pending),
            unrecovered,
            "the pending change was too large to keep whole",
        );
    }
    let patch = scratch.join(format!("{}-restore.patch", old.id));
    if code_workspace::apply(dir, &pending.diff, &patch, copy)
        .await
        .is_err()
    {
        return lost(
            next,
            Some(pending),
            unrecovered,
            "the pending change does not fit your files as they are now",
        );
    }
    let text = format!(
        "The scratch copy was gone; Nook made a new one from {from} and put the pending change back."
    );
    Ok(Recovery {
        session: next.append(note(&text, "info")),
        failure: None,
    })
}

/// The pending change could not come back: it is saved, the session says where, and the turn
/// stops.
fn lost(
    next: CodeSession,
    pending: Option<&Change>,
    unrecovered: &Path,
    why: &str,
) -> Result<Recovery> {
    let mut saved = String::new();
    if let Some(p) = pending.filter(|p| !p.diff.trim().is_empty()) {
        std::fs::create_dir_all(unrecovered)?;
        let f = unrecovered.join(format!("{}-{}.patch", next.id, now_millis()));
        std::fs::write(&f, p.diff.as_bytes())?;
        saved = format!(
            " The pending change is saved in {}{}",
            f.display(),
            if p.cut {
                " (cut short, as it was stored)."
            } else {
                "."
            }
        );
    }
    let text = format!(
        "The scratch copy was gone and {why}.{saved} Send again to go on from a fresh copy without it, or Discard."
    );
    Ok(Recovery {
        session: next.append(note(&text, "error")),
        failure: Some(text),
    })
}

/// A session read from disk: a run that was in flight when Nook stopped is over.
fn settle(s: CodeSession) -> CodeSession {
    let running: Vec<String> = s
        .entries
        .iter()
        .filter_map(|e| match e {
            Entry::Run(r) if r.running => Some(r.id.clone()),
            _ => None,
        })
        .collect();
    running.iter().fold(s, |out, id| {
        out.edit_run(id, |r| r.failed("Nook closed before this run finished."))
    })
}

fn note(text: &str, tone: &str) -> Entry {
    Entry::Note(Note {
        id: short_id(),
        at: now_millis(),
        text: text.to_string(),
        tone: Some(tone.to_string()),
    })
}

/// A session's title: the request's first line, cut to sixty characters.
pub fn title_for(text: &str) -> String {
    let mut t = text.trim();
    if let Some(nl) = t.find('\n') {
        t = t[..nl].trim();
    }
    if t.is_empty() {
        "New session".to_string()
    } else if t.chars().count() > 60 {
        format!("{}...", t.chars().take(57).collect::<String>())
    } else {
        t.to_string()
    }
}

fn blank_to_null(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Eight hex characters of a random UUID, as the original's ids.
fn short_id() -> String {
    uuid::Uuid::new_v4().to_string()[..8].to_string()
}

/// A folder's own name (the whole path for a drive).
fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

#[cfg(test)]
#[path = "code_service_tests.rs"]
mod tests;

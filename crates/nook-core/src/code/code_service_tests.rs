//! CodeService: its pure parts (the brief, the triage reply, stop reasons, recovery of a lost
//! copy; ported from CodeWorkspaceTest) and whole turns against a fake runtime whose engine is a
//! local HTTP server answering from a script.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};

use super::*;
use crate::code::code_session::tests::{note as note_entry, run, session};
use crate::code::code_workspace::tests::{read, repo};
use crate::code::code_workspace::{create, diff, files, remove, tree};
use crate::runtime::api::Lease;
use crate::runtime::downloader::tests::serve;
use crate::runtime::inference_client::InferenceClient;
use crate::runtime::model_catalog::ModelCatalog;
use crate::runtime::model_registry::ModelRegistry;
use crate::runtime::Downloader;
use crate::worker::web_tools::tests::FakeWeb;
use crate::worker::worker_loop::tests::{reply, tool_call};

// ---------------------------------------------------------------------- the pure parts

#[test]
fn later_turns_recall_earlier_requests() {
    let s = session(vec![
        Entry::Task(Task::new("t1", 1, "Add a greeting to the CLI")),
        Entry::Run(run("u1", "Added greet() and a test.")),
        Entry::Task(Task::new("t0", 1, "Rename the module")),
        Entry::Run(run("u0", "Renamed it.").mark_undone()),
        Entry::Task(Task::new("t2", 1, "Handle the empty name")),
        Entry::Run(Run::started("u2", "gpt-oss")),
    ]);
    let b = brief(&s, "Handle the empty name");
    assert!(
        b.contains("Earlier request: Add a greeting to the CLI"),
        "{b}"
    );
    assert!(b.contains("What you did: Added greet() and a test."), "{b}");
    assert!(b.ends_with("Now: Handle the empty name"), "{b}");
    assert!(!b.contains("Earlier request: Handle the empty name"), "{b}");
    assert!(
        !b.contains("Rename the module"),
        "a request the person undid is not in the files: {b}"
    );

    let first = session(vec![
        Entry::Task(Task::new("t1", 1, "Add a greeting")),
        Entry::Run(Run::started("u1", "gpt-oss")),
    ]);
    assert_eq!("Add a greeting", brief(&first, "Add a greeting"));
}

#[test]
fn a_discarded_request_is_not_told_to_the_next_turn_as_done() {
    let s = session(vec![
        Entry::Task(Task::new("t1", 1, "Add a CLI")),
        Entry::Run(run("u1", "Added the CLI.")),
        note_entry("n1", "Applied 1 file.", APPLIED),
        Entry::Task(Task::new("t2", 1, "Add a greeting feature")),
        Entry::Run(run("u2", "Added the greeting feature.")),
        note_entry("n2", "Changes discarded.", DISCARDED),
        Entry::Task(Task::new("t3", 1, "What does main.py do now?")),
        Entry::Run(Run::started("u3", "gpt-oss")),
    ]);
    let b = brief(&s, "What does main.py do now?");
    assert!(
        b.contains("Earlier request: Add a CLI"),
        "applied before the discard, so still in the files: {b}"
    );
    assert!(
        !b.contains("greeting"),
        "QA-04: discarded, so not in the files: {b}"
    );
}

#[test]
fn engine_errors_read_as_a_sentence() {
    let raw = format!(
        "engine error after 5 attempts: Engine returned HTTP 500: {{\"error\":{}",
        "\\n    ".repeat(2000)
    );
    let said = stop_reason(Some(&raw)).unwrap();
    assert!(said.chars().count() < 200, "{said}");
    assert!(said.contains("could not be read"), "{said}");
    // A model that did not load says why itself; the session adds the full stop.
    assert_eq!(
        Some("X can't run in Nook: the engine (llama.cpp b10752) doesn't know its model architecture 'y'".to_string()),
        stop_reason(Some("X can't run in Nook: the engine (llama.cpp b10752) doesn't know its model architecture 'y'."))
    );
    assert_eq!(Some("stopped".to_string()), stop_reason(Some("cancelled")));
    assert_eq!(
        Some("tool-call budget".to_string()),
        stop_reason(Some("tool-call budget"))
    );
    assert!(stop_reason(Some("context"))
        .unwrap()
        .starts_with("its context was full"));
    assert_eq!(None, stop_reason(None));
    let long = "x".repeat(250);
    assert_eq!(200, stop_reason(Some(&long)).unwrap().chars().count());
}

#[test]
fn a_question_about_the_assistant_is_answered_and_everything_else_is_work() {
    let t = |s: &str| talk_reply(Some(s));
    assert_eq!(
        Some("I read and change this project's files.".to_string()),
        t("TALK: I read and change this project's files.")
    );
    assert_eq!(
        Some("Hi! What should I build?".to_string()),
        t("<think>a greeting</think>\nTALK:\nHi! What should I build?")
    );
    assert_eq!(None, t("WORK"));
    assert_eq!(
        Some("Hi there! How can I help?".to_string()),
        t("TALK: Hi there! How can I help?\nWORK"),
        "a verdict word after the answer is not shown"
    );
    assert_eq!(
        Some("I read and change files.".to_string()),
        t("<|channel|>analysis<|message|>A greeting: TALK.<|end|>TALK: I read and change files."),
        "gpt-oss puts its reasoning before the verdict"
    );
    assert_eq!(
        None,
        t("<|channel|>analysis<|message|>A change to make.<|end|>WORK")
    );
    assert_eq!(
        None,
        t("Sure, I will add the function. TALK: no"),
        "TALK: must lead the reply"
    );
    assert_eq!(None, t("TALK:"));
    assert_eq!(None, talk_reply(None));
}

#[test]
fn only_short_messages_go_through_the_triage() {
    assert!(may_be_talk("Hey"));
    assert!(may_be_talk("What can you do?"));
    assert!(!may_be_talk(
        "Add a withdraw function.\n- owner only\n- emit an event\n- zero the fees first"
    ));
    assert!(!may_be_talk(&"x".repeat(301)));
}

#[test]
fn a_request_from_the_code_page_says_which_file_is_open() {
    assert_eq!("Fix it", decorate("Fix it", None));
    let file_only = EditorContext {
        file: Some("src/app.py".into()),
        selection: None,
        lines: None,
    };
    assert_eq!(
        "Fix it\n\nContext from the editor: the open file is src/app.py.",
        decorate("Fix it  \n", Some(&file_only))
    );
    let selected = EditorContext {
        file: Some("src\\app.py".into()),
        selection: Some("def main():\n    pass\n".into()),
        lines: Some((3, 4)),
    };
    assert_eq!(
        "Fix it\n\nContext from the editor: the open file is src/app.py. The selected text, lines 3–4:\n```\ndef main():\n    pass\n```",
        decorate("Fix it", Some(&selected))
    );
    let long = EditorContext {
        file: Some("a.txt".into()),
        selection: Some("y".repeat(5000)),
        lines: Some((1, 1)),
    };
    assert!(decorate("x", Some(&long)).contains(&format!("\n{}\n```", "y".repeat(4000))));
    assert_eq!(
        "x",
        decorate("x", Some(&EditorContext::default())),
        "no file open: nothing to say"
    );
    let from_ui: EditorContext =
        serde_json::from_str(r#"{"file":"a.rs","selection":"b","lines":[2,5]}"#).unwrap();
    assert_eq!(Some((2, 5)), from_ui.lines);
}

#[test]
fn titles_are_the_first_line_cut_short() {
    assert_eq!("New session", title_for("  "));
    assert_eq!("Add a thing", title_for("Add a thing\nwith details"));
    let t = title_for(&"w".repeat(80));
    assert_eq!(60, t.chars().count());
    assert!(t.ends_with("..."));
}

// ---------------------------------------------------------------------- recovery

fn lost_session(
    id: &str,
    repository: &Path,
    c: &Created,
    baseline: Option<String>,
    change: Option<Change>,
) -> CodeSession {
    CodeSession {
        id: id.into(),
        title: "t".into(),
        repository: repository.to_string_lossy().into_owned(),
        created_at: 1,
        updated_at: 1,
        worktree: Some(c.dir.to_string_lossy().into_owned()),
        base_commit: Some(c.head.clone()),
        baseline,
        verify: None,
        change,
        entries: Vec::new(),
        origin: Origin::Chat,
    }
}

#[tokio::test]
async fn a_lost_private_copy_comes_back_with_what_was_applied_and_what_is_pending() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("plain");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("A.txt"), "original\n").unwrap();
    let scratch = tmp.path().join("scratch").join("s9");
    let c = create(&folder, &scratch).await.unwrap();
    std::fs::write(c.dir.join("A.txt"), "applied version\n").unwrap();
    let d = diff(&c.dir, None, true).await.unwrap();
    code_workspace::apply(&folder, &d, &tmp.path().join("a.patch"), true)
        .await
        .unwrap();
    let baseline = tree(&c.dir).await.unwrap();
    std::fs::write(c.dir.join("A.txt"), "pending work after apply\n").unwrap();
    let change = change_of(&c.dir, Some(&baseline)).await.unwrap();
    let old = lost_session("s9", &folder, &c, Some(baseline), change);
    remove(&folder, &c.dir).await; // a cleaned temp folder

    let again = create(&folder, &scratch).await.unwrap();
    let r = recover(
        &old,
        old.with_worktree(&again.dir.to_string_lossy(), &again.head),
        &again,
        &tmp.path().join("unrecovered"),
        &tmp.path().join("tmp"),
    )
    .await
    .unwrap();
    assert_eq!(None, r.failure);
    assert_eq!(
        "pending work after apply\n",
        read(&again.dir.join("A.txt")),
        "QA-01: the pending work is back"
    );
    let next = diff(&again.dir, r.session.baseline.as_deref(), false)
        .await
        .unwrap();
    assert!(
        next.contains("-applied version") && next.contains("+pending work after apply"),
        "Apply would bring only what is pending: {next}"
    );
    remove(&folder, &again.dir).await;
}

#[tokio::test]
async fn a_deleted_worktree_is_made_again_and_its_work_comes_back() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(&tmp.path().join("repo")).await;
    let scratch = tmp.path().join("scratch").join("s10");
    let c = create(&repo, &scratch).await.unwrap();
    std::fs::write(c.dir.join("A.txt"), "one\napplied\n").unwrap();
    let d = diff(&c.dir, None, true).await.unwrap();
    code_workspace::apply(&repo, &d, &tmp.path().join("a.patch"), false)
        .await
        .unwrap();
    let baseline = tree(&c.dir).await.unwrap();
    std::fs::write(c.dir.join("B.txt"), "pending\n").unwrap();
    let change = change_of(&c.dir, Some(&baseline)).await.unwrap();
    let old = lost_session("s10", &repo, &c, Some(baseline.clone()), change);
    code_workspace::delete_tree(&c.dir).unwrap(); // the folder goes; git still has it registered

    let again = create(&repo, &scratch).await.unwrap(); // QA-03: git refused this before
    let r = recover(
        &old,
        old.with_worktree(&again.dir.to_string_lossy(), &again.head),
        &again,
        &tmp.path().join("unrecovered"),
        &tmp.path().join("tmp"),
    )
    .await
    .unwrap();
    assert_eq!(None, r.failure);
    assert_eq!(
        Some(baseline),
        r.session.baseline,
        "the applied tree is still in the repository"
    );
    assert_eq!("one\napplied\n", read(&again.dir.join("A.txt")));
    assert_eq!("pending\n", read(&again.dir.join("B.txt")));
    let next = diff(&again.dir, r.session.baseline.as_deref(), false)
        .await
        .unwrap();
    assert_eq!(vec!["B.txt"], files(&next));
    remove(&repo, &again.dir).await;
}

#[tokio::test]
async fn pending_work_that_cannot_come_back_is_saved_and_the_turn_stops() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("plain");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("A.txt"), "original\n").unwrap();
    let scratch = tmp.path().join("scratch").join("s11");
    let c = create(&folder, &scratch).await.unwrap();
    std::fs::write(c.dir.join("A.txt"), "pending\n").unwrap();
    let change = change_of(&c.dir, None).await.unwrap();
    let old = lost_session("s11", &folder, &c, None, change);
    remove(&folder, &c.dir).await;
    std::fs::write(folder.join("A.txt"), "edited by the person meanwhile\n").unwrap();

    let again = create(&folder, &scratch).await.unwrap();
    let r = recover(
        &old,
        old.with_worktree(&again.dir.to_string_lossy(), &again.head),
        &again,
        &tmp.path().join("unrecovered"),
        &tmp.path().join("tmp"),
    )
    .await
    .unwrap();
    let failure = r.failure.clone().expect("not silent");
    let patch = std::fs::read_dir(tmp.path().join("unrecovered"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(read(&patch).contains("+pending"), "the work is kept");
    assert!(failure.contains(&patch.display().to_string()), "{failure}");
    match r.session.entries.last() {
        Some(Entry::Note(n)) => assert_eq!(Some("error"), n.tone.as_deref()),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        old.change, r.session.change,
        "the pending record stays until the person decides"
    );
    remove(&folder, &again.dir).await;
}

// ---------------------------------------------------------------------- whole turns

/// What the fake engine answers: the worker's turns (requests with tools) and the triage's
/// (requests without), in order; "hang" makes a worker turn wait a long time.
#[derive(Default)]
struct Script {
    worker: Mutex<VecDeque<Value>>,
    triage: Mutex<VecDeque<String>>,
    bodies: Mutex<Vec<Value>>,
}

impl Script {
    fn worker(&self, turn: Value) {
        self.worker.lock().push_back(turn);
    }

    fn triage(&self, answer: &str) {
        self.triage.lock().push_back(answer.to_string());
    }

    fn worker_bodies(&self) -> Vec<Value> {
        self.bodies
            .lock()
            .iter()
            .filter(|b| b.get("tools").is_some())
            .cloned()
            .collect()
    }

    fn triage_bodies(&self) -> Vec<Value> {
        self.bodies
            .lock()
            .iter()
            .filter(|b| b.get("tools").is_none())
            .cloned()
            .collect()
    }
}

async fn engine(script: Arc<Script>) -> String {
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let script = script.clone();
            async move {
                script.bodies.lock().push(body.clone());
                if body.get("tools").is_some() {
                    let turn = script
                        .worker
                        .lock()
                        .pop_front()
                        .unwrap_or_else(|| json!("Done."));
                    if turn == json!("hang") {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                    }
                    Json(reply(turn))
                } else {
                    let answer = script
                        .triage
                        .lock()
                        .pop_front()
                        .unwrap_or_else(|| "WORK".to_string());
                    Json(
                        json!({"choices": [{"message": {"role": "assistant", "content": answer}}]}),
                    )
                }
            }
        }),
    );
    serve(router).await
}

struct FakeLease(InferenceClient);

impl Lease for FakeLease {
    fn client(&self) -> &InferenceClient {
        &self.0
    }
}

struct FakeRuntime {
    registry: Arc<ModelRegistry>,
    catalog: Arc<ModelCatalog>,
    base: String,
    loaded: AtomicBool,
    /// What the next leases fail with, one each, before they succeed again.
    failures: Mutex<VecDeque<anyhow::Error>>,
    /// Leases asked for.
    asked: AtomicUsize,
}

#[async_trait]
impl WorkerRuntime for FakeRuntime {
    fn registry(&self) -> Arc<ModelRegistry> {
        self.registry.clone()
    }

    fn catalog(&self) -> Arc<ModelCatalog> {
        self.catalog.clone()
    }

    fn is_loaded(&self, _model_id: &str) -> bool {
        self.loaded.load(Ordering::SeqCst)
    }

    fn context_per_request(&self, _model_id: &str) -> u32 {
        if self.loaded.load(Ordering::SeqCst) {
            6144
        } else {
            0
        }
    }

    async fn acquire(&self, _model_id: &str, priority: Priority) -> Result<Box<dyn Lease>> {
        assert_eq!(Priority::Interactive, priority, "the person is waiting");
        self.asked.fetch_add(1, Ordering::SeqCst);
        if let Some(e) = self.failures.lock().pop_front() {
            return Err(e);
        }
        self.loaded.store(true, Ordering::SeqCst);
        Ok(Box::new(FakeLease(InferenceClient::new(&self.base, None))))
    }

    fn speech_problem(&self) -> Option<String> {
        Some("The speech model is not installed.".to_string())
    }

    async fn transcribe(&self, _wav: &Path, model_id: Option<&str>) -> Result<String> {
        assert_eq!(None, model_id);
        Ok("  add a test  ".to_string())
    }
}

const CATALOG: &str = r#"{"models": [
  {"id": "test-worker", "displayName": "Test Worker", "family": "test", "task": "chat",
   "capabilities": ["worker"], "defaults": {"nCtx": "8192"},
   "artifacts": [{"file": "worker.bin", "url": "http://127.0.0.1:9/worker.bin", "bytes": 4, "format": "bin"}]},
  {"id": "whisper-test", "displayName": "Whisper Test", "family": "whisper", "task": "speech",
   "artifacts": [{"file": "w.bin", "url": "http://127.0.0.1:9/w.bin", "bytes": 1000, "format": "bin"}]}
], "defaultWorkerModel": "test-worker", "defaultSpeechModel": "whisper-test"}"#;

/// A home with two chat models on disk: the catalog's worker and another chat model.
fn installed(home: &Home, models: &[(&str, &str, &str)]) {
    for (dir, id, name) in models {
        let d = home.models_dir().join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(format!("{id}.bin")), "test").unwrap();
        std::fs::write(
            d.join(format!("{id}.bin.json")),
            json!({"id": id, "displayName": name, "family": dir, "task": "chat"}).to_string(),
        )
        .unwrap();
    }
}

struct Fixture {
    tmp: tempfile::TempDir,
    script: Arc<Script>,
    runtime: Arc<FakeRuntime>,
    service: CodeService,
    web_on: Arc<AtomicBool>,
}

async fn fixture(models: &[(&str, &str, &str)]) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_layout().unwrap();
    installed(&home, models);
    let script = Arc::new(Script::default());
    let base = engine(script.clone()).await;
    let catalog = Arc::new(ModelCatalog::from_json(CATALOG).unwrap());
    let registry = Arc::new(ModelRegistry::with_shared_dir(
        home.clone(),
        catalog.clone(),
        Arc::new(Downloader::new()),
        None,
    ));
    let runtime = Arc::new(FakeRuntime {
        registry,
        catalog,
        base,
        loaded: AtomicBool::new(false),
        failures: Mutex::new(VecDeque::new()),
        asked: AtomicUsize::new(0),
    });
    let web_on = Arc::new(AtomicBool::new(false));
    let switch = web_on.clone();
    let service = CodeService::with_web(
        home,
        runtime.clone(),
        Arc::new(FakeWeb::default()),
        move || switch.load(Ordering::SeqCst),
    );
    Fixture {
        tmp,
        script,
        runtime,
        service,
        web_on,
    }
}

const BOTH: &[(&str, &str, &str)] = &[
    ("test", "test-worker", "Test Worker"),
    ("other", "plain-chat", "Plain Chat"),
];

/// The session once its turn has ended.
async fn settled(service: &CodeService, id: &str) -> CodeSession {
    for _ in 0..1200 {
        let s = service.session(id).expect("the session");
        if !s.running() {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the turn did not end");
}

fn last_run(s: &CodeSession) -> Run {
    s.last_run().cloned().expect("a run")
}

fn last_note(s: &CodeSession) -> Note {
    match s.entries.last() {
        Some(Entry::Note(n)) => n.clone(),
        other => panic!("not a note: {other:?}"),
    }
}

#[tokio::test]
async fn a_turn_changes_the_copy_and_apply_brings_it_to_the_repository() {
    let f = fixture(BOTH).await;
    let repo = repo(&f.tmp.path().join("my repo")).await;
    let mut events = events::subscribe();
    f.script.worker(tool_call(
        "write_file",
        r#"{"path":"hello.txt","content":"hi\n"}"#,
    ));
    f.script.worker(json!("Wrote hello.txt."));

    let s = f
        .service
        .start(&repo.to_string_lossy(), "Add a hello file", None, None)
        .await
        .unwrap();
    assert_eq!("Add a hello file", s.title);
    assert!(s.running(), "the turn runs in the background");
    assert_eq!(
        Some("a Code session is working".to_string()),
        f.service.busy_with()
    );
    let s = settled(&f.service, &s.id).await;
    let r = last_run(&s);
    assert_eq!(None, r.error, "{r:?}");
    assert_eq!(Some("Wrote hello.txt."), r.summary.as_deref());
    assert_eq!(None, r.gave_up);
    assert_eq!(None, r.verified, "no check was asked for");
    assert_eq!(1, r.tool_calls);
    assert_eq!(Some("Test Worker"), r.model.as_deref());
    assert!(r.before.is_some() && r.after.is_some() && r.before != r.after);
    assert!(
        r.stat.as_deref().unwrap_or("").contains("hello.txt"),
        "{r:?}"
    );
    assert_eq!(
        vec![
            "loading Test Worker on the GPU".to_string(),
            "making a scratch copy of my repo".to_string(),
            "writing hello.txt".to_string(),
        ],
        r.steps
    );
    assert!(
        r.context.is_some_and(|c| c.window == 6144),
        "the engine's own window once loaded"
    );
    let change = s.change.clone().expect("a change");
    assert!(change.diff.contains("+hi"), "{}", change.diff);
    assert!(
        !repo.join("hello.txt").exists(),
        "the person's files wait for Apply"
    );
    assert_eq!(None, f.service.busy_with());
    assert!(
        f.service.phases().is_empty(),
        "a finished turn has no phase"
    );

    // what the engine was sent
    let triage = f.script.triage_bodies();
    assert_eq!(1, triage.len(), "a short request is triaged first");
    assert_eq!(600, triage[0]["max_tokens"]);
    assert!(triage[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .ends_with("\nThe project folder is my repo."));
    let work = f.script.worker_bodies();
    assert_eq!("auto", work[0]["tool_choice"]);
    assert!(work[0]["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("TASK: Add a hello file"));

    // the code this run wrote
    let written = f.service.run_diff(&s.id, &r.id).await.expect("a diff");
    assert!(written.contains("hello.txt"), "{written}");
    let next = f
        .service
        .next_context(&s.id)
        .expect("a worker and a session");
    assert_eq!(6144, next.window);
    assert!(
        next.tokens > 1000,
        "the instructions and the tools: {next:?}"
    );

    // a change the UI heard about
    let mut heard = false;
    while let Ok(e) = events.try_recv() {
        heard |= e.topic == topic::CODE && e.payload["sessionId"] == json!(s.id);
    }
    assert!(heard, "every change goes out as a code event");

    f.service.apply(&s.id).await.unwrap();
    assert_eq!("hi\n", read(&repo.join("hello.txt")));
    let s = f.service.session(&s.id).unwrap();
    let n = last_note(&s);
    assert_eq!(Some(APPLIED), n.tone.as_deref());
    assert!(
        n.text.starts_with("Applied 1 file to ") && n.text.ends_with(". Nothing was committed."),
        "{}",
        n.text
    );
    assert_eq!(None, s.change);
    assert!(s.baseline.is_some());
    assert_eq!(
        "There is nothing new to apply.",
        f.service.apply(&s.id).await.unwrap_err().to_string()
    );

    // the next request builds on it and recalls it
    f.script.worker(tool_call(
        "write_file",
        r#"{"path":"bye.txt","content":"bye\n"}"#,
    ));
    f.script.worker(json!("Wrote bye.txt."));
    f.service
        .send(&s.id, "Now add a goodbye file", None, None)
        .await
        .unwrap();
    let s = settled(&f.service, &s.id).await;
    let change = s.change.clone().expect("a change");
    assert_eq!(
        vec!["bye.txt"],
        files(&change.diff),
        "only what is new since the apply"
    );
    let brief_sent = f.script.worker_bodies().last().unwrap()["messages"][1]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        brief_sent.contains("Earlier request: Add a hello file")
            && brief_sent.contains("What you did: Wrote hello.txt.")
            && brief_sent.contains("Now: Now add a goodbye file"),
        "{brief_sent}"
    );

    // undo takes the last request back, once
    f.service.undo(&s.id).await.unwrap();
    let s = f.service.session(&s.id).unwrap();
    let worktree = PathBuf::from(s.worktree.clone().unwrap());
    assert!(!worktree.join("bye.txt").exists());
    assert!(last_run(&s).undone);
    assert_eq!(Some(UNDONE), last_note(&s).tone.as_deref());
    assert_eq!(None, s.change);
    assert_eq!(
        "There is no request to undo.",
        f.service.undo(&s.id).await.unwrap_err().to_string()
    );

    // discard goes back to what was last applied
    std::fs::write(worktree.join("stray.txt"), "x").unwrap();
    f.service.discard(&s.id).await.unwrap();
    let s = f.service.session(&s.id).unwrap();
    assert!(!worktree.join("stray.txt").exists());
    assert_eq!(
        "Changes discarded. The next request starts from what was last applied.",
        last_note(&s).text
    );

    f.service.rename(&s.id, "  Hello and goodbye ");
    assert_eq!("Hello and goodbye", f.service.session(&s.id).unwrap().title);
    f.service.rename(&s.id, " ");
    assert_eq!("Hello and goodbye", f.service.session(&s.id).unwrap().title);
    assert_eq!(
        vec![repo.to_string_lossy().into_owned()],
        f.service.recent_repositories()
    );

    f.service.delete(&s.id).await;
    assert!(f.service.session(&s.id).is_none());
    for _ in 0..400 {
        if !worktree.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(!worktree.exists(), "the scratch copy goes with the session");
    assert!(!f
        .tmp
        .path()
        .join("home")
        .join("code")
        .join("sessions")
        .join(format!("{}.json", s.id))
        .exists());
}

#[tokio::test]
async fn a_greeting_is_answered_without_a_scratch_copy() {
    let f = fixture(BOTH).await;
    let folder = f.tmp.path().join("plain");
    std::fs::create_dir_all(&folder).unwrap();
    f.web_on.store(true, Ordering::SeqCst);
    f.script
        .triage("TALK: Hi! I change a private copy of your files.");
    let s = f
        .service
        .start(&folder.to_string_lossy(), "Hey", None, None)
        .await
        .unwrap();
    let s = settled(&f.service, &s.id).await;
    let r = last_run(&s);
    assert_eq!(
        Some("Hi! I change a private copy of your files."),
        r.summary.as_deref()
    );
    assert_eq!(0, r.tool_calls);
    assert_eq!(None, s.worktree, "no copy for a greeting");
    assert!(f.script.worker_bodies().is_empty());
    let system = f.script.triage_bodies()[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(system.contains(TALK_WEB.trim()), "the web is on: {system}");
}

#[tokio::test]
async fn a_session_says_which_page_started_it() {
    let f = fixture(BOTH).await;
    let folder = f.tmp.path().join("plain");
    std::fs::create_dir_all(&folder).unwrap();
    let folder = folder.to_string_lossy().to_string();
    f.script.triage("TALK: Hi.");
    f.script.triage("TALK: Hi again.");
    let chat = f.service.start(&folder, "Hey", None, None).await.unwrap();
    let editor = f
        .service
        .start_from(Origin::Editor, &folder, "Hey", None, None)
        .await
        .unwrap();
    assert_eq!(Origin::Chat, chat.origin);
    assert_eq!(Origin::Editor, editor.origin);
    settled(&f.service, &chat.id).await;
    settled(&f.service, &editor.id).await;

    // An older session the Code page's panel remembers becomes the panel's, and stays so on disk.
    f.service
        .mark_editor_sessions([chat.id.as_str(), "no-such-session"]);
    assert_eq!(Origin::Editor, f.service.session(&chat.id).unwrap().origin);
    let file = Home::at(f.tmp.path().join("home"))
        .code_dir()
        .join("sessions")
        .join(format!("{}.json", chat.id));
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    assert_eq!("editor", saved["origin"]);
}

#[test]
fn a_session_saved_before_origins_reads_as_the_chat_pages() {
    let s: CodeSession =
        serde_json::from_str(r#"{"id":"ab12cd34","title":"Old","repository":"C:\\r"}"#).unwrap();
    assert_eq!(Origin::Chat, s.origin);
}

#[tokio::test]
async fn stop_ends_a_turn_at_once() {
    let f = fixture(BOTH).await;
    let repo = repo(&f.tmp.path().join("repo")).await;
    f.script.worker(json!("hang"));
    let long = format!("Refactor the module. {}", "Keep the behaviour. ".repeat(20));
    let s = f
        .service
        .start(&repo.to_string_lossy(), &long, None, None)
        .await
        .unwrap();
    let mut thinking = false;
    for _ in 0..800 {
        if f.service.phases().values().any(|p| p == THINKING) {
            thinking = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(thinking, "the model is writing its next move");
    assert!(
        f.script.triage_bodies().is_empty(),
        "a long request is work"
    );
    f.service.stop(&s.id);
    let s = tokio::time::timeout(Duration::from_secs(20), settled(&f.service, &s.id))
        .await
        .expect("stopped without waiting for the engine");
    let r = last_run(&s);
    assert_eq!(Some("stopped"), r.gave_up.as_deref());
    assert_eq!(None, r.error);
}

/// A worker whose model does not load stops at once with the runtime's reason: not after the
/// loop's retries, and not as replies that could not be read (2026-09-26: a vision encoder was
/// the worker, the engine started six times and the session blamed the replies).
#[tokio::test]
async fn a_worker_that_cannot_load_stops_at_once_with_the_reason() {
    let f = fixture(BOTH).await;
    let repo = repo(&f.tmp.path().join("repo")).await;
    let refusal = || -> anyhow::Error {
        ModelLoadError {
            message: "Test Worker can't run in Nook: the engine (llama.cpp b10752) doesn't know its model architecture 'deepseek4-vision'.".into(),
            permanent: true,
        }
        .into()
    };
    // A short request is triaged first: the triage's lease fails, and nothing else is tried.
    f.runtime
        .failures
        .lock()
        .extend([refusal(), refusal(), refusal()]);
    let s = f
        .service
        .start(&repo.to_string_lossy(), "Add a hello file", None, None)
        .await
        .unwrap();
    let s = settled(&f.service, &s.id).await;
    let r = last_run(&s);
    assert_eq!(
        r.gave_up.as_deref(),
        Some("Test Worker can't run in Nook: the engine (llama.cpp b10752) doesn't know its model architecture 'deepseek4-vision'"),
        "{r:?}"
    );
    assert_eq!(None, r.error);
    assert_eq!(1, f.runtime.asked.load(Ordering::SeqCst), "asked once");
    assert_eq!(None, s.worktree, "no copy made for a turn that cannot run");
    assert!(f.script.triage_bodies().is_empty() && f.script.worker_bodies().is_empty());

    // A long request goes to the worker's loop, which stops the same way; an engine that only
    // said it exited reads as the first line of what it said.
    f.runtime.failures.lock().clear();
    f.runtime.failures.lock().extend([
        anyhow!("Engine exited with code 1. \n=== starting llama-server.exe\nloading"),
        anyhow!("again"),
    ]);
    let long = format!("Refactor the module. {}", "Keep the behaviour. ".repeat(20));
    f.service.send(&s.id, &long, None, None).await.unwrap();
    let s = settled(&f.service, &s.id).await;
    let r = last_run(&s);
    assert_eq!(
        r.gave_up.as_deref(),
        Some("Test Worker could not be loaded: engine exited with code 1"),
        "{r:?}"
    );
    assert_eq!(2, f.runtime.asked.load(Ordering::SeqCst), "asked once more");
    assert_eq!(0, r.tool_calls);

    // A model unloaded while the request waited is asked for again, as before.
    f.runtime.failures.lock().clear();
    f.runtime.failures.lock().push_back(
        AdmissionError("The model was unloaded while the request was waiting.".into()).into(),
    );
    f.script.worker(json!("Nothing to change."));
    f.service.send(&s.id, &long, None, None).await.unwrap();
    let r = last_run(&settled(&f.service, &s.id).await);
    assert_eq!(None, r.gave_up, "{r:?}");
    assert_eq!(Some("Nothing to change."), r.summary.as_deref());
    assert_eq!(4, f.runtime.asked.load(Ordering::SeqCst));
}

#[tokio::test]
async fn requests_that_cannot_start_say_why() {
    let f = fixture(BOTH).await;
    let repo = repo(&f.tmp.path().join("repo")).await;
    let folder = repo.to_string_lossy().into_owned();
    let err = |r: Result<()>| r.unwrap_err().to_string();

    assert_eq!(
        "That session is gone.",
        err(f.service.send("nope", "x", None, None).await)
    );
    assert_eq!(
        "Say what to change.",
        f.service
            .start(&folder, "  ", None, None)
            .await
            .unwrap_err()
            .to_string()
    );
    assert!(
        f.service.sessions().is_empty(),
        "nothing ran: no session kept"
    );
    let refused = f
        .service
        .start(&folder, "Fix it", Some("rm -rf /"), None)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        refused.starts_with("\"rm -rf /\" is not a check this project allows. Allowed commands (")
            && refused.ends_with(". Add it to nook.json in the project folder ({\"verify\": [\"...\"]}) or pick one of those."),
        "{refused}"
    );
    let missing = f
        .service
        .start(&repo.join("gone").to_string_lossy(), "Fix it", None, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(missing.starts_with("Path does not exist: "), "{missing}");
    assert!(f.service.repository_state("relative/path").await.is_err());
    let st = f.service.repository_state(&folder).await.unwrap();
    assert_eq!(code_workspace::Readiness::Repository, st.readiness);

    // the worker menu, and a choice that is not installed
    let choices = f.service.worker_choices();
    assert_eq!(
        vec![
            WorkerChoice {
                id: "test-worker".into(),
                name: "Test Worker".into(),
                tested: true
            },
            WorkerChoice {
                id: "plain-chat".into(),
                name: "Plain Chat".into(),
                tested: false
            },
        ],
        choices
    );
    assert_eq!(
        "That model is not installed as a worker.",
        f.service.set_worker("nope").unwrap_err().to_string()
    );
    f.service.set_worker("plain-chat").unwrap();
    assert_eq!(
        Some("plain-chat".to_string()),
        f.service.worker().map(|w| w.id)
    );

    // voice input goes through the runtime
    assert_eq!(
        Some(SpeechModel {
            id: "whisper-test".into(),
            name: "Whisper Test".into(),
            bytes: 1000
        }),
        f.service.speech_model()
    );
    assert_eq!(
        Some("The speech model is not installed.".to_string()),
        f.service.speech_problem()
    );
    assert_eq!(
        "add a test",
        f.service
            .transcribe(&f.tmp.path().join("x.wav"))
            .await
            .unwrap()
    );
    assert!(!f.runtime.loaded.load(Ordering::SeqCst));

    // no worker at all
    let none = fixture(&[]).await;
    assert_eq!(
        "No worker model is installed. Download Test Worker in Settings > Models.",
        none.service
            .start(&folder, "Fix it", None, None)
            .await
            .unwrap_err()
            .to_string()
    );
    assert_eq!("Test Worker", none.service.worker_names());
    assert!(none.service.snapshot().worker_name.is_none());
}

#[tokio::test]
async fn a_session_whose_copy_is_not_here_applies_from_one_made_again() {
    // A session imported from the Kotlin Nook (crate::migrate): its scratch copy stayed in the old
    // home, and its worktree names the place this home keeps copies, where there is none yet.
    let f = fixture(BOTH).await;
    let home = Home::at(f.tmp.path().join("home"));
    let repo = repo(&f.tmp.path().join("repo")).await;
    let old_copy = f
        .tmp
        .path()
        .join("Nook")
        .join("tmp")
        .join("code")
        .join("k1");
    let old = create(&repo, &old_copy).await.unwrap();
    std::fs::write(old.dir.join("B.txt"), "pending\n").unwrap();
    let change = change_of(&old.dir, None).await.unwrap();
    let mut s = lost_session("k1", &repo, &old, None, change);
    let here = home.temp_dir().join("code").join("k1");
    s.worktree = Some(here.to_string_lossy().into_owned());
    CodeStore::new(home.code_dir().join("sessions"))
        .save(&s)
        .unwrap();
    let service = CodeService::with_web(
        home,
        f.runtime.clone(),
        Arc::new(FakeWeb::default()),
        || false,
    );

    service.apply("k1").await.unwrap();
    assert_eq!("pending\n", read(&repo.join("B.txt")), "applied");
    assert!(here.join("B.txt").is_file(), "the copy was made here");
    assert_eq!(
        "pending\n",
        read(&old.dir.join("B.txt")),
        "the old copy is as it was"
    );
    let s = service.session("k1").unwrap();
    let notes: Vec<&str> = s
        .entries
        .iter()
        .filter_map(|e| match e {
            Entry::Note(n) => Some(n.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        notes[0].contains("put the pending change back"),
        "{notes:?}"
    );
    assert!(notes[1].starts_with("Applied 1 file"), "{notes:?}");
    remove(&repo, &here).await;
    remove(&repo, &old.dir).await;
}

#[tokio::test]
async fn a_run_in_flight_when_nook_closed_is_over_when_it_starts_again() {
    let f = fixture(BOTH).await;
    let home = Home::at(f.tmp.path().join("home"));
    let store = CodeStore::new(home.code_dir().join("sessions"));
    let s = session(vec![
        Entry::Task(Task::new("t1", 1, "x")),
        Entry::Run(Run::started("r1", "Test Worker")),
    ]);
    store.save(&s).unwrap();
    let again = CodeService::with_web(
        home,
        f.runtime.clone(),
        Arc::new(FakeWeb::default()),
        || false,
    );
    let back = again.session("s").unwrap();
    assert!(!back.running());
    assert_eq!(
        Some("Nook closed before this run finished."),
        last_run(&back).error.as_deref()
    );
    assert_eq!(None, again.busy_with());
}

#[tokio::test]
async fn the_snapshot_is_what_the_ui_reads() {
    let f = fixture(BOTH).await;
    let folder = f.tmp.path().join("plain");
    std::fs::create_dir_all(&folder).unwrap();
    f.script.triage("TALK: Hello.");
    let s = f
        .service
        .start(&folder.to_string_lossy(), "Hi", None, None)
        .await
        .unwrap();
    settled(&f.service, &s.id).await;
    let v = serde_json::to_value(f.service.snapshot()).unwrap();
    assert_eq!("Test Worker", v["workerName"]);
    assert_eq!("Test Worker", v["workerHint"]);
    assert_eq!("test-worker", v["workerId"]);
    assert_eq!(true, v["workers"][0]["tested"]);
    assert!(v["phases"].as_object().unwrap().is_empty());
    let session = &v["sessions"][0];
    assert_eq!("task", session["entries"][0]["kind"]);
    assert_eq!("run", session["entries"][1]["kind"]);
    assert_eq!("Hello.", session["entries"][1]["summary"]);
    assert!(session["worktree"].is_null());
    assert!(session.get("createdAt").is_some() && session.get("updatedAt").is_some());
    let next = serde_json::to_value(f.service.next_context(&s.id).unwrap()).unwrap();
    assert!(
        next.get("tokens").is_some() && next.get("recap").is_some() && next.get("window").is_some()
    );
}

//! One Code-mode session: a repository, a scratch copy of it the local worker edits, the change
//! that copy holds, and the conversation about it (what the person asked, what the worker did,
//! notes). Treated as a value: [`CodeService`](super::code_service::CodeService) replaces a
//! session with an edited copy and saves it.
//!
//! Ports `code/CodeSession.java`. The JSON is the original's (Jackson wrote the records' components
//! as they are named; entries carry `"kind": "task" | "run" | "note"`), so the UI types in
//! `ui/src/api/code.ts` read it field for field. Missing fields read as Jackson read them (null,
//! 0, false) and unknown ones are ignored.

use serde::{Deserialize, Serialize};

/// A session.
///
/// - `worktree`: the scratch copy (a git worktree of the repository, or a private copy of a
///   plain folder), created on the first run
/// - `base_commit`: the commit the scratch copy started from
/// - `baseline`: what the change is taken against: None for `base_commit`, else the tree the
///   scratch copy held when its changes were last applied to the repository
/// - `change`: what the scratch copy holds that the repository does not, or None when nothing
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CodeSession {
    pub id: String,
    pub title: String,
    pub repository: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub worktree: Option<String>,
    pub base_commit: Option<String>,
    pub baseline: Option<String>,
    pub verify: Option<String>,
    pub change: Option<Change>,
    #[serde(deserialize_with = "null_as_empty")]
    pub entries: Vec<Entry>,
}

/// The change as it stands: the diff against the baseline, cut at
/// [`MAX_STORED_DIFF`](super::code_service::MAX_STORED_DIFF) characters for the record (`cut`);
/// the scratch copy has all of it and Apply takes all of it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Change {
    pub diff: String,
    pub cut: bool,
    pub stat: String,
}

/// A step in the conversation. (A run is a few hundred bytes more than a task or a note; a
/// session holds tens of entries, so they are kept unboxed, as the records they mirror.)
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Entry {
    Task(Task),
    Run(Run),
    Note(Note),
}

impl Entry {
    pub fn id(&self) -> &str {
        match self {
            Entry::Task(t) => &t.id,
            Entry::Run(r) => &r.id,
            Entry::Note(n) => &n.id,
        }
    }

    pub fn at(&self) -> i64 {
        match self {
            Entry::Task(t) => t.at,
            Entry::Run(r) => r.at,
            Entry::Note(n) => n.at,
        }
    }
}

/// What the person asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Task {
    pub id: String,
    pub at: i64,
    pub text: String,
}

impl Task {
    pub fn new(id: impl Into<String>, at: i64, text: impl Into<String>) -> Task {
        Task {
            id: id.into(),
            at,
            text: text.into(),
        }
    }
}

/// How full the worker's context was in a run: `used` tokens after the model's last reply and
/// the `peak` of the run, of the `window` one request has, as the engine counted them
/// (`measured`) or estimated; `dropped` old tool outputs blanked to make room. (`CodeSession.Context`
/// in the original; named as the UI names it.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RunContext {
    pub used: u32,
    pub peak: u32,
    pub window: u32,
    pub dropped: u32,
    pub measured: bool,
}

impl RunContext {
    fn next(&self, now: u32, window: u32, dropped: u32, measured: bool) -> RunContext {
        RunContext {
            used: now,
            peak: self.peak.max(now),
            window,
            dropped,
            measured: self.measured && measured,
        }
    }

    fn first(now: u32, window: u32, dropped: u32, measured: bool) -> RunContext {
        RunContext {
            used: now,
            peak: now,
            window,
            dropped,
            measured,
        }
    }
}

/// One run of the local worker. While `running`, `steps` grows; afterwards the rest is filled
/// in. `stat` is the change as it stood after this run; `before` is the tree the scratch copy
/// held before it, which Undo puts back; `after` the tree it held when the run ended, so the reply
/// can show the code this run wrote; `context` how full the worker's context got, None for a run
/// that asked the model nothing (or ran before it was kept).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Run {
    pub id: String,
    pub at: i64,
    pub model: Option<String>,
    pub running: bool,
    #[serde(deserialize_with = "null_as_empty")]
    pub steps: Vec<String>,
    pub summary: Option<String>,
    pub stat: Option<String>,
    pub verified: Option<bool>,
    pub verify_command: Option<String>,
    pub verify_note: Option<String>,
    pub verify_output: Option<String>,
    pub gave_up: Option<String>,
    pub tool_calls: u32,
    pub seconds: u64,
    pub error: Option<String>,
    pub before: Option<String>,
    pub undone: bool,
    pub after: Option<String>,
    pub context: Option<RunContext>,
}

/// Steps a run keeps: the newest.
const MAX_STEPS: usize = 300;

impl Run {
    pub fn started(id: impl Into<String>, model: impl Into<String>) -> Run {
        Run {
            id: id.into(),
            at: now_millis(),
            model: Some(model.into()),
            running: true,
            ..Run::default()
        }
    }

    pub fn with_step(&self, step: &str) -> Run {
        let mut r = self.clone();
        r.steps.push(step.to_string());
        if r.steps.len() > MAX_STEPS {
            let extra = r.steps.len() - MAX_STEPS;
            r.steps.drain(..extra);
        }
        r
    }

    pub fn with_before(&self, tree: &str) -> Run {
        Run {
            before: Some(tree.to_string()),
            ..self.clone()
        }
    }

    pub fn failed(&self, why: &str) -> Run {
        Run {
            running: false,
            seconds: ((now_millis() - self.at) / 1000).max(0) as u64,
            error: Some(why.to_string()),
            ..self.clone()
        }
    }

    /// With the context as the engine's latest reply left it.
    pub fn with_context(&self, used: u32, window: u32, dropped: u32, measured: bool) -> Run {
        let c = match &self.context {
            None => RunContext::first(used, window, dropped, measured),
            Some(c) => c.next(used, window, dropped, measured),
        };
        Run {
            context: Some(c),
            ..self.clone()
        }
    }

    pub fn mark_undone(&self) -> Run {
        Run {
            undone: true,
            ..self.clone()
        }
    }
}

/// Tones of a [`Note`]: `applied` and `discarded` end a change; `undone`, info, ok and error are
/// said only.
pub const APPLIED: &str = "applied";
pub const DISCARDED: &str = "discarded";
pub const UNDONE: &str = "undone";

/// Something Nook did or has to say: applied, discarded, undone, a failure.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Note {
    pub id: String,
    pub at: i64,
    pub text: String,
    pub tone: Option<String>,
}

impl CodeSession {
    // ------------------------------------------------------------------ edits

    fn with(&self, entries: Vec<Entry>) -> CodeSession {
        CodeSession {
            updated_at: now_millis(),
            entries,
            ..self.clone()
        }
    }

    pub fn append(&self, e: Entry) -> CodeSession {
        let mut l = self.entries.clone();
        l.push(e);
        self.with(l)
    }

    /// Replaces the entry with `entry_id` by what `edit` makes of it.
    pub fn edit(&self, entry_id: &str, edit: impl FnOnce(&Entry) -> Entry) -> CodeSession {
        match self.entries.iter().position(|e| e.id() == entry_id) {
            Some(i) => {
                let mut l = self.entries.clone();
                l[i] = edit(&l[i]);
                self.with(l)
            }
            None => self.clone(),
        }
    }

    /// Replaces the run with `run_id` by what `edit` makes of it; other entries stay.
    pub fn edit_run(&self, run_id: &str, edit: impl FnOnce(&Run) -> Run) -> CodeSession {
        self.edit(run_id, |e| match e {
            Entry::Run(r) => Entry::Run(edit(r)),
            other => other.clone(),
        })
    }

    pub fn with_worktree(&self, dir: &str, base: &str) -> CodeSession {
        CodeSession {
            updated_at: now_millis(),
            worktree: Some(dir.to_string()),
            base_commit: Some(base.to_string()),
            baseline: None,
            ..self.clone()
        }
    }

    pub fn with_baseline(&self, tree: Option<&str>) -> CodeSession {
        CodeSession {
            updated_at: now_millis(),
            baseline: tree.map(str::to_string),
            ..self.clone()
        }
    }

    pub fn with_change(&self, c: Option<Change>) -> CodeSession {
        CodeSession {
            updated_at: now_millis(),
            change: c,
            ..self.clone()
        }
    }

    pub fn with_verify(&self, v: Option<&str>) -> CodeSession {
        CodeSession {
            verify: v.map(str::to_string),
            ..self.clone()
        }
    }

    pub fn with_title(&self, t: &str) -> CodeSession {
        CodeSession {
            title: t.to_string(),
            ..self.clone()
        }
    }

    /// The last run, or None.
    pub fn last_run(&self) -> Option<&Run> {
        self.entries.iter().rev().find_map(|e| match e {
            Entry::Run(r) => Some(r),
            _ => None,
        })
    }

    /// The run Undo would take back: the last one, when it finished, is not undone yet, knows its
    /// starting tree, and nothing was applied or discarded since.
    pub fn undoable(&self) -> Option<&Run> {
        for e in self.entries.iter().rev() {
            match e {
                Entry::Note(n) if matches!(n.tone.as_deref(), Some(APPLIED) | Some(DISCARDED)) => {
                    return None;
                }
                Entry::Run(r) => {
                    return (!r.running && !r.undone && r.before.is_some()).then_some(r);
                }
                _ => {}
            }
        }
        None
    }

    /// The person's requests in order.
    pub fn tasks(&self) -> Vec<&Task> {
        self.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Task(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    pub fn running(&self) -> bool {
        self.last_run().is_some_and(|r| r.running)
    }
}

/// Milliseconds since the epoch (`System.currentTimeMillis`).
pub fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// A list written as null reads as empty (the original's compact constructors did the same).
fn null_as_empty<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A finished run as the original's tests made one.
    pub(crate) fn run(id: &str, summary: &str) -> Run {
        Run {
            id: id.into(),
            at: 1,
            model: Some("gpt-oss".into()),
            running: false,
            steps: vec!["reading A.java".into()],
            summary: Some(summary.into()),
            stat: Some(" 1 file changed".into()),
            verified: Some(true),
            verify_command: Some("gradlew test".into()),
            tool_calls: 3,
            seconds: 20,
            before: Some("tree0".into()),
            after: Some("tree1".into()),
            ..Run::default()
        }
    }

    pub(crate) fn session(entries: Vec<Entry>) -> CodeSession {
        CodeSession {
            id: "s".into(),
            title: "t".into(),
            repository: "F:/r".into(),
            created_at: 1,
            updated_at: 1,
            entries,
            ..CodeSession::default()
        }
    }

    pub(crate) fn note(id: &str, text: &str, tone: &str) -> Entry {
        Entry::Note(Note {
            id: id.into(),
            at: 1,
            text: text.into(),
            tone: Some(tone.into()),
        })
    }

    #[test]
    fn only_the_last_finished_run_can_be_undone_and_not_across_an_apply() {
        let s = session(vec![
            Entry::Task(Task::new("t1", 1, "x")),
            Entry::Run(run("a", "one")),
            Entry::Task(Task::new("t2", 1, "y")),
            Entry::Run(run("b", "two")),
        ]);
        assert_eq!("b", s.undoable().unwrap().id);
        assert!(
            s.edit_run("b", Run::mark_undone).undoable().is_none(),
            "one step back only"
        );
        assert!(s
            .append(note("n", "Applied.", APPLIED))
            .undoable()
            .is_none());
        assert!(
            s.append(Entry::Task(Task::new("t3", 1, "z")))
                .append(Entry::Run(Run::started("c", "m")))
                .undoable()
                .is_none(),
            "not while running"
        );
    }

    #[test]
    fn a_run_keeps_how_full_its_context_got() {
        let r = Run::started("u1", "gpt-oss");
        assert!(
            r.context.is_none(),
            "nothing counted before the model's first reply"
        );
        let r = r
            .with_context(3000, 8192, 0, true)
            .with_context(6100, 8192, 1, true)
            .with_context(4200, 8192, 2, true);
        assert_eq!(
            Some(RunContext {
                used: 4200,
                peak: 6100,
                window: 8192,
                dropped: 2,
                measured: true
            }),
            r.context,
            "now, the most, the window, what was dropped"
        );
        assert!(
            !r.with_context(4300, 8192, 2, false)
                .context
                .unwrap()
                .measured,
            "one estimate makes the peak an estimate too"
        );
        assert_eq!(
            r.context,
            r.with_step("reading A.java").failed("Stopped.").context,
            "kept through steps and a failure"
        );
    }

    #[test]
    fn the_json_is_the_originals() {
        let s = session(vec![
            Entry::Task(Task::new("t1", 1, "Do it")),
            Entry::Run(run("u1", "Did it.")),
            note("n1", "Applied 1 file.", "ok"),
        ]);
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!("task", v["entries"][0]["kind"]);
        assert_eq!("run", v["entries"][1]["kind"]);
        assert_eq!("note", v["entries"][2]["kind"]);
        assert_eq!("gradlew test", v["entries"][1]["verifyCommand"]);
        assert_eq!(3, v["entries"][1]["toolCalls"]);
        assert!(v["entries"][1]["context"].is_null());
        assert!(v["baseCommit"].is_null() && v["change"].is_null());
        assert_eq!(1, v["createdAt"]);

        // what Jackson wrote for a run in flight, with nulls and a field this build does not know
        let old = r#"{"id":"x","title":"T","repository":"C:\\r","createdAt":1,"updatedAt":2,
            "worktree":null,"baseCommit":null,"baseline":null,"verify":null,"change":null,"extra":1,
            "entries":[{"kind":"run","id":"r","at":1,"model":"M","running":true,"steps":null,
            "summary":null,"stat":null,"verified":null,"verifyCommand":null,"verifyNote":null,
            "verifyOutput":null,"gaveUp":null,"toolCalls":0,"seconds":0,"error":null,
            "before":null,"undone":false,"after":null}]}"#;
        let back: CodeSession = serde_json::from_str(old).unwrap();
        assert!(back.running());
        assert!(back.last_run().unwrap().steps.is_empty());
        assert_eq!("C:\\r", back.repository);
    }
}

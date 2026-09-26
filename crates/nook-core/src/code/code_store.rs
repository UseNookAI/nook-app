//! Code sessions on disk: one JSON file per session under `<home>\code\sessions`.
//!
//! Ports `code/CodeStore.java`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use super::code_session::CodeSession;

pub struct CodeStore {
    dir: PathBuf,
}

impl CodeStore {
    pub fn new(dir: impl Into<PathBuf>) -> CodeStore {
        CodeStore { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every session that can be read; one that cannot is skipped with a warning.
    pub fn load_all(&self) -> Vec<CodeSession> {
        let mut out = Vec::new();
        if !self.dir.is_dir() {
            return out;
        }
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    "Could not list code sessions in {}: {e}",
                    self.dir.display()
                );
                return out;
            }
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().ends_with(".json"))
            })
            .collect();
        files.sort();
        for f in files {
            match self.read(&f) {
                Ok(s) => out.push(s),
                Err(e) => tracing::warn!(
                    "Skipping a code session Nook cannot read: {} ({e:#})",
                    f.file_name().unwrap_or_default().to_string_lossy()
                ),
            }
        }
        out
    }

    /// One session file. Sessions from before 2026-09-24 can hold reviews by a Claude model, which
    /// Nook no longer has: they are left out, and the file as it was is kept once in
    /// `<home>\code\with-reviews`, since the next save writes the session without them.
    fn read(&self, f: &Path) -> Result<CodeSession> {
        let bytes = std::fs::read(f)?;
        let mut root: Value = serde_json::from_slice(&bytes)?;
        let mut dropped = false;
        if let Some(es) = root.get_mut("entries").and_then(Value::as_array_mut) {
            let before = es.len();
            es.retain(|e| e.get("kind").and_then(Value::as_str) != Some("review"));
            dropped = es.len() != before;
        }
        if dropped {
            if let (Some(parent), Some(name)) = (self.dir.parent(), f.file_name()) {
                let kept = parent.join("with-reviews").join(name);
                if !kept.exists() {
                    std::fs::create_dir_all(parent.join("with-reviews"))?;
                    std::fs::copy(f, &kept)?;
                }
            }
        }
        Ok(serde_json::from_value(root)?)
    }

    /// Writes through a temporary file, so a crash mid-write leaves the previous version.
    pub fn save(&self, s: &CodeSession) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("Could not create {}", self.dir.display()))?;
        let tmp = self.dir.join(format!("{}.json.tmp", s.id));
        let bytes = serde_json::to_vec(s)?;
        std::fs::write(&tmp, bytes)
            .with_context(|| format!("Could not write {}", tmp.display()))?;
        let target = self.dir.join(format!("{}.json", s.id));
        std::fs::rename(&tmp, &target)
            .with_context(|| format!("Could not replace {}", target.display()))?;
        Ok(())
    }

    pub fn delete(&self, id: &str) {
        let f = self.dir.join(format!("{id}.json"));
        match std::fs::remove_file(&f) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("Could not delete code session {id}: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::code_session::tests::{note, run};
    use crate::code::code_session::{Change, Entry, RunContext, Task};

    #[test]
    fn sessions_survive_a_round_trip_through_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CodeStore::new(tmp.path());
        let t = 5;
        let s = CodeSession {
            id: "abc".into(),
            title: "Title".into(),
            repository: "F:/r".into(),
            created_at: t,
            updated_at: t,
            worktree: Some("F:/w".into()),
            base_commit: Some("1234567".into()),
            baseline: Some("tree".into()),
            verify: Some("gradlew test".into()),
            change: Some(Change {
                diff: "diff --git a/A b/A".into(),
                cut: false,
                stat: " 1 file changed".into(),
            }),
            entries: vec![
                Entry::Task(Task::new("t1", t, "Do it")),
                Entry::Run(run("u1", "Did it.")),
                note("n1", "Applied 1 file.", "ok"),
            ],
        };
        store.save(&s).unwrap();
        let back = store.load_all();
        assert_eq!(vec![s], back);
    }

    #[test]
    fn a_session_with_a_claude_review_still_opens_without_it() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let old = concat!(
            "{\"id\":\"old\",\"title\":\"Title\",\"repository\":\"F:/r\",\"createdAt\":1,\"updatedAt\":1,\"entries\":[",
            "{\"kind\":\"task\",\"id\":\"t1\",\"at\":1,\"text\":\"Do it\"},",
            "{\"kind\":\"review\",\"id\":\"r1\",\"at\":1,\"reviewer\":\"claude-opus-5-5\",\"status\":\"done\",\"verdict\":\"approve\",",
            "\"summary\":\"Fine.\",\"findings\":[{\"file\":\"A.java\",\"line\":3,\"severity\":\"low\",\"comment\":\"x\"}]},",
            "{\"kind\":\"note\",\"id\":\"n1\",\"at\":1,\"text\":\"Applied 1 file.\",\"tone\":\"applied\"}]}"
        );
        std::fs::write(sessions.join("old.json"), old).unwrap();
        let back = CodeStore::new(&sessions).load_all();
        assert_eq!(1, back.len());
        let ids: Vec<&str> = back[0].entries.iter().map(|e| e.id()).collect();
        assert_eq!(vec!["t1", "n1"], ids);
        assert_eq!(
            old,
            std::fs::read_to_string(tmp.path().join("with-reviews").join("old.json")).unwrap(),
            "the file as it was is kept aside"
        );
    }

    #[test]
    fn a_runs_context_is_kept_on_disk_and_a_broken_file_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CodeStore::new(tmp.path());
        let r = crate::code::code_session::Run::started("u1", "gpt-oss")
            .with_context(3000, 8192, 0, true)
            .with_context(6100, 8192, 1, true);
        let s = CodeSession {
            id: "ctx".into(),
            title: "Title".into(),
            repository: "F:/r".into(),
            created_at: 5,
            updated_at: 5,
            entries: vec![Entry::Task(Task::new("t1", 5, "Do it")), Entry::Run(r)],
            ..CodeSession::default()
        };
        store.save(&s).unwrap();
        std::fs::write(tmp.path().join("broken.json"), "{ not json").unwrap();
        let all = store.load_all();
        assert_eq!(1, all.len());
        match &all[0].entries[1] {
            Entry::Run(r) => assert_eq!(
                Some(RunContext {
                    used: 6100,
                    peak: 6100,
                    window: 8192,
                    dropped: 1,
                    measured: true
                }),
                r.context
            ),
            other => panic!("{other:?}"),
        }
        store.delete("ctx");
        store.delete("ctx");
        assert!(store.load_all().is_empty());
    }
}

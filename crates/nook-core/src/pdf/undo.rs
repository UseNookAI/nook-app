//! The PDF editor's Undo: the document as it was before each edit, the last [`DEPTH`] of them.
//! Each is the whole document, so a large PDF edited many times would hold gigabytes: those that
//! fit in [`MEMORY_BUDGET`] stay in memory, and older ones go to files in a folder of the
//! document's own, removed when it closes.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// How many edits Undo can take back.
pub const DEPTH: usize = 30;
/// The most the snapshots hold in memory together; the oldest go to disk beyond it.
pub const MEMORY_BUDGET: usize = 192 << 20;

enum Snapshot {
    Memory(Vec<u8>),
    File(PathBuf),
}

pub struct UndoStack {
    items: VecDeque<Snapshot>,
    in_memory: usize,
    dir: PathBuf,
    next: u64,
    depth: usize,
    budget: usize,
}

impl UndoStack {
    /// Snapshots that go to disk are written into `dir`, made when first needed.
    pub fn new(dir: PathBuf) -> UndoStack {
        UndoStack::with_limits(dir, DEPTH, MEMORY_BUDGET)
    }

    pub fn with_limits(dir: PathBuf, depth: usize, budget: usize) -> UndoStack {
        UndoStack {
            items: VecDeque::new(),
            in_memory: 0,
            dir,
            next: 0,
            depth: depth.max(1),
            budget,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Bytes held in memory now.
    pub fn memory(&self) -> usize {
        self.in_memory
    }

    /// Keeps `bytes`, the document before an edit. The oldest go when there are more than the
    /// depth, and to disk when memory is over the budget. An older one that cannot be written is
    /// dropped; this one never is, and stays in memory instead. Returns how many were dropped.
    pub fn push(&mut self, bytes: Vec<u8>) -> usize {
        self.in_memory += bytes.len();
        self.items.push_back(Snapshot::Memory(bytes));
        while self.items.len() > self.depth {
            if let Some(old) = self.items.pop_front() {
                self.forget(old);
            }
        }
        self.spill()
    }

    /// The document before the last edit, or None when there is nothing to undo.
    pub fn pop(&mut self) -> Result<Option<Vec<u8>>> {
        match self.items.pop_back() {
            None => Ok(None),
            Some(Snapshot::Memory(bytes)) => {
                self.in_memory -= bytes.len();
                Ok(Some(bytes))
            }
            Some(Snapshot::File(path)) => {
                let bytes = std::fs::read(&path)
                    .with_context(|| format!("Could not read the undo step {}", path.display()));
                let _ = std::fs::remove_file(&path);
                bytes.map(Some)
            }
        }
    }

    /// Moves the oldest snapshots held in memory to disk until memory is within the budget,
    /// returning how many could not be written and were dropped.
    fn spill(&mut self) -> usize {
        let mut dropped = 0;
        while self.in_memory > self.budget {
            let Some(i) = self
                .items
                .iter()
                .position(|s| matches!(s, Snapshot::Memory(_)))
            else {
                break;
            };
            let Snapshot::Memory(bytes) =
                std::mem::replace(&mut self.items[i], Snapshot::File(PathBuf::new()))
            else {
                unreachable!();
            };
            self.in_memory -= bytes.len();
            match self.write(&bytes) {
                Ok(path) => self.items[i] = Snapshot::File(path),
                Err(e) if i + 1 == self.items.len() => {
                    // The step just taken: held over the budget rather than lost.
                    tracing::warn!(
                        "The last undo step could not go to disk, so it stays in memory: {e:#}"
                    );
                    self.in_memory += bytes.len();
                    self.items[i] = Snapshot::Memory(bytes);
                    break;
                }
                Err(e) => {
                    // Undo reaches back less far rather than holding more.
                    tracing::warn!("An undo step could not go to disk, so it is dropped: {e:#}");
                    self.items.remove(i);
                    dropped += 1;
                }
            }
        }
        dropped
    }

    fn write(&mut self, bytes: &[u8]) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("Could not create {}", self.dir.display()))?;
        self.next += 1;
        let path = self.dir.join(format!("{}.pdf", self.next));
        std::fs::write(&path, bytes)
            .with_context(|| format!("Could not write {}", path.display()))?;
        Ok(path)
    }

    fn forget(&mut self, s: Snapshot) {
        match s {
            Snapshot::Memory(bytes) => self.in_memory -= bytes.len(),
            Snapshot::File(path) => {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

impl Drop for UndoStack {
    fn drop(&mut self) {
        if self.dir.exists() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// Where the undo steps of the documents this Nook has open go: a folder of its own under the
/// computer's temporary folder.
pub fn root() -> PathBuf {
    std::env::temp_dir().join("Nook").join("pdf-undo")
}

/// Removes the undo folders a Nook that ended without closing its documents left behind.
pub fn sweep(root: &Path) {
    let mine = format!("{}-", std::process::id());
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs() > 3600);
        if !name.starts_with(&mine) && old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_steps_go_to_disk_and_come_back_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("doc");
        let mut u = UndoStack::with_limits(dir.clone(), 4, 25);
        for i in 0..6u8 {
            u.push(vec![i; 10]);
        }
        assert_eq!(u.len(), 4, "the depth holds");
        assert!(
            u.memory() <= 25,
            "memory stays within the budget: {}",
            u.memory()
        );
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            2,
            "two steps on disk"
        );
        for i in (2..6u8).rev() {
            assert_eq!(u.pop().unwrap(), Some(vec![i; 10]));
        }
        assert_eq!(u.pop().unwrap(), None);
        assert!(u.is_empty());
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "read steps are removed"
        );
        assert_eq!(u.push(vec![9; 40]), 0);
        assert_eq!(
            u.memory(),
            0,
            "one step over the budget goes to disk at once"
        );
        drop(u);
        assert!(!dir.exists(), "closing removes the folder");
    }

    #[test]
    fn with_no_room_on_disk_older_steps_go_and_the_last_one_stays() {
        let tmp = tempfile::tempdir().unwrap();
        // A file where the folder would be: nothing can be written there.
        let dir = tmp.path().join("doc");
        std::fs::write(&dir, b"in the way").unwrap();
        let mut u = UndoStack::with_limits(dir, 10, 25);
        assert_eq!(u.push(vec![1; 10]), 0);
        assert_eq!(u.push(vec![2; 10]), 0);
        assert_eq!(u.push(vec![3; 10]), 1, "the oldest could not go to disk");
        assert_eq!(u.len(), 2);
        assert_eq!(u.push(vec![4; 40]), 2, "both older ones went");
        assert_eq!(u.len(), 1);
        assert_eq!(u.memory(), 40, "the last one is held over the budget");
        assert_eq!(u.pop().unwrap(), Some(vec![4; 40]));
        assert_eq!(u.pop().unwrap(), None);
    }
}

//! Ports `update/BusyWork.java`: a long-running job that an automatic update must not cut off.
//! The updater installs a dev build only when every registered [`BusyWork`] says it is free.

pub trait BusyWork: Send + Sync {
    /// What it is doing, in a few words ("a model is downloading"), or None when idle.
    fn busy_with(&self) -> Option<String>;
}

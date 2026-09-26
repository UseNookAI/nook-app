//! Ports `update/BuildInfo.java`: what this build is. The version is the workspace version, or
//! `NOOK_VERSION` when the pipeline stamps a published build; the commit and time come from build.rs.

use std::cmp::Ordering;

use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildInfo {
    pub version: String,
    pub commit: String,
    pub time: Option<DateTime<Utc>>,
}

impl BuildInfo {
    pub fn current() -> BuildInfo {
        let version = option_env!("NOOK_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
        BuildInfo::of(version, env!("NOOK_BUILD_COMMIT"), env!("NOOK_BUILD_TIME"))
    }

    pub fn of(version: &str, commit: &str, epoch_secs: &str) -> BuildInfo {
        let time = epoch_secs
            .trim()
            .parse::<i64>()
            .ok()
            .and_then(|s| DateTime::from_timestamp(s, 0));
        let commit = if commit.trim().is_empty() || commit == "unknown" {
            "local"
        } else {
            commit.trim()
        };
        let version = if version.trim().is_empty() {
            "0.0.0"
        } else {
            version.trim()
        };
        BuildInfo {
            version: version.to_string(),
            commit: commit.to_string(),
            time,
        }
    }

    /// "0.5.0 (396a3b4, 2026-09-22)" for the About page.
    pub fn label(&self) -> String {
        let when = self
            .time
            .map(|t| format!(", {}", t.format("%Y-%m-%d")))
            .unwrap_or_default();
        format!("{} ({}{})", self.version, self.commit, when)
    }
}

/// Dotted numbers compared part by part, missing parts as 0, anything after `-`, `+` or a space
/// ignored. Greater when `a` is newer.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let (x, y) = (parts(a), parts(b));
    for i in 0..x.len().max(y.len()) {
        let p = x.get(i).copied().unwrap_or(0);
        let q = y.get(i).copied().unwrap_or(0);
        if p != q {
            return p.cmp(&q);
        }
    }
    Ordering::Equal
}

fn parts(v: &str) -> Vec<u64> {
    let core = v.trim();
    let core = core.split(['-', '+', ' ']).next().unwrap_or("");
    if core.is_empty() {
        return Vec::new();
    }
    core.split('.')
        .map(|b| b.trim().parse().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(compare_versions("0.4.10", "0.4.9"), Ordering::Greater);
        assert_eq!(compare_versions("0.5", "0.5.0"), Ordering::Equal);
        assert_eq!(compare_versions("0.5.0-dev", "0.5.0"), Ordering::Equal);
        assert_eq!(compare_versions("0.4.3", "0.5.0"), Ordering::Less);
    }

    #[test]
    fn label_has_commit_and_date() {
        let b = BuildInfo::of("0.5.0", "abc1234", "1758758400");
        assert_eq!(b.label(), "0.5.0 (abc1234, 2025-09-25)");
        assert_eq!(BuildInfo::of("0.5.0", "", "").label(), "0.5.0 (local)");
    }
}

//! The one-time import from the Kotlin Nook this app replaces.
//!
//! The Kotlin Nook kept its data in `%LOCALAPPDATA%\Nook`; this app keeps its own in
//! `%LOCALAPPDATA%\Nook-rs` and never writes the old folder. The first time this app starts on a
//! machine where the old one was used, its home is new (no settings, no Code sessions) and
//! [`import_once`], called by [`crate::Nook::new`] before any service reads its files, brings over:
//!
//! - the Code sessions, `code\sessions\*.json` (the same JSON: `CodeSession.java`, read by
//!   [`crate::code::code_session`]), and the editor's `code\ide.json`. A session's scratch copy
//!   stays in the old folder: its `worktree` is pointed at the place this app keeps scratch
//!   copies, and the first request, Apply or Undo makes it again from the repository with what
//!   was applied and the pending change put back (the Code service's recovery of a lost copy);
//! - `runtime\workers.json` (which model does which work), `runtime\probe.json` (the measured
//!   speeds) and `web.json` (the worker's web access);
//! - the engines, `runtime\bin`, as hard links (copies on another volume), so nothing is
//!   downloaded again; the Flows engines of 0.4.3 (`bin\ffmpeg`, `bin\<backend>\audio`) are left
//!   out, as this app has no Flows.
//!
//! Downloaded models are not copied: [`Home::shared_models_dir`] reads them where they are. The
//! settings in the old H2 database are not read; when the old home shows the app was used (a
//! session, the editor's state or a worker choice), setup counts as done, so the welcome screen,
//! which only greets, is not shown again.
//!
//! Nothing in the old folder is changed. A marker, `<home>\data\kotlin-import.json`, records what
//! was imported (or why nothing was), so the import runs once; a home that is in use already gets
//! the marker without an import.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::home::HOME_ENV;
use crate::settings::{Settings, IS_SETUP_COMPLETED, SETUP_VERSION};
use crate::Home;

/// The marker in `<home>\data`.
pub const MARKER: &str = "kotlin-import.json";
/// Names the Kotlin Nook's home to import from (tests, sandboxes); empty turns the import off.
pub const KOTLIN_HOME_ENV: &str = "NOOK_RS_KOTLIN_HOME";
/// The Kotlin Nook's home under %LOCALAPPDATA%.
pub const KOTLIN_HOME_DIR_NAME: &str = "Nook";
/// The welcome screen's flow version the Kotlin Nook's users went through (`WelcomeScreen.kt`;
/// `WELCOME_FLOW_VERSION` in `ui/src/screens/welcome/WelcomeScreen.tsx`).
const WELCOME_FLOW_VERSION: &str = "1";
/// Files copied as they are, relative to both homes.
const FILES: &[&str] = &[
    "code/ide.json",
    "runtime/workers.json",
    "runtime/probe.json",
    "web.json",
];
/// Engines of the Kotlin Nook 0.4.3's Flows, which this app does not have: `bin\ffmpeg` and
/// `bin\<backend>\audio`.
const FLOWS_ENGINES: &[&str] = &["ffmpeg", "audio"];

/// What an import did, as the marker keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    /// The Kotlin Nook's home.
    pub from: String,
    /// When, RFC 3339.
    pub at: String,
    /// Why nothing was imported, when nothing was.
    pub skipped: Option<String>,
    /// Code sessions copied.
    pub sessions: usize,
    /// Of those, sessions whose scratch copy will be made again here.
    pub repointed: usize,
    /// The other files copied, relative to the home.
    pub files: Vec<String>,
    /// Engine files brought over, and how many of them as hard links.
    pub engine_files: usize,
    pub engine_links: usize,
    /// Whether setup was marked done (the old home shows the app was used).
    pub setup_completed: bool,
    /// What could not be imported.
    pub problems: Vec<String>,
}

/// The Kotlin Nook's home: `NOOK_RS_KOTLIN_HOME` (empty: none); none when `NOOK_RS_HOME` moves this
/// app's home (a sandbox or a test, which must not pick up the machine's data); else
/// `%LOCALAPPDATA%\Nook`.
pub fn kotlin_home() -> Option<PathBuf> {
    let var = |name: &str| std::env::var(name).ok();
    kotlin_home_from(var(KOTLIN_HOME_ENV), var(HOME_ENV), var("LOCALAPPDATA"))
}

/// [`kotlin_home`] from the values of `NOOK_RS_KOTLIN_HOME`, `NOOK_RS_HOME` and `LOCALAPPDATA`.
fn kotlin_home_from(
    kotlin: Option<String>,
    home: Option<String>,
    local: Option<String>,
) -> Option<PathBuf> {
    if let Some(k) = kotlin {
        return (!k.trim().is_empty()).then(|| PathBuf::from(k.trim()));
    }
    if home.is_some_and(|h| !h.trim().is_empty()) {
        return None;
    }
    let local = local.filter(|l| !l.trim().is_empty())?;
    Some(PathBuf::from(local.trim()).join(KOTLIN_HOME_DIR_NAME))
}

/// Imports from the Kotlin Nook's home `old` into `home` when that is due: not done before, the old
/// home there, this one new. Returns what was written to the marker, or None when there was nothing
/// to decide (done before, or no old home).
pub fn import_once(home: &Home, old: Option<&Path>) -> Option<ImportReport> {
    let marker = home.data_dir().join(MARKER);
    if marker.exists() {
        return None;
    }
    let old = old.filter(|o| o.is_dir() && !same_dir(o, home.root()))?;
    let report = if in_use(home) {
        tracing::info!(
            "Not importing the Kotlin Nook's data from {}: {} is in use already",
            old.display(),
            home.root().display()
        );
        ImportReport {
            from: old.display().to_string(),
            at: now(),
            skipped: Some("this home was in use already".into()),
            ..ImportReport::default()
        }
    } else {
        import(old, home)
    };
    let written = serde_json::to_vec_pretty(&report)
        .map_err(anyhow::Error::from)
        .and_then(|json| {
            std::fs::create_dir_all(home.data_dir())?;
            crate::settings::write_atomic(&marker, &json)
        });
    if let Err(e) = written {
        tracing::warn!("Could not write {}: {e:#}", marker.display());
    }
    Some(report)
}

/// True when this home has settings or a Code session already.
fn in_use(home: &Home) -> bool {
    home.settings_file().exists() || !session_files(&home.code_dir().join("sessions")).is_empty()
}

/// Everything [`import_once`] brings over, each part on its own: a part that fails is logged and
/// kept in the report, and the rest goes on.
fn import(old: &Path, home: &Home) -> ImportReport {
    let mut r = ImportReport {
        from: old.display().to_string(),
        at: now(),
        ..ImportReport::default()
    };
    tracing::info!(
        "Importing the Kotlin Nook's data from {} (it stays there as it is)",
        old.display()
    );
    import_sessions(old, home, &mut r);
    for rel in FILES {
        let (from, to) = (at(old, rel), at(home.root(), rel));
        if !from.is_file() || to.exists() {
            continue;
        }
        match copy_file(&from, &to) {
            Ok(()) => r.files.push((*rel).to_string()),
            Err(e) => problem(&mut r, format!("{rel}: {e:#}")),
        }
    }
    import_engines(old, home, &mut r);
    let used = r.sessions > 0
        || r.files
            .iter()
            .any(|f| f == "code/ide.json" || f == "runtime/workers.json");
    if used {
        match Settings::load(home.settings_file()).and_then(|s| {
            s.set(IS_SETUP_COMPLETED, "true")?;
            s.set(SETUP_VERSION, WELCOME_FLOW_VERSION)
        }) {
            Ok(()) => r.setup_completed = true,
            Err(e) => problem(&mut r, format!("settings: {e:#}")),
        }
    }
    tracing::info!(
        "Imported from the Kotlin Nook: {} Code session{} ({} to have their scratch copy made again here), {}, {} engine file{} ({} as hard links){}",
        r.sessions,
        if r.sessions == 1 { "" } else { "s" },
        r.repointed,
        if r.files.is_empty() {
            "no other files".to_string()
        } else {
            r.files.join(", ")
        },
        r.engine_files,
        if r.engine_files == 1 { "" } else { "s" },
        r.engine_links,
        if r.setup_completed {
            "; setup counts as done"
        } else {
            ""
        }
    );
    r
}

/// The Code sessions, each with its scratch copy pointed at this home's scratch folder when it was
/// in the old home's. One that is not JSON is copied as it is: the store skips it with a warning,
/// as the old app did.
fn import_sessions(old: &Path, home: &Home, r: &mut ImportReport) {
    let from = old.join("code").join("sessions");
    let to = home.code_dir().join("sessions");
    let old_scratch = old.join("tmp").join("code");
    let new_scratch = home.temp_dir().join("code");
    for f in session_files(&from) {
        let Some(name) = f.file_name() else { continue };
        let target = to.join(name);
        if target.exists() {
            continue;
        }
        let copied = std::fs::read(&f)
            .with_context(|| format!("Could not read {}", f.display()))
            .and_then(|bytes| {
                let (bytes, repointed) = repoint(&bytes, &old_scratch, &new_scratch);
                std::fs::create_dir_all(&to)?;
                crate::settings::write_atomic(&target, &bytes)?;
                Ok(repointed)
            });
        match copied {
            Ok(repointed) => {
                r.sessions += 1;
                r.repointed += usize::from(repointed);
            }
            Err(e) => problem(r, format!("{}: {e:#}", name.to_string_lossy())),
        }
    }
}

/// A session's JSON with `worktree` moved from `old_scratch` to `new_scratch`, and whether it moved.
/// Anything else, and a file that is not a JSON object, is left as it was.
fn repoint(bytes: &[u8], old_scratch: &Path, new_scratch: &Path) -> (Vec<u8>, bool) {
    let Ok(Value::Object(mut session)) =
        serde_json::from_slice::<Value>(crate::settings::strip_bom(bytes))
    else {
        return (bytes.to_vec(), false);
    };
    let moved = session
        .get("worktree")
        .and_then(Value::as_str)
        .and_then(|w| relative_to(Path::new(w), old_scratch))
        .map(|rest| new_scratch.join(rest));
    let Some(moved) = moved else {
        return (bytes.to_vec(), false);
    };
    session.insert(
        "worktree".into(),
        Value::String(moved.to_string_lossy().into_owned()),
    );
    match serde_json::to_vec(&Value::Object(session)) {
        Ok(out) => (out, true),
        Err(_) => (bytes.to_vec(), false),
    }
}

/// The engines under `runtime\bin`, file by file: a hard link where the volume allows one, else a
/// copy. A file this home has already is left alone.
fn import_engines(old: &Path, home: &Home, r: &mut ImportReport) {
    let from = old.join("runtime").join("bin");
    if !from.is_dir() {
        return;
    }
    let to = home.runtime_dir().join("bin");
    let walk = walkdir::WalkDir::new(&from)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            !(matches!(e.depth(), 1 | 2)
                && e.file_type().is_dir()
                && FLOWS_ENGINES
                    .iter()
                    .any(|n| e.file_name().eq_ignore_ascii_case(n)))
        });
    for e in walk {
        let e = match e {
            Ok(e) => e,
            Err(e) => {
                problem(r, format!("runtime/bin: {e}"));
                continue;
            }
        };
        let Ok(rel) = e.path().strip_prefix(&from) else {
            continue;
        };
        let target = to.join(rel);
        let done = if e.file_type().is_dir() {
            std::fs::create_dir_all(&target).map(|_| None)
        } else if e.file_type().is_file() && !target.exists() {
            link_or_copy(e.path(), &target).map(Some)
        } else {
            Ok(None)
        };
        match done {
            Ok(Some(linked)) => {
                r.engine_files += 1;
                r.engine_links += usize::from(linked);
            }
            Ok(None) => {}
            Err(e) => problem(r, format!("runtime/bin/{}: {e}", rel.display())),
        }
    }
}

/// A hard link of `from` at `to` (true), or a copy where that cannot be (false).
fn link_or_copy(from: &Path, to: &Path) -> std::io::Result<bool> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::hard_link(from, to) {
        Ok(()) => Ok(true),
        Err(_) => std::fs::copy(from, to).map(|_| false),
    }
}

/// Copies a file's bytes (not its attributes: a read-only old file must not make a read-only new one).
fn copy_file(from: &Path, to: &Path) -> Result<()> {
    let bytes =
        std::fs::read(from).with_context(|| format!("Could not read {}", from.display()))?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::settings::write_atomic(to, &bytes)
}

/// The `*.json` files in a folder, sorted; none when it cannot be read.
fn session_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("json"))
        })
        .collect();
    files.sort();
    files
}

/// `path` below `base`, compared part by part (without case on Windows); None when it is not.
fn relative_to(path: &Path, base: &Path) -> Option<PathBuf> {
    let mut rest = path.components();
    for b in base.components() {
        let p = rest.next()?;
        if !same_part(&p, &b) {
            return None;
        }
    }
    let rest: PathBuf = rest.collect();
    (!rest.as_os_str().is_empty()).then_some(rest)
}

fn same_part(a: &Component, b: &Component) -> bool {
    let (a, b) = (
        a.as_os_str().to_string_lossy(),
        b.as_os_str().to_string_lossy(),
    );
    if cfg!(windows) {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// `rel` ("a/b") under `root`.
fn at(root: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(root.to_path_buf(), |p, part| p.join(part))
}

fn problem(r: &mut ImportReport, what: String) {
    tracing::warn!("Could not import from the Kotlin Nook: {what}");
    r.problems.push(what);
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::code_store::CodeStore;
    use crate::ide::{prefs_file, IdePrefs};

    /// A Kotlin Nook home as 0.4.3 leaves it: two sessions (one with a scratch copy in its temp
    /// folder), the editor's state, the worker files, engines for one backend plus the Flows ones.
    fn kotlin_home(root: &Path) {
        let sessions = root.join("code").join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let scratch = root.join("tmp").join("code").join("k1");
        std::fs::create_dir_all(&scratch).unwrap();
        let k1 = serde_json::json!({
            "id": "k1", "title": "Add a greeting", "repository": "F:\\work\\app",
            "createdAt": 1_758_800_000_000_i64, "updatedAt": 1_758_800_100_000_i64,
            "worktree": scratch.to_string_lossy(), "baseCommit": "1234567", "baseline": null,
            "verify": "gradlew test",
            "change": {"diff": "diff --git a/A b/A\n", "cut": false, "stat": " 1 file changed"},
            "entries": [
                {"kind": "task", "id": "t1", "at": 1, "text": "Add a greeting"},
                {"kind": "run", "id": "u1", "at": 2, "model": "gpt-oss-20b", "running": false,
                 "steps": ["Read A"], "summary": "Added it.", "stat": " 1 file changed",
                 "verified": true, "verifyCommand": "gradlew test", "verifyNote": null,
                 "verifyOutput": null, "gaveUp": null, "toolCalls": 3, "seconds": 41,
                 "error": null, "before": "abc", "undone": false, "after": "def",
                 "context": {"used": 3000, "peak": 3100, "window": 8192, "dropped": 0, "measured": true}},
                {"kind": "note", "id": "n1", "at": 3, "text": "Applied 1 file.", "tone": "applied"}
            ]
        });
        std::fs::write(sessions.join("k1.json"), k1.to_string()).unwrap();
        let k2 = serde_json::json!({
            "id": "k2", "title": "Explain main", "repository": "F:\\work\\other",
            "createdAt": 5, "updatedAt": 6, "worktree": null, "baseCommit": null, "baseline": null,
            "verify": null, "change": null,
            "entries": [{"kind": "task", "id": "t1", "at": 5, "text": "Explain main"}]
        });
        std::fs::write(sessions.join("k2.json"), k2.to_string()).unwrap();
        std::fs::write(
            root.join("code").join("ide.json"),
            r#"{"folder":"F:\\work\\app","open":[],"active":null,"explorerWidth":300,"assistantWidth":420,"explorerOpen":true,"assistantOpen":true,"sessions":{"F:\\work\\app":"k1"}}"#,
        )
        .unwrap();
        let runtime = root.join("runtime");
        std::fs::create_dir_all(runtime.join("bin").join("cuda").join("whisper")).unwrap();
        std::fs::create_dir_all(runtime.join("bin").join("cuda").join("audio")).unwrap();
        std::fs::create_dir_all(runtime.join("bin").join("ffmpeg")).unwrap();
        std::fs::write(runtime.join("workers.json"), r#"{"code": "gpt-oss-20b"}"#).unwrap();
        std::fs::write(
            runtime.join("probe.json"),
            r#"{"gpt-oss-20b": {"tps": 42.0}}"#,
        )
        .unwrap();
        std::fs::write(root.join("web.json"), r#"{"enabled": true}"#).unwrap();
        let bin = runtime.join("bin");
        std::fs::write(bin.join("cuda").join("llama-server.exe"), "llama").unwrap();
        std::fs::write(
            bin.join("cuda").join("installed.json"),
            r#"{"version":"b1","backend":"cuda","component":"llama"}"#,
        )
        .unwrap();
        std::fs::write(
            bin.join("cuda").join("whisper").join("whisper-server.exe"),
            "whisper",
        )
        .unwrap();
        std::fs::write(bin.join("cuda").join("audio").join("audiocpp_cli.exe"), "a").unwrap();
        std::fs::write(bin.join("ffmpeg").join("ffmpeg.exe"), "f").unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("data").join("nook.mv.db"), "h2").unwrap();
    }

    /// Every file under `root` with its bytes, to see nothing changed.
    fn contents(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut all: Vec<_> = walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| (e.path().to_path_buf(), std::fs::read(e.path()).unwrap()))
            .collect();
        all.sort();
        all
    }

    #[test]
    fn a_new_home_gets_the_sessions_the_worker_files_and_the_engines() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("Nook");
        kotlin_home(&old);
        let before = contents(&old);
        let home = Home::at(tmp.path().join("Nook-rs"));
        home.ensure_layout().unwrap();

        let r = import_once(&home, Some(&old)).expect("imported");
        assert_eq!(None, r.skipped);
        assert_eq!((2, 1), (r.sessions, r.repointed));
        assert_eq!(
            vec![
                "code/ide.json",
                "runtime/workers.json",
                "runtime/probe.json",
                "web.json"
            ],
            r.files
        );
        assert_eq!(
            3, r.engine_files,
            "llama, its marker and whisper; no Flows engines"
        );
        assert_eq!(3, r.engine_links, "the same volume: hard links");
        assert!(r.setup_completed);
        assert!(r.problems.is_empty(), "{:?}", r.problems);
        assert_eq!(before, contents(&old), "the old home is left as it was");

        // The sessions read as this app's, the first one's scratch copy to be made here.
        let sessions = CodeStore::new(home.code_dir().join("sessions")).load_all();
        assert_eq!(
            vec!["k1", "k2"],
            sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>()
        );
        assert_eq!(
            Some(
                home.temp_dir()
                    .join("code")
                    .join("k1")
                    .to_string_lossy()
                    .into_owned()
            ),
            sessions[0].worktree
        );
        assert_eq!(3, sessions[0].entries.len());
        assert_eq!(None, sessions[1].worktree);
        assert_eq!(
            Some("k1"),
            IdePrefs::load(&prefs_file(&home))
                .sessions
                .get("F:\\work\\app")
                .map(String::as_str)
        );
        assert_eq!(
            "{\"code\": \"gpt-oss-20b\"}",
            std::fs::read_to_string(home.runtime_dir().join("workers.json")).unwrap()
        );

        // The engines, where the runtime looks for them; the Flows ones stay behind.
        let bin = home.runtime_dir().join("bin");
        assert_eq!(
            "llama",
            std::fs::read_to_string(bin.join("cuda").join("llama-server.exe")).unwrap()
        );
        assert!(bin.join("cuda").join("installed.json").is_file());
        assert!(bin
            .join("cuda")
            .join("whisper")
            .join("whisper-server.exe")
            .is_file());
        assert!(!bin.join("cuda").join("audio").exists());
        assert!(!bin.join("ffmpeg").exists());

        // Setup counts as done; the rest of the settings are the defaults.
        let settings = Settings::load(home.settings_file()).unwrap();
        assert!(settings.get_bool(IS_SETUP_COMPLETED));
        assert_eq!(Some("1".to_string()), settings.get(SETUP_VERSION));
        assert_eq!(
            Some("stable".to_string()),
            settings.get(crate::settings::UPDATE_CHANNEL)
        );

        // An engine this app replaces later goes without touching the old one.
        std::fs::remove_file(bin.join("cuda").join("llama-server.exe")).unwrap();
        assert!(old
            .join("runtime")
            .join("bin")
            .join("cuda")
            .join("llama-server.exe")
            .is_file());
    }

    #[test]
    fn it_runs_once() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("Nook");
        kotlin_home(&old);
        let home = Home::at(tmp.path().join("Nook-rs"));
        home.ensure_layout().unwrap();
        assert!(import_once(&home, Some(&old)).is_some());
        let marker = home.data_dir().join(MARKER);
        let written: Value = serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
        assert_eq!(2, written["sessions"]);
        assert_eq!(old.display().to_string(), written["from"]);

        // Deleted here, a session does not come back at the next start.
        std::fs::remove_file(home.code_dir().join("sessions").join("k2.json")).unwrap();
        std::fs::remove_file(home.settings_file()).unwrap();
        assert_eq!(None, import_once(&home, Some(&old)));
        assert!(!home.code_dir().join("sessions").join("k2.json").exists());
    }

    #[test]
    fn a_home_in_use_is_left_as_it_is() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("Nook");
        kotlin_home(&old);
        let home = Home::at(tmp.path().join("Nook-rs"));
        home.ensure_layout().unwrap();
        Settings::load(home.settings_file()).unwrap();

        let r = import_once(&home, Some(&old)).expect("decided");
        assert_eq!(Some("this home was in use already".to_string()), r.skipped);
        assert_eq!(0, r.sessions);
        assert!(!home.code_dir().join("sessions").exists());
        assert!(home.data_dir().join(MARKER).is_file());

        // One with a session of its own, likewise.
        let other = Home::at(tmp.path().join("other"));
        other.ensure_layout().unwrap();
        std::fs::create_dir_all(other.code_dir().join("sessions")).unwrap();
        std::fs::write(other.code_dir().join("sessions").join("mine.json"), "{}").unwrap();
        assert!(import_once(&other, Some(&old)).unwrap().skipped.is_some());
        assert!(!other.runtime_dir().join("workers.json").exists());
    }

    #[test]
    fn without_an_old_home_nothing_happens() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("Nook-rs"));
        home.ensure_layout().unwrap();
        assert_eq!(None, import_once(&home, None));
        assert_eq!(None, import_once(&home, Some(&tmp.path().join("Nook"))));
        assert_eq!(
            None,
            import_once(&home, Some(home.root())),
            "never from itself"
        );
        assert!(!home.data_dir().join(MARKER).exists(), "decided nothing");
        assert!(!home.settings_file().exists());
    }

    #[test]
    fn an_unused_old_home_brings_engines_but_not_setup() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("Nook");
        std::fs::create_dir_all(old.join("runtime").join("bin").join("vulkan")).unwrap();
        std::fs::write(
            old.join("runtime")
                .join("bin")
                .join("vulkan")
                .join("llama-server.exe"),
            "x",
        )
        .unwrap();
        std::fs::create_dir_all(old.join("code").join("sessions")).unwrap();
        std::fs::write(
            old.join("code").join("sessions").join("broken.json"),
            "{ no",
        )
        .unwrap();
        let home = Home::at(tmp.path().join("Nook-rs"));
        home.ensure_layout().unwrap();

        let r = import_once(&home, Some(&old)).unwrap();
        assert_eq!(1, r.engine_files);
        assert_eq!(1, r.sessions, "copied as it is");
        assert_eq!(0, r.repointed);
        assert_eq!(
            "{ no",
            std::fs::read_to_string(home.code_dir().join("sessions").join("broken.json")).unwrap()
        );
        assert!(CodeStore::new(home.code_dir().join("sessions"))
            .load_all()
            .is_empty());
        assert!(r.setup_completed, "a session file says the app was used");
    }

    #[test]
    fn only_a_scratch_copy_in_the_old_temp_folder_moves() {
        let old = Path::new(r"C:\Users\a\AppData\Local\Nook\tmp\code");
        let new = Path::new(r"C:\Users\a\AppData\Local\Nook-rs\tmp\code");
        let json = |w: &str| {
            serde_json::to_vec(&serde_json::json!({"id": "x", "worktree": w, "entries": []}))
                .unwrap()
        };
        let (out, moved) = repoint(&json(r"C:\Users\a\AppData\Local\Nook\tmp\code\x"), old, new);
        assert!(moved);
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            new.join("x").to_string_lossy(),
            v["worktree"].as_str().unwrap()
        );
        assert_eq!("x", v["id"]);
        if cfg!(windows) {
            assert!(repoint(&json(r"c:\users\A\appdata\local\nook\TMP\code\x"), old, new).1);
        }
        let elsewhere = json(r"D:\scratch\x");
        assert_eq!((elsewhere.clone(), false), repoint(&elsewhere, old, new));
        assert!(!repoint(&json(r"C:\Users\a\AppData\Local\Nook\tmp\codex"), old, new).1);
        assert!(!repoint(b"not json", old, new).1);
    }

    #[test]
    fn the_old_home_is_found_beside_this_one_unless_a_test_home_is_set() {
        let s = |v: &str| Some(v.to_string());
        let local = s(r"C:\Users\a\AppData\Local");
        assert_eq!(
            Some(PathBuf::from(r"C:\Users\a\AppData\Local").join("Nook")),
            kotlin_home_from(None, None, local.clone())
        );
        assert_eq!(
            None,
            kotlin_home_from(None, s(r"F:\sandbox\home"), local.clone()),
            "a sandbox or a test does not pick up the machine's data"
        );
        assert_eq!(
            Some(PathBuf::from(r"F:\old")),
            kotlin_home_from(s(r" F:\old "), s(r"F:\sandbox\home"), local.clone())
        );
        assert_eq!(None, kotlin_home_from(s(""), None, local), "turned off");
        assert_eq!(None, kotlin_home_from(None, s(" "), None));
    }
}

//! The path rules Code mode applies to the folder it is asked to work on: paths must be absolute
//! and exist, and a denylist refuses the Windows and Program Files directories, the user's
//! credential stores and Nook's own gateway.json.
//!
//! Ports `worker/PathPolicy.java`. Paths are compared the way Windows (and Java's Windows paths)
//! compare them: without case, component by component, after making them absolute and dropping
//! `.` and `..`.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Result};
use once_cell::sync::Lazy;
use regex::Regex;

pub struct PathPolicy {
    denied: Vec<PathBuf>,
    denied_files: Vec<PathBuf>,
    /// Folders that may hold a repository but are not one: reading them whole reads everything.
    broad: Vec<PathBuf>,
    user_home: Option<PathBuf>,
}

impl PathPolicy {
    /// The rules for this user and this Nook home.
    pub fn new(nook_home: &Path) -> PathPolicy {
        let user_home = std::env::var("USERPROFILE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| std::env::var("HOME").ok().filter(|v| !v.trim().is_empty()));
        PathPolicy::from_env(
            |name| std::env::var(name).ok(),
            user_home.as_deref(),
            Some(nook_home),
        )
    }

    /// The rules for the environment `env` answers (tests pass their own).
    pub(crate) fn from_env(
        env: impl Fn(&str) -> Option<String>,
        user_home: Option<&str>,
        nook_home: Option<&Path>,
    ) -> PathPolicy {
        let mut policy = PathPolicy {
            denied: Vec::new(),
            denied_files: Vec::new(),
            broad: Vec::new(),
            user_home: None,
        };
        for name in [
            "SystemRoot",
            "windir",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "ProgramData",
        ] {
            add(&mut policy.denied, env(name).as_deref());
        }
        if let Some(home) = user_home.filter(|h| !h.trim().is_empty()) {
            let home = Path::new(home);
            policy.user_home = Some(absolute(home));
            for d in [
                "Desktop",
                "Documents",
                "Downloads",
                "Pictures",
                "Videos",
                "Music",
                "OneDrive",
            ] {
                policy.broad.push(absolute(&home.join(d)));
            }
            // Cloud and SSH credentials, and the AI tools' own homes: Claude Code keeps OAuth tokens
            // under ~/.claude and MCP bearer headers in ~/.claude.json; Cursor and Codex keep theirs
            // in ~/.cursor and ~/.codex. A prompt-injected worker must not be able to read the
            // person's keys.
            for d in [
                ".ssh",
                ".gnupg",
                ".aws",
                ".azure",
                ".kube",
                ".docker",
                ".gcloud",
                ".config",
                ".claude",
                ".cursor",
                ".codex",
                // more places a private key or a token sits in a home directory: the Android debug
                // key was found in an index of this desk's home on 2026-09-22, and every one of
                // these has been somebody's leak at some point
                ".android",
                ".m2",
                ".gradle",
                ".nuget",
                ".oci",
                ".chef",
                ".vagrant.d",
                ".terraform.d",
                ".cargo",
                ".npm",
                ".yarn",
                ".bundle",
                ".subversion",
                ".vscode",
                ".vscode-server",
                ".jupyter",
                ".ipython",
                ".dbt",
                ".snowflake",
                ".databricks",
                ".vault",
                ".pki",
            ] {
                add_path(&mut policy.denied, &home.join(d));
            }
            for f in [
                ".claude.json",
                ".netrc",
                ".git-credentials",
                ".npmrc",
                ".pypirc",
                ".pgpass",
                ".my.cnf",
                ".vault-token",
                ".rclone.conf",
                ".boto",
                ".s3cfg",
                ".dockercfg",
                ".gitconfig",
                ".irb_history",
                ".bash_history",
                ".zsh_history",
                ".psql_history",
            ] {
                add_path(&mut policy.denied_files, &home.join(f));
            }
        }
        // A Mac's system folders, and the stores in a Mac home's Library where keys, cookies,
        // mail and the same browsers' and tools' secrets are (the Library itself holds Nook's own
        // home, the scratch copies included, so it is not closed whole).
        if cfg!(target_os = "macos") {
            for d in [
                "/System",
                "/Library",
                "/Applications",
                "/usr",
                "/bin",
                "/sbin",
                "/private/etc",
                "/private/var/db",
                "/private/var/root",
            ] {
                add(&mut policy.denied, Some(d));
            }
            if let Some(home) = user_home.filter(|h| !h.trim().is_empty()) {
                let home = Path::new(home);
                for d in ["Movies", "Public"] {
                    policy.broad.push(absolute(&home.join(d)));
                }
                let library = home.join("Library");
                for d in [
                    "Keychains",
                    "Cookies",
                    "Safari",
                    "Mail",
                    "Messages",
                    "Accounts",
                    "Containers",
                    "Group Containers",
                    "Application Support/Google/Chrome",
                    "Application Support/Microsoft Edge",
                    "Application Support/BraveSoftware",
                    "Application Support/Firefox",
                    "Application Support/Claude",
                    "Application Support/Code/User",
                    "Application Support/Cursor/User",
                    "Application Support/1Password",
                    "Application Support/Bitwarden",
                    "Application Support/gcloud",
                ] {
                    add_path(&mut policy.denied, &library.join(d));
                }
            }
        }
        if let Some(app_data) = env("APPDATA").filter(|v| !v.trim().is_empty()) {
            // Roaming credential stores: Windows vaults, browser profiles, cloud CLIs, password
            // managers, and the editors' user folders (Claude Desktop config, VS Code and Cursor
            // secrets).
            for d in [
                "Microsoft\\Credentials",
                "Microsoft\\Protect",
                "Microsoft\\Crypto",
                "Mozilla",
                "Thunderbird",
                "gcloud",
                "Bitwarden",
                "1Password",
                "KeePass",
                "Claude",
                "Code\\User",
                "Cursor\\User",
            ] {
                add_path(&mut policy.denied, &Path::new(&app_data).join(d));
            }
        }
        if let Some(local) = env("LOCALAPPDATA").filter(|v| !v.trim().is_empty()) {
            for d in [
                "Microsoft\\Credentials",
                "Google\\Chrome\\User Data",
                "Microsoft\\Edge\\User Data",
                "BraveSoftware\\Brave-Browser\\User Data",
            ] {
                add_path(&mut policy.denied, &Path::new(&local).join(d));
            }
        }
        if let Some(nook) = nook_home {
            add_path(&mut policy.denied_files, &nook.join("gateway.json"));
            add_path(&mut policy.denied, &nook.join("data"));
            // What an older Nook left in the same home (session transcripts, the search index, the
            // evidence store) quotes other repositories: no tool may read one project from another.
            add_path(&mut policy.denied, &nook.join("memory"));
            add_path(&mut policy.denied, &nook.join("index"));
            add_path(&mut policy.denied, &nook.join("evidence"));
        }
        policy
    }

    /// Resolves and checks a path argument. Fails with a sentence the model can act on.
    pub fn check(&self, raw: &str, must_be_directory: bool) -> Result<PathBuf> {
        if raw.trim().is_empty() {
            bail!("A path is required.");
        }
        let temp = std::env::var("TEMP").ok();
        let given = from_posix_shell(
            raw.trim_matches(|c| c <= ' '),
            cfg!(windows),
            temp.as_deref(),
        );
        if !valid_path(&given) {
            bail!("Not a valid path: {raw}");
        }
        let p = PathBuf::from(given);
        if !p.is_absolute() {
            let example = if cfg!(windows) {
                "C:\\Users\\me\\project"
            } else {
                "/Users/me/project"
            };
            bail!("The path must be absolute (for example {example}), got: {raw}");
        }
        let p = absolute(&p);
        if self.is_denied(&p) {
            bail!(
                "Refused: {} is a system or credential location Nook does not read.",
                p.display()
            );
        }
        // and again on what the path really points at: a junction or a symlink inside an allowed
        // folder must not be a way into a denied one
        let real = PathPolicy::real(&p);
        if !same(&real, &p) && self.is_denied(&real) {
            bail!(
                "Refused: {} leads to {}, a system or credential location Nook does not read.",
                p.display(),
                real.display()
            );
        }
        if !p.exists() {
            bail!("Path does not exist: {}", p.display());
        }
        if must_be_directory && !p.is_dir() {
            bail!("Not a folder: {}", p.display());
        }
        if !must_be_directory && p.is_dir() {
            bail!("Expected a file but got a folder: {}", p.display());
        }
        Ok(p)
    }

    /// True when the path, as written or as it really points, is inside a denied directory or is a
    /// denied file. Both are judged: a link can lead into a denied place, and a denied place can
    /// hold a link out of it.
    pub fn is_denied(&self, p: &Path) -> bool {
        let norm = absolute(p);
        if self.denied_exactly(&norm) {
            return true;
        }
        let real = PathPolicy::real(&norm);
        !same(&real, &norm) && self.denied_exactly(&real)
    }

    /// The denial rule on the path as written, without following links. The walk of a folder asks
    /// this for every directory and file it meets, where a call per file would be too much; a link
    /// is caught separately by [`PathPolicy::is_link`].
    pub fn is_denied_as_written(&self, p: &Path) -> bool {
        self.denied_exactly(&absolute(p))
    }

    fn denied_exactly(&self, norm: &Path) -> bool {
        self.denied_files.iter().any(|f| same(f, norm))
            || self
                .denied
                .iter()
                .any(|d| starts_with(norm, d) || same(d, norm))
    }

    /// Why a folder is too broad to be read as a repository (a drive, a home directory, or one of
    /// the folders everything lands in), or None when it is not. Indexing one of these reads a
    /// person's whole machine, and whatever the denied list has not thought of goes into the index
    /// with it: an index of this desk's home on 2026-09-22 held 22,444 files and answered a search
    /// with an Android debug key. A project inside any of them is still fine to point at.
    pub fn too_broad_to_read(&self, p: &Path) -> Option<String> {
        let norm = absolute(p);
        if norm.parent().is_none() {
            return Some("a whole drive".to_string());
        }
        let real = PathPolicy::real(&norm);
        let either = |d: &Path| same(d, &norm) || same(&PathPolicy::real(d), &real);
        if self.user_home.as_deref().is_some_and(either) {
            return Some("your home folder".to_string());
        }
        self.broad.iter().find(|d| either(d)).map(|d| {
            format!(
                "the {} folder",
                d.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            )
        })
    }

    /// True when the path is a symbolic link, a Windows junction or any other reparse point (or
    /// device). A walk of somebody's repository has to skip these, or a link pointing at a
    /// credential folder puts its files in the index (2026-09-22). Java reported a junction as an
    /// ordinary directory and needed `isOther()` to see it; Rust's `is_symlink` sees junctions, and
    /// the attribute check covers the rest as `isOther()` did.
    pub fn is_link(p: &Path) -> bool {
        let Ok(meta) = std::fs::symlink_metadata(p) else {
            return false;
        };
        if meta.file_type().is_symlink() {
            return true;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_DEVICE: u32 = 0x40;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
            meta.file_attributes() & (FILE_ATTRIBUTE_DEVICE | FILE_ATTRIBUTE_REPARSE_POINT) != 0
        }
        #[cfg(not(windows))]
        {
            let t = meta.file_type();
            !t.is_file() && !t.is_dir()
        }
    }

    /// What a path really points at, with every link on the way followed. A path that does not
    /// exist yet (a file about to be written) is resolved as far as its nearest existing parent,
    /// so a write through a linked folder is judged by where it would land.
    pub fn real(p: &Path) -> PathBuf {
        let abs = absolute(p);
        let mut here: Option<&Path> = Some(&abs);
        let mut tail: Vec<&OsStr> = Vec::new();
        while let Some(h) = here {
            if let Ok(resolved) = std::fs::canonicalize(h) {
                let mut out = without_verbatim(resolved);
                for name in tail.iter().rev() {
                    out.push(name);
                }
                return normalize(&out);
            }
            match h.file_name() {
                Some(name) => tail.push(name),
                None => return abs.clone(),
            }
            here = h.parent();
        }
        abs.clone()
    }
}

static GIT_BASH_DRIVE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^/([A-Za-z])(/[^\n\r\u{85}\u{2028}\u{2029}]*)?$").expect("a valid pattern")
});

/// On Windows, a path as Git Bash prints it: `/c/Users/me` is `C:/Users/me` and `/tmp/x` is the
/// user's temp folder, so a path copied from Git Bash works (one was refused on 2026-09-23 for
/// `/tmp/nook-bench-...`). On Windows such a path is never absolute, so reading it this way cannot
/// change what a valid argument means. Anything else is returned as given.
pub fn from_posix_shell(raw: &str, windows: bool, temp: Option<&str>) -> String {
    if !windows || !raw.starts_with('/') {
        return raw.to_string();
    }
    if let Some(drive) = GIT_BASH_DRIVE.captures(raw) {
        let letter = drive
            .get(1)
            .map(|m| m.as_str().to_ascii_uppercase())
            .unwrap_or_default();
        let rest = drive.get(2).map(|m| m.as_str()).unwrap_or("/");
        return format!("{letter}:{rest}");
    }
    if let Some(temp) = temp.filter(|t| !t.trim().is_empty()) {
        if raw == "/tmp" || raw.starts_with("/tmp/") {
            return format!("{temp}{}", &raw[4..]);
        }
    }
    raw.to_string()
}

/// What Java refused as an invalid path: a NUL anywhere, and on Windows a control character or one
/// of `<>:"|?*` past the drive.
fn valid_path(s: &str) -> bool {
    if s.contains('\0') {
        return false;
    }
    if !cfg!(windows) {
        return true;
    }
    let b = s.as_bytes();
    let rest = if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        &s[2..]
    } else {
        s
    };
    !rest.chars().any(|c| c < ' ' || "<>:\"|?*".contains(c))
}

/// Adds a place to a list as written and, when it differs, as it really is on disk: Windows
/// spells a folder two ways (`C:\Users\RUNNER~1` and `C:\Users\runneradmin`, when the variable a
/// place came from uses its short name), and a link is judged by its long, resolved form, which
/// must still meet the denial.
fn add(list: &mut Vec<PathBuf>, value: Option<&str>) {
    match value {
        Some(v) if !v.trim().is_empty() && valid_path(v) => {
            let written = absolute(Path::new(v));
            let real = PathPolicy::real(&written);
            if !same(&real, &written) && !list.iter().any(|p| same(p, &real)) {
                list.push(real);
            }
            list.push(written);
        }
        _ => {} // missing, or an odd environment value; skip it
    }
}

fn add_path(list: &mut Vec<PathBuf>, p: &Path) {
    add(list, p.to_str());
}

/// Absolute and normalised (`toAbsolutePath().normalize()`): on Windows the separators become
/// backslashes and `.`/`..` go, as Windows itself reads the path.
pub(crate) fn absolute(p: &Path) -> PathBuf {
    normalize(&std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()))
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `\\?\C:\x` as `C:\x` and `\\?\UNC\server\share` as `\\server\share`: canonicalize's verbatim
/// form, which Java's toRealPath never produced.
fn without_verbatim(p: PathBuf) -> PathBuf {
    let Some(s) = p.to_str() else { return p };
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return PathBuf::from(rest);
        }
    }
    p
}

fn lower(s: &OsStr) -> String {
    s.to_string_lossy().to_lowercase()
}

/// The same path, ignoring case.
fn same(a: &Path, b: &Path) -> bool {
    lower(a.as_os_str()) == lower(b.as_os_str())
}

/// `p` is `prefix` or inside it, component by component and ignoring case.
fn starts_with(p: &Path, prefix: &Path) -> bool {
    let mut parts = p.components();
    prefix.components().all(|c| {
        parts
            .next()
            .is_some_and(|x| lower(x.as_os_str()) == lower(c.as_os_str()))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const TEMP: &str = "C:\\Users\\me\\AppData\\Local\\Temp";

    #[test]
    fn a_git_bash_path_is_read_as_the_windows_path_it_names() {
        assert_eq!(
            "C:/Users/me/project",
            from_posix_shell("/c/Users/me/project", true, Some(TEMP))
        );
        assert_eq!("F:/", from_posix_shell("/f", true, Some(TEMP)));
        assert_eq!(
            format!("{TEMP}/nook-bench-u0zqh50l/wt-M1"),
            from_posix_shell("/tmp/nook-bench-u0zqh50l/wt-M1", true, Some(TEMP))
        );
    }

    #[test]
    fn anything_else_is_left_as_given() {
        assert_eq!(
            "C:\\Users\\me",
            from_posix_shell("C:\\Users\\me", true, Some(TEMP))
        );
        assert_eq!(
            "/usr/lib",
            from_posix_shell("/usr/lib", true, Some(TEMP)),
            "no mapping for other roots: still refused as not absolute"
        );
        assert_eq!(
            "/tmpfolder",
            from_posix_shell("/tmpfolder", true, Some(TEMP))
        );
        assert_eq!(
            "/c/Users/me",
            from_posix_shell("/c/Users/me", false, Some(TEMP)),
            "on Linux or macOS the path is already absolute"
        );
    }

    /// A directory junction, the way in that needs no privilege on Windows; false when it cannot be made.
    fn junction(link: &Path, target: &Path) -> bool {
        #[cfg(windows)]
        {
            let _ = crate::process::std_command("cmd.exe")
                .args(["/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            link.exists()
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    fn unlink(link: &Path) {
        // the link, not what it points at; and the temp folder must not walk into it
        std::fs::remove_dir(link)
            .or_else(|_| std::fs::remove_file(link))
            .unwrap();
    }

    /// `p` by its short (8.3) name when Windows has one for it, as the temporary folder of a CI
    /// runner is (`C:\Users\RUNNER~1\...`); else `p` as it is.
    fn short_name(p: &Path) -> PathBuf {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::{OsStrExt, OsStringExt};
            let wide: Vec<u16> = p.as_os_str().encode_wide().chain([0]).collect();
            let mut buf = vec![0u16; 1024];
            // SAFETY: both buffers are NUL-terminated and sized as said.
            let n = unsafe {
                windows_sys::Win32::Storage::FileSystem::GetShortPathNameW(
                    wide.as_ptr(),
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                )
            } as usize;
            if n > 0 && n < buf.len() {
                return PathBuf::from(std::ffi::OsString::from_wide(&buf[..n]));
            }
        }
        p.to_path_buf()
    }

    #[test]
    fn path_policy_enforces_the_rules() {
        let dir = tempfile::tempdir().unwrap();
        // The places as a short name spells them (where Windows keeps short names), so the rules
        // hold whichever way a folder is written.
        let short = short_name(dir.path());
        let tmp = short.as_path();
        let home = tmp.join("nook");
        std::fs::create_dir_all(home.join("data")).unwrap();
        std::fs::write(home.join("gateway.json"), "{}").unwrap();
        let windows = tmp.join("Windows");
        let programs = tmp.join("Program Files");
        let user_home = tmp.join("me");
        let roaming = user_home.join("AppData").join("Roaming");
        std::fs::create_dir_all(windows.join("System32")).unwrap();
        std::fs::create_dir_all(programs.join("App")).unwrap();
        std::fs::create_dir_all(user_home.join(".ssh")).unwrap();
        std::fs::create_dir_all(roaming.join("Mozilla")).unwrap();
        let project = user_home.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("a.txt"), "hello").unwrap();
        std::fs::write(user_home.join(".ssh").join("id_rsa"), "secret").unwrap();
        // The AI tools' own homes hold OAuth tokens and bearer headers (QA finding 1).
        std::fs::create_dir_all(user_home.join(".claude")).unwrap();
        std::fs::write(user_home.join(".claude").join(".credentials.json"), "{}").unwrap();
        std::fs::write(user_home.join(".claude.json"), "{}").unwrap();
        std::fs::create_dir_all(user_home.join(".cursor")).unwrap();
        std::fs::write(user_home.join(".cursor").join("mcp.json"), "{}").unwrap();
        std::fs::create_dir_all(roaming.join("Claude")).unwrap();
        std::fs::write(
            roaming.join("Claude").join("claude_desktop_config.json"),
            "{}",
        )
        .unwrap();

        let env: HashMap<&str, String> = HashMap::from([
            ("SystemRoot", windows.display().to_string()),
            ("ProgramFiles", programs.display().to_string()),
            ("APPDATA", roaming.display().to_string()),
        ]);
        let policy = PathPolicy::from_env(|k| env.get(k).cloned(), user_home.to_str(), Some(&home));
        let s = |p: &Path| p.display().to_string();

        assert_eq!(project, policy.check(&s(&project), true).unwrap());
        assert_eq!(
            project.join("a.txt"),
            policy.check(&s(&project.join("a.txt")), false).unwrap()
        );
        assert!(policy.check("relative/path", true).is_err());
        assert!(policy.check(&s(&project.join("missing")), true).is_err());
        assert!(
            policy.check(&s(&project.join("a.txt")), true).is_err(),
            "a file where a folder is expected"
        );
        assert!(
            policy.check(&s(&project), false).is_err(),
            "a folder where a file is expected"
        );
        assert!(policy.is_denied(&windows.join("System32")));
        assert!(policy.is_denied(&programs.join("App")));
        assert!(policy.is_denied(&user_home.join(".ssh").join("id_rsa")));
        assert!(policy.is_denied(&roaming.join("Mozilla").join("profiles.ini")));
        assert!(policy.is_denied(&home.join("gateway.json")));
        assert!(policy.is_denied(&home.join("data").join("nook.mv.db")));
        // Nook's own record of other people's repositories: the index holds excerpts of every
        // repository this machine has indexed, and the evidence store quotes the ones it describes.
        assert!(
            policy.is_denied(&home.join("index").join("abc123").join("chunks.jsonl")),
            "the search index"
        );
        assert!(
            policy.is_denied(&home.join("evidence").join("abc123").join("claims.json")),
            "the evidence store"
        );
        assert!(
            !policy.is_denied(&home.join("images").join("x.png")),
            "generated images are readable"
        );
        assert!(
            !policy.is_denied(&home.join("logs").join("nook.log")),
            "Nook's own log is written to be read"
        );
        assert!(
            policy.is_denied(&user_home.join(".claude").join(".credentials.json")),
            "Claude Code OAuth tokens"
        );
        assert!(
            policy.is_denied(&user_home.join(".claude.json")),
            "Claude Code config with MCP bearer headers"
        );
        assert!(
            policy.is_denied(&user_home.join(".cursor").join("mcp.json")),
            "Cursor MCP config"
        );
        assert!(
            policy.is_denied(&roaming.join("Claude").join("claude_desktop_config.json")),
            "Claude Desktop config"
        );

        assert!(
            !policy.is_denied(&user_home.join(".claude-plugins-of-mine").join("x")),
            "a sibling that merely starts with the name is not denied"
        );
        let message = policy
            .check(&s(&user_home.join(".ssh").join("id_rsa")), false)
            .unwrap_err()
            .to_string();
        assert!(message.starts_with("Refused"), "{message}");
        // A link inside an allowed folder is not a way into a denied one (QA finding, 2026-09-22):
        // the path is judged by what it really points at, not by how it is spelled.
        let link = project.join("notes");
        if junction(&link, &user_home.join(".claude")) {
            assert!(PathPolicy::is_link(&link));
            assert!(
                policy.is_denied(&link),
                "a junction into ~/.claude is denied"
            );
            assert!(
                !policy.is_denied_as_written(&link),
                "as written it is an ordinary folder name"
            );
            assert!(
                policy.is_denied(&link.join(".credentials.json")),
                "and so is a file through it"
            );
            assert!(policy
                .check(&s(&link.join(".credentials.json")), false)
                .is_err());
            assert!(
                !policy.is_denied(&project.join("a.txt")),
                "an ordinary file beside it is still readable"
            );
            unlink(&link);
        }
        // A path that does not exist yet is judged where it would land: a write through a linked folder.
        let out = project.join("out");
        if junction(&out, &user_home.join(".ssh")) {
            assert!(
                policy.is_denied(&out.join("stolen.txt")),
                "a file that would be written through the link"
            );
            unlink(&out);
        }
        assert!(!PathPolicy::is_link(&project));
        // A home directory is not a repository: reading one whole reads everything in it, and the
        // denied list can only name what somebody thought of. An index of this desk's home on
        // 2026-09-22 held 22,444 files and a search of it returned an Android debug key from
        // ~/.android, a folder nobody had listed (found by the live check of the Codex fixes).
        assert_eq!(
            Some("your home folder".to_string()),
            policy.too_broad_to_read(&user_home),
            "a home folder"
        );
        assert_eq!(
            Some("the Downloads folder".to_string()),
            policy.too_broad_to_read(&user_home.join("Downloads")),
            "a folder everything lands in"
        );
        let root: PathBuf = user_home.ancestors().last().map(Path::to_path_buf).unwrap();
        assert_eq!(
            Some("a whole drive".to_string()),
            policy.too_broad_to_read(&root),
            "a whole drive"
        );
        assert_eq!(
            None,
            policy.too_broad_to_read(&project),
            "a project inside it is what Nook reads"
        );
        assert!(
            policy.is_denied(&user_home.join(".android").join("adbkey")),
            "the Android debug key"
        );
        assert!(
            policy.is_denied(&user_home.join(".m2").join("settings.xml")),
            "Maven credentials"
        );
        assert!(
            policy.is_denied(&user_home.join(".pgpass")),
            "a Postgres password file"
        );

        // Case and separators do not matter on Windows.
        #[cfg(windows)]
        {
            assert!(policy.is_denied(&PathBuf::from(s(&windows).to_uppercase()).join("system32")));
            assert!(
                policy.is_denied(&PathBuf::from(s(&windows).replace('\\', "/")).join("System32"))
            );
            assert!(
                policy.is_denied(&project.join("..").join(".ssh").join("id_rsa")),
                "a way out spelled with .."
            );
            assert_eq!(
                project,
                policy.check(&s(&project).replace('\\', "/"), true).unwrap()
            );
        }
    }

    #[test]
    fn real_resolves_as_far_as_the_path_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let real_tmp = PathPolicy::real(tmp.path());
        assert!(
            !real_tmp.to_string_lossy().starts_with(r"\\?\"),
            "{}",
            real_tmp.display()
        );
        assert_eq!(
            real_tmp.join("not").join("yet.txt"),
            PathPolicy::real(&tmp.path().join("not").join("yet.txt"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn odd_characters_are_not_a_path() {
        let policy = PathPolicy::from_env(|_| None, None, None);
        assert_eq!(
            "Not a valid path: C:\\a|b",
            policy.check("C:\\a|b", true).unwrap_err().to_string()
        );
        assert_eq!(
            "A path is required.",
            policy.check("  ", true).unwrap_err().to_string()
        );
        assert!(policy
            .check("\\no\\drive", true)
            .unwrap_err()
            .to_string()
            .starts_with("The path must be absolute"));
    }
}

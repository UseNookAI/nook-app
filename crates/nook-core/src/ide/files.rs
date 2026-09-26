//! The Code page's files: listing a folder for the explorer, reading a file into the editor and
//! writing it back the way it was read, and the explorer's new file / new folder / rename / delete.
//! Ports `IdeSupport.kt` and the file work of `IdeWorkspace.kt`.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Files the editor opens whole; bigger ones are for another tool.
pub const MAX_EDITOR_BYTES: u64 = 4 << 20;

/// How much of a file is looked at to tell text from binary.
const BINARY_SNIFF_BYTES: usize = 8000;

/// Charset names as Java's `Charset.name()` gives them; the status bar shows them as they are.
pub const UTF_8: &str = "UTF-8";
pub const ISO_8859_1: &str = "ISO-8859-1";

/// Line-ending names for the status bar and for saving.
pub const CRLF: &str = "CRLF";
pub const LF: &str = "LF";

/// How long `git rev-parse` may take before the branch is left out.
const BRANCH_TIMEOUT: Duration = Duration::from_secs(5);

/// One entry of a folder, for the explorer (`FileNode`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileNode {
    pub path: String,
    pub name: String,
    pub is_directory: bool,
}

impl FileNode {
    fn of(path: &Path) -> FileNode {
        FileNode {
            path: path.to_string_lossy().into_owned(),
            name: file_name(path),
            is_directory: path.is_dir(),
        }
    }
}

/// A file as the editor holds it (`IdeWorkspace.Loaded`): the text with `\n` line breaks, the line
/// ending and charset it was read with (a save writes them back), and its modification time in
/// milliseconds since the epoch, which the page compares to notice changes made elsewhere.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedFile {
    pub text: String,
    pub line_ending: String,
    pub charset: String,
    pub time: i64,
}

/// A folder's entries for the explorer: folders first, then files, each in name order without
/// regard to case. Git's own folder is left out; everything else shows, dotfiles included. A folder
/// that cannot be read lists as empty.
pub fn list_entries(dir: &Path) -> Vec<FileNode> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<FileNode> = read
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() != ".git")
        .map(|e| FileNode::of(&e.path()))
        .collect();
    out.sort_by(|a, b| {
        b.is_directory
            .cmp(&a.is_directory)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// A NUL byte among the first bytes, the way git tells a binary file from text.
pub fn looks_binary(head: &[u8]) -> bool {
    head.contains(&0)
}

/// "CRLF" when the text's first line break is Windows', "LF" otherwise; text without a line break
/// gets this system's, so a new file saves the way its neighbours do.
pub fn line_ending_of(text: &str) -> &'static str {
    match text.find('\n') {
        None => {
            if cfg!(windows) {
                CRLF
            } else {
                LF
            }
        }
        Some(nl) if nl > 0 && text.as_bytes()[nl - 1] == b'\r' => CRLF,
        Some(_) => LF,
    }
}

/// The file as text with `\n` line breaks: UTF-8, or Latin-1 when it is not valid UTF-8; never a
/// binary file, and never one over [`MAX_EDITOR_BYTES`].
pub fn read_text(file: &Path) -> Result<LoadedFile> {
    let name = file_name(file);
    // The time is taken before the read: a change landing in between shows as a newer time on the
    // next check and is read again, instead of slipping by.
    let meta = std::fs::metadata(file).with_context(|| format!("Could not open {name}"))?;
    let size = meta.len();
    if size > MAX_EDITOR_BYTES {
        bail!("{name} is {} MB, too big for the editor.", size >> 20);
    }
    let bytes = std::fs::read(file).with_context(|| format!("Could not open {name}"))?;
    if looks_binary(&bytes[..bytes.len().min(BINARY_SNIFF_BYTES)]) {
        bail!("{name} is not a text file.");
    }
    let (raw, charset) = match String::from_utf8(bytes) {
        Ok(s) => (s, UTF_8),
        Err(e) => (
            e.into_bytes()
                .iter()
                .map(|&b| b as char)
                .collect::<String>(),
            ISO_8859_1,
        ),
    };
    let ending = line_ending_of(&raw);
    Ok(LoadedFile {
        text: raw.replace("\r\n", "\n").replace('\r', "\n"),
        line_ending: ending.to_string(),
        charset: charset.to_string(),
        time: millis(&meta),
    })
}

/// Writes the editor's text back the way the file was read: its line ending and charset (a
/// character Latin-1 cannot hold becomes `?`, as Java's encoder does). Atomic; returns the file's
/// new modification time.
pub fn write_text(file: &Path, text: &str, line_ending: &str, charset: &str) -> Result<i64> {
    let text = if line_ending == CRLF {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    };
    let bytes = if charset.eq_ignore_ascii_case(ISO_8859_1) {
        text.chars()
            .map(|c| if (c as u32) <= 0xFF { c as u8 } else { b'?' })
            .collect()
    } else {
        text.into_bytes()
    };
    write_atomic(file, &bytes)?;
    let meta =
        std::fs::metadata(file).with_context(|| format!("Could not read {}", file_name(file)))?;
    Ok(millis(&meta))
}

/// The modification time of each path that is a regular file, None for the rest (gone, a folder).
pub fn file_times(paths: &[PathBuf]) -> Vec<Option<i64>> {
    paths
        .iter()
        .map(|p| {
            std::fs::metadata(p)
                .ok()
                .filter(|m| m.is_file())
                .map(|m| millis(&m))
        })
        .collect()
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Writes through a temporary file beside the target and a rename, so a crash mid-write leaves the
/// previous version. Each write has its own temporary name: two writes at once raced on a shared
/// one in the original (the ide.json fix of 2026-09-25). When the rename is refused (another
/// program holds the file open without letting it be replaced), the file is written in place.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = file_name(path);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("Could not create {}", parent.display()))?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{name}.{}-{seq}.tmp", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("Could not write {name}"));
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(path, bytes).with_context(|| format!("Could not write {name}"))?;
    }
    Ok(())
}

/// A name for a new file or folder, trimmed; refused when it is empty, a dot name, or holds a path
/// separator or a drive colon.
pub fn checked_name(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() || n == "." || n == ".." || n.chars().any(|c| c == '/' || c == '\\' || c == ':')
    {
        bail!("\"{name}\" is not a file name.");
    }
    Ok(n.to_string())
}

/// Creates an empty file `name` in `dir` and returns its path.
pub fn create_file(dir: &Path, name: &str) -> Result<PathBuf> {
    let file = dir.join(checked_name(name)?);
    if exists(&file) {
        bail!("{name} is already there.");
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)?;
    Ok(file)
}

/// Creates the folder `name` in `dir` and returns its path.
pub fn create_folder(dir: &Path, name: &str) -> Result<PathBuf> {
    let sub = dir.join(checked_name(name)?);
    if exists(&sub) {
        bail!("{name} is already there.");
    }
    std::fs::create_dir(&sub)?;
    Ok(sub)
}

/// Renames a file or folder in place and returns what it became. A different entry of that name
/// is never replaced; changing only the case of a name is allowed.
pub fn rename(path: &Path, name: &str) -> Result<FileNode> {
    let checked = checked_name(name)?;
    let target = match path.parent() {
        Some(parent) => parent.join(&checked),
        None => PathBuf::from(&checked),
    };
    if target == path {
        return Ok(FileNode::of(path));
    }
    if exists(&target) && !same_entry(path, &target) {
        bail!("{name} is already there.");
    }
    std::fs::rename(path, &target)?;
    Ok(FileNode::of(&target))
}

/// Removes a file, or a folder with everything in it, for good (there is no bin to restore from).
/// A link is removed itself, never what it points to; a path that is already gone is fine.
pub fn delete(path: &Path) -> Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if meta.is_symlink() {
        // A link to a folder (a junction, a directory symlink) is removed as a folder on Windows.
        std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path))?;
    } else if meta.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// The folder as the page keys it: absolute, with `.` and `..` taken out.
pub fn normalize_folder(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for c in absolute.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(
                    out.components().next_back(),
                    Some(Component::RootDir | Component::Prefix(_)) | None
                ) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The branch checked out in `dir`, or None when it is not a git repository (or git is not here,
/// or it takes longer than five seconds).
pub async fn read_branch(dir: &Path) -> Option<String> {
    let mut cmd = crate::process::command("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let out = tokio::time::timeout(BRANCH_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !branch.is_empty()).then_some(branch)
}

/// The last part of a path, or the whole path when it has none (a drive root).
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Whether anything is at `path`, a dangling link included.
fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Whether two paths name the same entry: on a case-insensitive disk, `a.txt` and `A.txt`.
fn same_entry(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

fn millis(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(dir: &Path) -> Vec<String> {
        list_entries(dir)
            .into_iter()
            .map(|n| n.name + if n.is_directory { "/" } else { "" })
            .collect()
    }

    #[test]
    fn a_folder_lists_folders_first_by_name_and_hides_git() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(d.join("Docs")).unwrap();
        std::fs::write(d.join("b.txt"), "").unwrap();
        std::fs::write(d.join("A.txt"), "").unwrap();
        std::fs::write(d.join(".gitignore"), "").unwrap();
        assert_eq!(
            names(d),
            vec!["Docs/", "src/", ".gitignore", "A.txt", "b.txt"]
        );
        let first = &list_entries(d)[0];
        assert_eq!(PathBuf::from(&first.path), d.join("Docs"));
        // A folder that cannot be read lists as empty.
        assert!(list_entries(&d.join("missing")).is_empty());
    }

    #[test]
    fn text_is_told_from_binary_and_its_line_ending_kept() {
        assert!(!looks_binary(b"plain text\n"));
        assert!(looks_binary(&[0x50, 0x4B, 0x03, 0x04, 0]));
        assert_eq!(line_ending_of("a\r\nb\r\n"), CRLF);
        assert_eq!(line_ending_of("a\nb\r\n"), LF);
        assert_eq!(line_ending_of("\nx"), LF);
        assert_eq!(
            line_ending_of("no break"),
            if cfg!(windows) { CRLF } else { LF }
        );
    }

    #[test]
    fn a_file_reads_as_text_with_plain_line_breaks() {
        let dir = tempfile::tempdir().unwrap();
        let crlf = dir.path().join("win.txt");
        std::fs::write(&crlf, "one\r\ntwo\r\n").unwrap();
        let loaded = read_text(&crlf).unwrap();
        assert_eq!(loaded.text, "one\ntwo\n");
        assert_eq!(loaded.line_ending, CRLF);
        assert_eq!(loaded.charset, UTF_8);
        assert_eq!(
            Some(loaded.time),
            file_times(std::slice::from_ref(&crlf))[0]
        );

        let latin = dir.path().join("latin.txt");
        std::fs::write(&latin, [b'c', b'a', b'f', 0xE9, b'\n']).unwrap();
        let loaded = read_text(&latin).unwrap();
        assert_eq!(loaded.text, "café\n");
        assert_eq!(loaded.charset, ISO_8859_1);
        assert_eq!(loaded.line_ending, LF);

        let old_mac = dir.path().join("mac.txt");
        std::fs::write(&old_mac, "a\rb").unwrap();
        assert_eq!(read_text(&old_mac).unwrap().text, "a\nb");
    }

    #[test]
    fn binary_and_big_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("logo.png");
        std::fs::write(&bin, [0x89, b'P', b'N', b'G', 0, 1, 2]).unwrap();
        assert_eq!(
            read_text(&bin).unwrap_err().to_string(),
            "logo.png is not a text file."
        );

        let big = dir.path().join("big.log");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_EDITOR_BYTES + 1).unwrap();
        drop(f);
        assert_eq!(
            read_text(&big).unwrap_err().to_string(),
            "big.log is 4 MB, too big for the editor."
        );

        let missing = read_text(&dir.path().join("gone.txt")).unwrap_err();
        assert!(format!("{missing:#}").starts_with("Could not open gone.txt: "));
    }

    #[test]
    fn a_save_writes_back_the_line_ending_and_charset_it_was_read_with() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x\r\n").unwrap();
        let time = write_text(&file, "one\ntwo\n", CRLF, UTF_8).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"one\r\ntwo\r\n");
        assert_eq!(Some(time), file_times(std::slice::from_ref(&file))[0]);

        write_text(&file, "café €\n", LF, ISO_8859_1).unwrap();
        assert_eq!(
            std::fs::read(&file).unwrap(),
            [b'c', b'a', b'f', 0xE9, b' ', b'?', b'\n']
        );

        // Nothing is left beside it.
        assert_eq!(names(dir.path()), vec!["a.txt"]);
    }

    #[test]
    fn saves_at_once_each_use_their_own_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("race.txt");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let file = file.clone();
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        write_atomic(&file, format!("writer {i}").as_bytes()).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert!(std::fs::read_to_string(&file)
            .unwrap()
            .starts_with("writer "));
        assert_eq!(names(dir.path()), vec!["race.txt"]);
    }

    #[test]
    fn file_times_skip_folders_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "x").unwrap();
        let times = file_times(&[file, dir.path().to_path_buf(), dir.path().join("nope")]);
        assert!(times[0].is_some());
        assert_eq!(times[1..], [None, None]);
    }

    #[test]
    fn names_are_checked() {
        assert_eq!(checked_name("  main.rs ").unwrap(), "main.rs");
        for bad in ["", "  ", ".", "..", "a/b", "a\\b", "c:x"] {
            assert_eq!(
                checked_name(bad).unwrap_err().to_string(),
                format!("\"{bad}\" is not a file name.")
            );
        }
    }

    #[test]
    fn the_explorer_creates_renames_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let file = create_file(d, "notes.md").unwrap();
        assert_eq!(file, d.join("notes.md"));
        assert!(file.is_file());
        assert_eq!(
            create_file(d, "notes.md").unwrap_err().to_string(),
            "notes.md is already there."
        );

        let sub = create_folder(d, "src").unwrap();
        assert!(sub.is_dir());
        assert_eq!(
            create_folder(d, "src").unwrap_err().to_string(),
            "src is already there."
        );
        assert_eq!(
            create_folder(d, "notes.md").unwrap_err().to_string(),
            "notes.md is already there."
        );

        // A rename never replaces another entry.
        std::fs::write(d.join("other.md"), "keep").unwrap();
        assert_eq!(
            rename(&file, "other.md").unwrap_err().to_string(),
            "other.md is already there."
        );
        assert_eq!(std::fs::read_to_string(d.join("other.md")).unwrap(), "keep");

        let renamed = rename(&file, "readme.md").unwrap();
        assert_eq!(
            renamed,
            FileNode {
                path: d.join("readme.md").display().to_string(),
                name: "readme.md".into(),
                is_directory: false
            }
        );
        assert!(!file.exists());

        // Only the case changes.
        let upper = rename(&d.join("readme.md"), "README.md").unwrap();
        assert_eq!(upper.name, "README.md");
        assert!(names(d).contains(&"README.md".to_string()));

        let moved = rename(&sub, "lib").unwrap();
        assert!(moved.is_directory);

        // A folder goes with everything in it; a missing path is fine.
        std::fs::create_dir_all(d.join("lib").join("deep")).unwrap();
        std::fs::write(d.join("lib").join("deep").join("x.rs"), "fn x() {}").unwrap();
        delete(&d.join("lib")).unwrap();
        assert!(!d.join("lib").exists());
        delete(&d.join("README.md")).unwrap();
        delete(&d.join("README.md")).unwrap();
        assert_eq!(names(d), vec!["other.md"]);
    }

    #[test]
    fn a_folder_is_keyed_absolute_and_normalized() {
        let dir = tempfile::tempdir().unwrap();
        let messy = dir.path().join("a").join("..").join(".").join("b");
        assert_eq!(
            normalize_folder(&messy),
            normalize_folder(&dir.path().join("b"))
        );
        assert!(normalize_folder(Path::new("relative")).is_absolute());
    }

    #[tokio::test]
    async fn a_folder_outside_git_has_no_branch() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_branch(dir.path()).await, None);
    }

    async fn git(dir: &Path, args: &[&str]) -> bool {
        let out = crate::process::command("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .await;
        matches!(out, Ok(o) if o.status.success())
    }

    #[tokio::test]
    async fn a_repository_tells_its_branch() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        // Machines without git skip this.
        if !git(d, &["init", "-q", "-b", "trunk"]).await {
            return;
        }
        let commit = [
            "-c",
            "user.name=Nook",
            "-c",
            "user.email=nook@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "start",
        ];
        if !git(d, &commit).await {
            return;
        }
        assert_eq!(read_branch(d).await.as_deref(), Some("trunk"));
    }
}

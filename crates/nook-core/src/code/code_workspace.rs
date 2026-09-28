//! The git side of a Code session. The worker never edits the person's files: it edits a scratch
//! copy that lives as long as the session, and the person's files change only when they press
//! Apply.
//!
//! Two kinds of copy, one set of operations. In a git repository with a commit, the copy is
//! `git worktree add --detach` of HEAD. Any other folder (no git, or a repository without a
//! commit) gets a private copy: its files as they are on disk, copied into Nook's temp folder,
//! with a git record of their own kept beside the copy (`<id>.git`) and never in the person's
//! folder. The diff, undo and discard work the same on both; Apply is `git apply`, which also
//! works in a folder that is not a repository.
//!
//! The diff is taken against a baseline: the start commit, or after an apply the tree the scratch
//! copy held then, so the next diff holds only what is new.
//!
//! Ports `code/CodeWorkspace.java`. git runs through [`crate::process::command`] (no console
//! window); what it prints on stderr is part of a failure's message but, unlike the original,
//! never of a successful command's output (a warning on stderr could otherwise end up in a diff
//! or a tree id).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::worker::path_policy::absolute;

/// A scratch copy just made; `from` says what it started from, in words for the session's notes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created {
    pub dir: PathBuf,
    pub head: String,
    pub from: String,
}

/// How Code mode would work on a folder: on a git REPOSITORY (a worktree), on a plain FOLDER (a
/// private copy), or not at all because it is MISSING or TOO_BIG to copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Readiness {
    Repository,
    Folder,
    Missing,
    TooBig,
}

/// `root` is what the session works on; `reason` says why it cannot, for MISSING and TOO_BIG.
/// (`CodeWorkspace.State` in the original.)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryState {
    pub readiness: Readiness,
    pub root: Option<PathBuf>,
    pub reason: Option<String>,
}

impl RepositoryState {
    pub fn usable(&self) -> bool {
        matches!(self.readiness, Readiness::Repository | Readiness::Folder)
    }
}

/// A private copy is made on every new session, so a folder must be a project's size, not a
/// drive's.
pub const COPY_MAX_FILES: u64 = 20_000;
pub const COPY_MAX_BYTES: u64 = 1 << 30;

/// What running a check leaves behind and is never part of a change: interpreter and tool
/// caches. A repository without a .gitignore for them (the first live Code session, on
/// 2026-09-23, put two __pycache__/*.pyc files in its diff) still gets a clean change.
pub const GENERATED: &[&str] = &[
    "**/__pycache__/**",
    "**/*.pyc",
    "**/.pytest_cache/**",
    "**/.mypy_cache/**",
    "**/.ruff_cache/**",
    "**/node_modules/**",
    "**/.gradle/**",
    "**/*.class",
    "**/.DS_Store",
];

/// Folders a private copy leaves out (see [`skipped`]).
const SKIPPED_NAMES: &[&str] = &[
    ".git",
    "node_modules",
    "__pycache__",
    ".gradle",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".venv",
    "venv",
];

/// The nearest folder at or above `inside` that holds `.git`, or None.
fn find_root(inside: &Path) -> Option<PathBuf> {
    let mut p = absolute(inside);
    if p.is_file() {
        p = p.parent()?.to_path_buf();
    }
    let mut here = Some(p.as_path());
    while let Some(h) = here {
        if h.join(".git").exists() {
            return Some(h.to_path_buf());
        }
        here = h.parent();
    }
    None
}

/// How a session would work on `folder`.
pub async fn state(folder: &Path) -> RepositoryState {
    if !folder.is_dir() {
        return RepositoryState {
            readiness: Readiness::Missing,
            root: None,
            reason: Some(format!("{} is not there any more.", folder.display())),
        };
    }
    let root = find_root(folder);
    if let Some(r) = &root {
        if has_commit(r).await {
            return RepositoryState {
                readiness: Readiness::Repository,
                root: Some(r.clone()),
                reason: None,
            };
        }
    }
    // A repository without a commit is copied from its root, like any folder.
    let base = root.unwrap_or_else(|| absolute(folder));
    let checked = base.clone();
    let size = tokio::task::spawn_blocking(move || {
        check_copy_size(&checked, COPY_MAX_FILES, COPY_MAX_BYTES)
    })
    .await
    .unwrap_or_else(|e| Err(anyhow!("{e}")));
    match size {
        Err(e) => RepositoryState {
            readiness: Readiness::TooBig,
            root: Some(base),
            reason: Some(e.to_string()),
        },
        Ok(()) => RepositoryState {
            readiness: Readiness::Folder,
            root: Some(base),
            reason: None,
        },
    }
}

async fn has_commit(root: &Path) -> bool {
    git(root, 60, &["rev-parse", "--verify", "-q", "HEAD"])
        .await
        .is_ok()
}

/// Refuses a folder too big to copy (a home folder, a drive, a model store). Blocking: it walks
/// the folder.
pub fn check_copy_size(root: &Path, max_files: u64, max_bytes: u64) -> Result<()> {
    let p = absolute(root);
    if p.parent().is_none() {
        bail!(
            "{} is a whole drive. Choose the project's own folder.",
            p.display()
        );
    }
    if let Some(home) = user_home() {
        if same_path(&p, &absolute(&home)) {
            bail!(
                "{} is your home folder. Choose the project's own folder.",
                p.display()
            );
        }
    }
    let mut files = 0u64;
    let mut bytes = 0u64;
    let walk = walkdir::WalkDir::new(&p)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !skipped_folder(e.file_name()));
    for e in walk {
        let e = e.map_err(|e| anyhow!("Could not read {}: {e}", p.display()))?;
        let rel = e.path().strip_prefix(&p).unwrap_or(e.path());
        if skipped(rel) {
            continue;
        }
        // a link is counted as what it points at, as the original's isRegularFile read it
        let meta = match std::fs::metadata(e.path()) {
            Ok(m) => m,
            Err(_) => continue, // unreadable: the copy will say so
        };
        if !meta.is_file() {
            continue;
        }
        files += 1;
        bytes += meta.len();
        if files > max_files || bytes > max_bytes {
            break;
        }
    }
    if files > max_files || bytes > max_bytes {
        bail!(
            "{} holds more than {max_files} files or {} MB, too much to copy for each session. Choose the project's own folder.",
            p.file_name().unwrap_or_default().to_string_lossy(),
            max_bytes >> 20
        );
    }
    Ok(())
}

fn skipped_folder(name: &std::ffi::OsStr) -> bool {
    SKIPPED_NAMES.contains(&name.to_string_lossy().as_ref())
}

/// What a private copy leaves out: git's own folder, and the caches and environments of
/// [`GENERATED`] that can be rebuilt and would only make the copy slow.
fn skipped(rel: &Path) -> bool {
    if rel.components().any(|c| skipped_folder(c.as_os_str())) {
        return true;
    }
    let name = rel
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.ends_with(".pyc") || name.ends_with(".class") || name == ".DS_Store"
}

/// Makes the scratch copy of `root` at `dir`: a worktree for a repository with a commit, else a
/// private copy.
pub async fn create(root: &Path, dir: &Path) -> Result<Created> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Could not create {}", parent.display()))?;
    }
    if root.join(".git").exists() && has_commit(root).await {
        let head = git(root, 60, &["rev-parse", "--short", "HEAD"])
            .await?
            .trim()
            .to_string();
        // --force: a copy whose folder was deleted (a cleaned temp folder) is still registered,
        // and git refuses the same path again without it. It overrides only that, for this path.
        let dir_arg = dir.to_string_lossy().into_owned();
        let out = run_git(
            root,
            180,
            &[
                "worktree",
                "add",
                "--force",
                "--detach",
                dir_arg.as_str(),
                "HEAD",
            ],
        )
        .await?
        .success()?;
        if !dir.is_dir() {
            bail!("git worktree add failed: {}", out.trim());
        }
        return Ok(Created {
            dir: dir.to_path_buf(),
            from: format!("commit {head}"),
            head,
        });
    }
    copy(root, dir).await
}

/// The private git record of a copy, kept beside it so the worker never sees it.
pub fn private_git_dir(dir: &Path) -> PathBuf {
    let name = format!(
        "{}.git",
        dir.file_name().unwrap_or_default().to_string_lossy()
    );
    match dir.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

/// True when `dir` is a private copy rather than a worktree of the person's repository.
pub fn is_private_copy(dir: &Path) -> bool {
    private_git_dir(dir).is_dir()
}

async fn copy(folder: &Path, dir: &Path) -> Result<Created> {
    let from = absolute(folder);
    let git_dir = private_git_dir(dir);
    {
        let (from, dir, git_dir) = (from.clone(), dir.to_path_buf(), git_dir.clone());
        tokio::task::spawn_blocking(move || -> Result<()> {
            check_copy_size(&from, COPY_MAX_FILES, COPY_MAX_BYTES)?;
            delete_tree(&dir)?;
            delete_tree(&git_dir)?;
            std::fs::create_dir_all(&dir)?;
            copy_files(&from, &dir).map_err(|e| anyhow!("Could not copy {}: {e}", from.display()))
        })
        .await
        .map_err(|e| anyhow!("{e}"))??;
    }
    let parent = dir.parent().unwrap_or(dir);
    let separate = format!("--separate-git-dir={}", git_dir.display());
    let dir_arg = dir.to_string_lossy().into_owned();
    git(
        parent,
        60,
        &["init", "-q", separate.as_str(), dir_arg.as_str()],
    )
    .await?;
    // Bytes as they are: the diff has to fit the person's files exactly when Apply writes it back.
    git(dir, 30, &["config", "core.autocrlf", "false"]).await?;
    git(dir, 30, &["config", "core.safecrlf", "false"]).await?;
    stage(dir).await?;
    let message = format!("Nook's copy of {}", from.display());
    git(
        dir,
        120,
        &[
            "-c",
            "user.name=Nook",
            "-c",
            "user.email=nook@localhost",
            "commit",
            "-q",
            "--allow-empty",
            "--no-verify",
            "-m",
            message.as_str(),
        ],
    )
    .await?;
    let head = git(dir, 60, &["rev-parse", "--short", "HEAD"])
        .await?
        .trim()
        .to_string();
    Ok(Created {
        dir: dir.to_path_buf(),
        head,
        from: "your files as they are now".to_string(),
    })
}

/// The files of `from` into `dir`, without what [`skipped`] leaves out. Links are left out, file
/// and folder links alike (symbolic links, junctions): a link can point anywhere on the computer,
/// and what it points at would come into the scratch copy as an ordinary file, past every check
/// that keeps the worker inside it. Each entry is asked what it is itself, never what it points
/// at, and only plain files and folders are copied.
fn copy_files(from: &Path, dir: &Path) -> Result<()> {
    let walk = walkdir::WalkDir::new(from)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !(skipped_folder(e.file_name()) || is_link(e)));
    for e in walk {
        let e = e?;
        let rel = e.path().strip_prefix(from).unwrap_or(e.path());
        if rel.as_os_str().is_empty() || skipped(rel) || !inside(rel) || is_link(&e) {
            continue;
        }
        let to = dir.join(rel);
        let f = e.path();
        let kind = e.file_type();
        if kind.is_dir() {
            std::fs::create_dir_all(&to)?;
        } else if kind.is_file() {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(f, &to).with_context(|| f.display().to_string())?;
        }
    }
    Ok(())
}

/// Whether a walked entry is a link Windows follows by name: a symbolic link to a file or a
/// folder, or a junction (`FileType::is_symlink` covers all three on Windows), asked of the entry
/// itself.
fn is_link(e: &walkdir::DirEntry) -> bool {
    e.path_is_symlink()
        || e.file_type().is_symlink()
        || std::fs::symlink_metadata(e.path()).is_ok_and(|m| m.file_type().is_symlink())
}

/// Whether a relative path stays below its root: no `..`, no root or drive of its own.
fn inside(rel: &Path) -> bool {
    rel.components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Deletes a folder tree (or a file); git marks its objects read-only, which Windows will not
/// delete as they are. Links inside are removed, never followed.
pub fn delete_tree(root: &Path) -> std::io::Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(root) else {
        return Ok(());
    };
    if !meta.is_dir() {
        return std::fs::remove_file(root);
    }
    if std::fs::remove_dir_all(root).is_ok() {
        return Ok(());
    }
    for e in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if e.file_type().is_file() {
            if let Ok(m) = e.metadata() {
                let mut perms = m.permissions();
                if perms.readonly() {
                    #[allow(clippy::permissions_set_readonly_false)]
                    perms.set_readonly(false);
                    let _ = std::fs::set_permissions(e.path(), perms);
                }
            }
        }
    }
    std::fs::remove_dir_all(root)
}

/// Stages the scratch copy for a diff: everything except [`GENERATED`], which is taken out if it
/// got in earlier.
pub async fn stage(dir: &Path) -> Result<()> {
    let mut add: Vec<String> = ["add", "-A", "--", "."].map(String::from).to_vec();
    let mut reset: Vec<String> = ["reset", "-q", "--"].map(String::from).to_vec();
    for g in GENERATED {
        add.push(format!(":(exclude,glob){g}"));
        reset.push(format!(":(glob){g}"));
    }
    git(dir, 120, &add).await?;
    git(dir, 120, &reset).await?;
    Ok(())
}

/// Everything the scratch copy changed since `baseline` (a tree id), or since its start commit
/// when None. With `binary`, binary files come as git binary patches, so [`apply`] can carry them.
pub async fn diff(dir: &Path, baseline: Option<&str>, binary: bool) -> Result<String> {
    stage(dir).await?;
    let mut args = vec!["diff", "--cached", "--no-color"];
    if binary {
        args.push("--binary");
    }
    if let Some(b) = baseline {
        args.push(b);
    }
    git(dir, 120, &args).await
}

pub async fn stat(dir: &Path, baseline: Option<&str>) -> Result<String> {
    stage(dir).await?;
    let mut args = vec!["diff", "--cached", "--stat", "--no-color"];
    if let Some(b) = baseline {
        args.push(b);
    }
    Ok(git(dir, 120, &args).await?.trim().to_string())
}

/// The tree the scratch copy holds now, as a git tree id.
pub async fn tree(dir: &Path) -> Result<String> {
    stage(dir).await?;
    Ok(git(dir, 60, &["write-tree"]).await?.trim().to_string())
}

/// True when the scratch copy's git record still holds tree `id`. A worktree shares the
/// repository's objects.
pub async fn has_tree(dir: &Path, id: &str) -> bool {
    if id.trim().is_empty() {
        return false;
    }
    let object = format!("{id}^{{tree}}");
    git(dir, 30, &["cat-file", "-e", object.as_str()])
        .await
        .is_ok()
}

/// Puts the scratch copy back to `baseline` (or its start commit), dropping new files too.
pub async fn reset(dir: &Path, baseline: Option<&str>) -> Result<()> {
    git(
        dir,
        120,
        &["read-tree", "-u", "--reset", baseline.unwrap_or("HEAD")],
    )
    .await?;
    git(dir, 120, &["clean", "-fdq"]).await?;
    Ok(())
}

/// Applies `patch` to the person's files. Checks first, so a patch that does not fit changes
/// nothing; the error carries git's reason. In a folder that is not a repository, `git apply`
/// works as `patch` does.
///
/// With `exact_bytes` (a patch from a private copy, which keeps bytes as they are), line endings
/// are not converted on the way in: outside a repository `git apply` would otherwise follow the
/// machine's core.autocrlf and turn a file's LF lines into CRLF.
pub async fn apply(
    target: &Path,
    patch: &str,
    scratch_file: &Path,
    exact_bytes: bool,
) -> Result<()> {
    if let Some(parent) = scratch_file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Could not create {}", parent.display()))?;
    }
    std::fs::write(scratch_file, patch.as_bytes())
        .with_context(|| format!("Could not write {}", scratch_file.display()))?;
    let file = scratch_file.to_string_lossy().into_owned();
    let result = async {
        let crlf: &[&str] = if exact_bytes {
            &["-c", "core.autocrlf=false"]
        } else {
            &[]
        };
        let check: Vec<&str> = crlf
            .iter()
            .copied()
            .chain(["apply", "--check", "--whitespace=nowarn", file.as_str()])
            .collect();
        git(target, 120, &check).await?;
        let apply: Vec<&str> = crlf
            .iter()
            .copied()
            .chain(["apply", "--whitespace=nowarn", file.as_str()])
            .collect();
        git(target, 120, &apply).await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let _ = std::fs::remove_file(scratch_file);
    result
}

/// Removes a scratch copy: a private copy's folder and record, or a worktree (then pruned).
pub async fn remove(repo: &Path, dir: &Path) {
    if is_private_copy(dir) {
        let (d, g) = (dir.to_path_buf(), private_git_dir(dir));
        // a file held open by a check stays; the temp folder is swept with Nook's other leftovers
        let _ = tokio::task::spawn_blocking(move || {
            let _ = delete_tree(&d);
            let _ = delete_tree(&g);
        })
        .await;
        return;
    }
    let dir_arg = dir.to_string_lossy().into_owned();
    // the folder may be gone already; prune below forgets it either way
    let _ = git(
        repo,
        120,
        &["worktree", "remove", "--force", dir_arg.as_str()],
    )
    .await;
    let _ = git(repo, 60, &["worktree", "prune"]).await;
}

/// The files a diff writes to, repository-relative.
pub fn files(diff: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in diff.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if !line.starts_with("+++ ") && !line.starts_with("--- ") {
            continue;
        }
        let mut p = line[4..].trim_matches(|c: char| c <= ' ');
        if p == "/dev/null" {
            continue;
        }
        if p.starts_with("a/") || p.starts_with("b/") {
            p = &p[2..];
        }
        if !p.trim().is_empty() && !out.iter().any(|o| o == p) {
            out.push(p.to_string());
        }
    }
    out
}

/// What git said: whether it succeeded, its stdout, and stdout with stderr after it.
struct GitOutput {
    ok: bool,
    stdout: String,
    combined: String,
    command: String,
}

impl GitOutput {
    fn success(self) -> Result<String> {
        if !self.ok {
            bail!("git {} failed: {}", self.command, self.combined.trim());
        }
        Ok(self.combined)
    }
}

async fn run_git<S: AsRef<str>>(cwd: &Path, timeout_seconds: u64, args: &[S]) -> Result<GitOutput> {
    let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    let mut cmd = crate::process::command("git");
    cmd.arg("-c")
        .arg("core.quotepath=off")
        .args(&args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().map_err(|e| {
        anyhow!(
            "Cannot run program \"git\" (in directory \"{}\"): {e}",
            cwd.display()
        )
    })?;
    let first = args.first().copied().unwrap_or("");
    // A git that does not finish is killed when its future is dropped (kill-on-drop).
    let out = match tokio::time::timeout(
        Duration::from_secs(timeout_seconds),
        child.wait_with_output(),
    )
    .await
    {
        Ok(out) => out?,
        Err(_) => bail!("git {first} did not finish in {timeout_seconds} s"),
    };
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    Ok(GitOutput {
        ok: out.status.success(),
        combined: format!("{stdout}{stderr}"),
        stdout,
        command: args.join(" "),
    })
}

/// Runs `git -c core.quotepath=off <args>` in `cwd`: its output, or an error with what it said.
pub async fn git<S: AsRef<str>>(cwd: &Path, timeout_seconds: u64, args: &[S]) -> Result<String> {
    let out = run_git(cwd, timeout_seconds, args).await?;
    if !out.ok {
        bail!("git {} failed: {}", out.command, out.combined.trim());
    }
    Ok(out.stdout)
}

fn user_home() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The same path; without case on Windows.
fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// The scratch copy of a Code session: it changes while the repository does not, its diff is
/// taken against a baseline that moves when the change is applied, and discarding goes back to
/// that baseline.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) async fn repo(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, 60, &["init", "-q"]).await.unwrap();
        git(dir, 60, &["config", "user.email", "t@example.com"])
            .await
            .unwrap();
        git(dir, 60, &["config", "user.name", "t"]).await.unwrap();
        git(dir, 60, &["config", "core.autocrlf", "false"])
            .await
            .unwrap();
        git(dir, 60, &["config", "commit.gpgsign", "false"])
            .await
            .unwrap();
        std::fs::write(dir.join("A.txt"), "one\ntwo\n").unwrap();
        git(dir, 60, &["add", "-A"]).await.unwrap();
        git(dir, 60, &["commit", "-q", "-m", "start"])
            .await
            .unwrap();
        dir.to_path_buf()
    }

    pub(crate) fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    #[tokio::test]
    async fn a_sessions_change_reaches_the_repository_only_when_applied() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo(&tmp.path().join("repo")).await;
        let c = create(&repo, &tmp.path().join("scratch").join("s1"))
            .await
            .unwrap();
        let dir = c.dir.clone();
        assert!(!c.head.trim().is_empty());

        std::fs::write(dir.join("A.txt"), "one\ntwo changed\n").unwrap();
        std::fs::write(dir.join("B.txt"), "new file\n").unwrap();
        let d = diff(&dir, None, false).await.unwrap();
        assert_eq!(vec!["A.txt", "B.txt"], files(&d));
        assert_eq!(
            "one\ntwo\n",
            read(&repo.join("A.txt")),
            "the person's files are untouched until Apply"
        );
        let st = stat(&dir, None).await.unwrap();
        assert!(st.contains("2 files changed"), "{st}");

        apply(&repo, &d, &tmp.path().join("apply.patch"), false)
            .await
            .unwrap();
        assert_eq!("one\ntwo changed\n", read(&repo.join("A.txt")));
        assert_eq!("new file\n", read(&repo.join("B.txt")));
        assert!(
            !tmp.path().join("apply.patch").exists(),
            "the patch file is not left behind"
        );

        // After an apply, the baseline moves: the next diff holds only what is new.
        let baseline = tree(&dir).await.unwrap();
        assert!(diff(&dir, Some(&baseline), false)
            .await
            .unwrap()
            .trim()
            .is_empty());
        std::fs::write(dir.join("B.txt"), "new file\nand more\n").unwrap();
        std::fs::write(dir.join("C.txt"), "stray\n").unwrap();
        let next = diff(&dir, Some(&baseline), false).await.unwrap();
        assert_eq!(vec!["B.txt", "C.txt"], files(&next));
        assert!(!next.contains("two changed"), "{next}");

        // Discard goes back to the baseline, new files included.
        reset(&dir, Some(&baseline)).await.unwrap();
        assert_eq!("new file\n", read(&dir.join("B.txt")));
        assert!(!dir.join("C.txt").exists());
        assert!(diff(&dir, Some(&baseline), false)
            .await
            .unwrap()
            .trim()
            .is_empty());

        remove(&repo, &dir).await;
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn a_patch_that_does_not_fit_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo(&tmp.path().join("repo")).await;
        let dir = create(&repo, &tmp.path().join("scratch").join("s2"))
            .await
            .unwrap()
            .dir;
        std::fs::write(dir.join("A.txt"), "one\ntwo from nook\n").unwrap();
        let d = diff(&dir, None, false).await.unwrap();
        std::fs::write(repo.join("A.txt"), "one\ntwo edited by the person\n").unwrap();
        let e = apply(&repo, &d, &tmp.path().join("p.patch"), false)
            .await
            .unwrap_err();
        assert!(e.to_string().starts_with("git apply --check"), "{e}");
        assert_eq!(
            "one\ntwo edited by the person\n",
            read(&repo.join("A.txt")),
            "the person's edit stands"
        );
        remove(&repo, &dir).await;
    }

    #[tokio::test]
    async fn what_a_check_leaves_behind_is_never_part_of_the_change() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo(&tmp.path().join("repo")).await;
        let dir = create(&repo, &tmp.path().join("scratch").join("s3"))
            .await
            .unwrap()
            .dir;
        std::fs::write(dir.join("A.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::create_dir_all(dir.join("__pycache__")).unwrap();
        std::fs::write(
            dir.join("__pycache__").join("a.cpython-311.pyc"),
            [1u8, 2, 3, 0],
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("pkg").join(".pytest_cache")).unwrap();
        std::fs::write(dir.join("pkg").join(".pytest_cache").join("v"), "x").unwrap();
        assert_eq!(
            vec!["A.txt"],
            files(&diff(&dir, None, false).await.unwrap())
        );
        assert!(!stat(&dir, None).await.unwrap().contains("pycache"));

        // Staged earlier (by an older build, or a .gitignore added later): taken out again.
        git(&dir, 60, &["add", "-A", "--", "."]).await.unwrap();
        assert_eq!(
            vec!["A.txt"],
            files(&diff(&dir, None, false).await.unwrap())
        );
        remove(&repo, &dir).await;
    }

    #[tokio::test]
    async fn a_new_binary_file_reaches_the_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo(&tmp.path().join("repo")).await;
        let dir = create(&repo, &tmp.path().join("scratch").join("s4"))
            .await
            .unwrap()
            .dir;
        let png = [0x89u8, b'P', b'N', b'G', 0, 0, 1, 2, 3];
        std::fs::write(dir.join("logo.png"), png).unwrap();
        let d = diff(&dir, None, true).await.unwrap();
        apply(&repo, &d, &tmp.path().join("b.patch"), false)
            .await
            .unwrap();
        assert_eq!(png.to_vec(), std::fs::read(repo.join("logo.png")).unwrap());
        remove(&repo, &dir).await;
    }

    #[tokio::test]
    async fn the_repository_root_is_found_from_inside() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo(&tmp.path().join("repo")).await;
        let inside = repo.join("src").join("main");
        std::fs::create_dir_all(&inside).unwrap();
        let st = state(&inside).await;
        assert_eq!(Readiness::Repository, st.readiness);
        assert_eq!(Some(absolute(&repo)), st.root);
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let st = state(&plain).await;
        assert_eq!(Readiness::Folder, st.readiness);
        assert_eq!(Some(absolute(&plain)), st.root);
        let json = serde_json::to_value(&st).unwrap();
        assert_eq!("FOLDER", json["readiness"]);
        assert!(json["reason"].is_null());
    }

    #[tokio::test]
    async fn a_plain_folder_is_worked_on_through_a_private_copy_and_never_gets_git() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("Testing Nook");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("app.py"), "print('hi')\n").unwrap();
        std::fs::write(folder.join("notes.txt"), "one\r\ntwo\r\n").unwrap();
        std::fs::create_dir_all(folder.join("__pycache__")).unwrap();
        std::fs::write(folder.join("__pycache__").join("app.cpython-311.pyc"), "x").unwrap();
        std::fs::create_dir_all(folder.join(".venv").join("lib")).unwrap();
        std::fs::write(folder.join(".venv").join("lib").join("big.py"), "x").unwrap();

        let dir = tmp.path().join("scratch").join("s1");
        let c = create(&folder, &dir).await.unwrap();
        assert!(is_private_copy(&dir));
        assert_eq!("your files as they are now", c.from);
        assert!(dir.join("app.py").exists());
        assert!(!dir.join("__pycache__").exists(), "caches are not copied");
        assert!(!dir.join(".venv").exists(), "environments are not copied");
        assert!(
            dir.join(".git").is_file(),
            "the copy's git record lives beside it, not in it"
        );

        // The worker edits the copy; the person's folder stays as it is until Apply.
        std::fs::write(dir.join("app.py"), "print('hello')\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "one\r\nTWO\r\n").unwrap();
        std::fs::write(dir.join("new.py"), "x = 1\n").unwrap();
        let d = diff(&dir, None, true).await.unwrap();
        assert_eq!(vec!["app.py", "new.py", "notes.txt"], files(&d));
        assert_eq!("print('hi')\n", read(&folder.join("app.py")));

        apply(&folder, &d, &tmp.path().join("apply.patch"), true)
            .await
            .unwrap();
        assert_eq!(
            "print('hello')\n",
            read(&folder.join("app.py")),
            "LF lines stay LF"
        );
        assert_eq!("x = 1\n", read(&folder.join("new.py")));
        assert_eq!(
            b"one\r\nTWO\r\n".to_vec(),
            std::fs::read(folder.join("notes.txt")).unwrap(),
            "Windows line endings come back exactly as they were"
        );
        assert!(
            !folder.join(".git").exists(),
            "the person's folder never gets a repository"
        );

        // Undo and discard work on the copy as on a worktree.
        let baseline = tree(&dir).await.unwrap();
        std::fs::write(dir.join("app.py"), "broken(\n").unwrap();
        reset(&dir, Some(&baseline)).await.unwrap();
        assert_eq!("print('hello')\n", read(&dir.join("app.py")));

        remove(&folder, &dir).await;
        assert!(!dir.exists());
        assert!(!tmp.path().join("scratch").join("s1.git").exists());
        assert!(!folder.join(".git").exists());
    }

    /// A link in the person's folder, to a file or a folder outside it, never brings what it
    /// points at into the scratch copy.
    #[cfg(windows)]
    #[tokio::test]
    async fn links_in_the_folder_are_left_out_of_the_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "the key\n").unwrap();
        let folder = tmp.path().join("project");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("app.py"), "print('hi')\n").unwrap();
        // A junction needs no privilege; a file link needs Developer Mode or an administrator.
        let junction = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(folder.join("linked"))
            .arg(&outside)
            .output()
            .unwrap();
        assert!(junction.status.success(), "{junction:?}");
        let file_link =
            std::os::windows::fs::symlink_file(outside.join("secret.txt"), folder.join("key.txt"))
                .is_ok();

        let dir = tmp.path().join("scratch").join("s3");
        create(&folder, &dir).await.unwrap();
        assert!(dir.join("app.py").is_file());
        assert!(!dir.join("linked").exists(), "a folder link is not copied");
        assert!(!dir.join("linked").join("secret.txt").exists());
        if file_link {
            assert!(!dir.join("key.txt").exists(), "a file link is not copied");
        }
        remove(&folder, &dir).await;
    }

    #[test]
    fn a_path_below_its_root_stays_there() {
        assert!(inside(Path::new("src/main.rs")));
        assert!(!inside(Path::new("../outside")));
        assert!(!inside(Path::new("src/../../x")));
        assert!(!inside(Path::new("C:\\Windows")));
    }

    #[tokio::test]
    async fn a_repository_without_a_commit_is_copied_too() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("empty-repo");
        std::fs::create_dir_all(&folder).unwrap();
        git(&folder, 60, &["init", "-q"]).await.unwrap();
        std::fs::write(folder.join("a.txt"), "a\n").unwrap();
        assert_eq!(Readiness::Folder, state(&folder).await.readiness);
        let dir = tmp.path().join("scratch").join("s2");
        create(&folder, &dir).await.unwrap();
        assert!(is_private_copy(&dir));
        assert!(dir.join("a.txt").exists());
        assert!(
            git(&folder, 60, &["status", "--porcelain"])
                .await
                .unwrap()
                .contains("a.txt"),
            "the person's repository is left as it was"
        );

        let gone = state(&tmp.path().join("gone")).await;
        assert_eq!(Readiness::Missing, gone.readiness);
        assert!(!gone.usable());
    }

    #[test]
    fn a_folder_too_big_to_copy_is_refused_with_a_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("big");
        std::fs::create_dir_all(&folder).unwrap();
        for i in 0..4 {
            std::fs::write(folder.join(format!("f{i}.txt")), "x").unwrap();
        }
        check_copy_size(&folder, 10, 1 << 20).unwrap();
        let e = check_copy_size(&folder, 3, 1 << 20).unwrap_err();
        assert!(e.to_string().contains("too much to copy"), "{e}");
        if let Some(home) = user_home() {
            assert!(check_copy_size(&home, 10, 1 << 20).is_err());
        }
    }

    #[test]
    fn the_files_of_a_diff() {
        let d = "diff --git a/x y.txt b/x y.txt\r\n--- a/x y.txt\t\r\n+++ b/x y.txt\t\r\n@@\r\n--- /dev/null\n+++ b/new.txt\n";
        assert_eq!(vec!["x y.txt", "new.txt"], files(d));
    }
}

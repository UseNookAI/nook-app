//! What the messages call the system Nook runs on: Windows, or on a Mac macOS and the Finder; and
//! on a Mac, the PATH a person's own shell has ([`adopt_shell_path`]).

use std::time::Duration;

/// The system's name in a message ("…pictures macOS could not read either").
pub const SYSTEM_NAME: &str = if cfg!(target_os = "macos") {
    "macOS"
} else {
    "Windows"
};

/// The file manager a folder is shown in.
pub const FILE_MANAGER: &str = if cfg!(target_os = "macos") {
    "the Finder"
} else {
    "Explorer"
};

/// How long the person's login shell may take to say its PATH.
const SHELL_WAIT: Duration = Duration::from_secs(3);
/// What marks the start of the PATH in the shell's output, past whatever its startup prints.
const MARK: &str = "__NOOK_PATH__";

/// A Mac app opened from the Finder or the Dock gets launchd's PATH (`/usr/bin:/bin:...`), not
/// the one the person's shell sets up (Homebrew, rustup, nvm, pyenv), so the Code worker's
/// checks would find none of their toolchains. As other editors do, Nook asks the person's login
/// shell for its PATH once, at start, and takes it; when the shell does not answer in time, the
/// usual places are added instead. Call it before any thread starts. Nothing on Windows, whose
/// apps get the person's PATH.
pub fn adopt_shell_path() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let current = std::env::var("PATH").unwrap_or_default();
    let path = shell_path().unwrap_or_else(|| with_usual_places(&current));
    if !path.is_empty() && path != current {
        std::env::set_var("PATH", path);
    }
}

/// The PATH the person's login shell sets up, or None when it did not say in time.
fn shell_path() -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| s.starts_with('/'))
        .unwrap_or_else(|| "/bin/zsh".to_string());
    let mut child = Command::new(shell)
        .args(["-ilc", &format!("printf '%s' '{MARK}'\"$PATH\"")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < SHELL_WAIT => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    path_from(&out)
}

/// The PATH after the mark in what the shell printed (its startup files may print before it).
fn path_from(output: &str) -> Option<String> {
    let (_, path) = output.rsplit_once(MARK)?;
    let path = path.trim();
    (path.contains('/') && !path.contains('\n')).then(|| path.to_string())
}

/// `current` with the places toolchains usually are on a Mac added after it, those that exist.
fn with_usual_places(current: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut dirs: Vec<String> = current
        .split(':')
        .filter(|d| !d.is_empty())
        .map(String::from)
        .collect();
    for dir in [
        "/opt/homebrew/bin".to_string(),
        "/opt/homebrew/sbin".to_string(),
        "/usr/local/bin".to_string(),
        format!("{home}/.cargo/bin"),
        format!("{home}/.local/bin"),
        format!("{home}/.bun/bin"),
        format!("{home}/.volta/bin"),
        format!("{home}/.pyenv/shims"),
    ] {
        if !dirs.contains(&dir) && std::path::Path::new(&dir).is_dir() {
            dirs.push(dir);
        }
    }
    dirs.join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_path_is_what_follows_the_mark() {
        assert_eq!(
            path_from("Welcome to zsh!\n__NOOK_PATH__/opt/homebrew/bin:/usr/bin").as_deref(),
            Some("/opt/homebrew/bin:/usr/bin")
        );
        assert_eq!(path_from("no mark here"), None);
        assert_eq!(path_from("__NOOK_PATH__"), None);
    }

    #[test]
    fn the_usual_places_come_after_the_path_there_is() {
        let path = with_usual_places("/usr/bin:/bin");
        assert!(path.starts_with("/usr/bin:/bin"));
        assert!(!path.contains("::"));
    }
}

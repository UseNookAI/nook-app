//! Running the downloaded installer and starting the new build afterwards (the original's
//! `VersionUpdateService.launchInstallerAndExit`).
//!
//! The app has to be gone before the installer can replace its files, so it cannot start the new
//! build itself, and the installer must not do it either: the original's Inno Setup ran with
//! Windows 11's Redirection Guard, a mitigation its children inherit, and a Nook started under it
//! could not traverse a junction inside the Nook home (someone with their models on another drive
//! would come back to an app that cannot see them). So, as there, a small script of our own does
//! the whole step: it waits for this process to exit, runs the Tauri NSIS setup silently (`/S`,
//! without `/R`, so Setup starts nothing; with `/UPDATE`, as Tauri's own updater passes it, so a
//! Start menu or desktop shortcut the person removed is not made again), then starts the installed
//! `Nook.exe` itself: the setup installs into the folder the previous install recorded, so that is
//! this exe's path (`%LOCALAPPDATA%\Programs\Nook` for a per-user install). The script is started
//! detached and outside Nook's kill-on-close job ([`crate::process`]), so it outlives the app, and
//! the build it starts inherits nothing from Setup.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The script's file name, in the download folder beside the installer.
pub const SCRIPT_NAME: &str = "nook-update.cmd";
/// The main binary's file name (tauri.conf.json `mainBinaryName`).
pub const MAIN_BINARY: &str = "Nook.exe";
/// What the Tauri NSIS setup leaves in the install folder; its presence beside the running exe
/// says this copy was installed (not a `cargo run` or `tauri dev` build, which must not be
/// replaced by, or restarted as, the installed one).
const UNINSTALLER: &str = "uninstall.exe";
/// How long the script waits for the app to exit before it runs the installer anyway (the
/// installer then closes whatever still holds its files), in two-second rounds: one minute.
const WAIT_ROUNDS: u32 = 30;

/// The exe to start once the installer is done: this one, when it is an installed copy.
pub fn relaunch_target() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let installed = exe
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case(MAIN_BINARY))
        && exe.is_file()
        && exe
            .parent()
            .is_some_and(|dir| dir.join(UNINSTALLER).is_file());
    installed.then_some(exe)
}

/// The script: wait for `pid` to exit, run the installer silently, start `relaunch` (when there is
/// one) and delete the installer.
pub fn script(installer: &Path, relaunch: Option<&Path>, pid: u32) -> String {
    let installer = quote(installer);
    let mut s = String::new();
    s.push_str("@echo off\r\n");
    s.push_str(
        "rem Nook updating itself (nook_core::update::install): wait for the app to exit,\r\n",
    );
    s.push_str("rem install silently, start the new build.\r\n");
    s.push_str("set /a tries=0\r\n");
    s.push_str(":wait\r\n");
    s.push_str(&format!(
        "tasklist /FI \"PID eq {pid}\" /NH 2>nul | find \" {pid} \" >nul || goto install\r\n"
    ));
    s.push_str("set /a tries+=1\r\n");
    s.push_str(&format!("if %tries% geq {WAIT_ROUNDS} goto install\r\n"));
    s.push_str("ping -n 3 127.0.0.1 >nul\r\n");
    s.push_str("goto wait\r\n");
    s.push_str(":install\r\n");
    // /S: silent, no questions; the setup finds the previous install folder by itself. /UPDATE:
    // an update, which leaves the shortcuts as they are.
    s.push_str(&format!("{installer} /S /UPDATE\r\n"));
    if let Some(exe) = relaunch {
        s.push_str("ping -n 3 127.0.0.1 >nul\r\n");
        s.push_str(&format!("start \"\" {}\r\n", quote(exe)));
    }
    s.push_str(&format!("del {installer} >nul 2>&1\r\n"));
    s
}

/// A path in double quotes for the script, with `%` doubled so cmd does not expand it.
fn quote(path: &Path) -> String {
    format!("\"{}\"", path.display().to_string().replace('%', "%%"))
}

/// Writes the script beside the installer and starts it, detached, outside Nook's job. The caller
/// quits the app next; the script waits for that.
pub fn launch(installer: &Path) -> Result<()> {
    let dir = installer.parent().context("the installer has no folder")?;
    let script_path = dir.join(SCRIPT_NAME);
    let relaunch = relaunch_target();
    if relaunch.is_none() {
        tracing::info!(
            "Update: this copy is not an installed one, so the new build is not started afterwards"
        );
    }
    std::fs::write(
        &script_path,
        script(installer, relaunch.as_deref(), std::process::id()),
    )
    .with_context(|| format!("could not write {}", script_path.display()))?;
    spawn_detached(&script_path)
}

#[cfg(windows)]
fn spawn_detached(script: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

    let spawn = |flags: u32| {
        Command::new("cmd.exe")
            .arg("/c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    // Not adopted into crate::process's job, which would kill it with the app. If whatever started
    // Nook put it in a job of its own, break away from that too; a job that forbids breaking away
    // refuses the spawn, and then the script starts inside it (as the original's always did).
    let base = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
    match spawn(base | CREATE_BREAKAWAY_FROM_JOB) {
        Ok(_) => Ok(()),
        Err(_) => spawn(base)
            .map(|_| ())
            .context("could not start the update script"),
    }
}

#[cfg(not(windows))]
fn spawn_detached(_script: &Path) -> Result<()> {
    anyhow::bail!("Nook updates itself only on Windows")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_waits_installs_silently_and_starts_the_new_build() {
        let installer =
            Path::new(r"C:\Users\a\AppData\Local\Nook-rs\tmp\update\Nook_0.5.1_x64-setup.exe");
        let exe = Path::new(r"C:\Users\a\AppData\Local\Programs\Nook\Nook.exe");
        let s = script(installer, Some(exe), 4242);
        let lines: Vec<&str> = s.split("\r\n").collect();
        assert_eq!(lines[0], "@echo off");
        assert!(
            s.contains(
                "tasklist /FI \"PID eq 4242\" /NH 2>nul | find \" 4242 \" >nul || goto install"
            ),
            "{s}"
        );
        let install = lines
            .iter()
            .position(|l| *l == format!("\"{}\" /S /UPDATE", installer.display()))
            .expect("runs the installer silently, as an update");
        let start = lines
            .iter()
            .position(|l| *l == format!("start \"\" \"{}\"", exe.display()))
            .expect("starts the new build");
        let delete = lines
            .iter()
            .position(|l| l.starts_with("del "))
            .expect("deletes the installer");
        assert!(lines.iter().position(|l| *l == ":wait").unwrap() < install);
        assert!(install < start && start < delete, "{s}");
        assert!(!s.contains("/R"), "Setup starts nothing itself: {s}");
    }

    #[test]
    fn a_copy_that_was_not_installed_is_not_started_again() {
        let s = script(Path::new(r"C:\t\setup.exe"), None, 1);
        assert!(!s.contains("start \"\""), "{s}");
        assert!(s.contains("\"C:\\t\\setup.exe\" /S"), "{s}");
    }

    #[test]
    fn percent_signs_in_paths_survive_cmd() {
        let s = script(Path::new(r"C:\100%\setup.exe"), None, 1);
        assert!(s.contains(r#""C:\100%%\setup.exe" /S"#), "{s}");
    }

    #[test]
    fn a_test_binary_is_not_an_installed_copy() {
        assert_eq!(relaunch_target(), None);
    }
}

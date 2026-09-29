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
//!
//! On a Mac the update is the new app itself, `Nook-<version>-macos-arm64.app.tar.gz`. It is
//! unpacked beside the running app (the same volume, so the swap is a rename), its signature is
//! checked when the running app's holds, and a `/bin/sh` script ([`mac_script`]) waits for Nook to
//! exit, puts the new app in the old one's place and opens it. An app run from its disk image or
//! from a translocated copy (a download opened where it lies) cannot be replaced: the person is
//! asked to move it to Applications first.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

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

/// The Mac's script, in the download folder beside the update.
pub const MAC_SCRIPT_NAME: &str = "nook-update.sh";
/// Where a Mac update is unpacked, beside the app it replaces (hidden from the Finder).
pub const MAC_STAGING: &str = ".nook-update";

/// The exe to start once the installer is done: this one, when it is an installed copy. On a Mac,
/// the app bundle Nook runs from, when it is one Nook may replace.
pub fn relaunch_target() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if cfg!(target_os = "macos") {
        return mac_bundle_of(&exe).ok();
    }
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
    if cfg!(target_os = "macos") {
        return launch_mac(installer);
    }
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

/// The script starts in a session of its own, so neither the end of Nook's process group nor the
/// reaper of its engines ends it; its output goes nowhere.
#[cfg(unix)]
fn spawn_detached(script: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    Command::new("/bin/sh")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
        .context("could not start the update script")
}

#[cfg(not(any(windows, unix)))]
fn spawn_detached(_script: &Path) -> Result<()> {
    bail!("Nook cannot update itself on this system")
}

/// The app bundle `exe` runs in (`<bundle>.app/Contents/MacOS/<exe>`), when Nook may replace it:
/// not on a disk image, and not a translocated copy (macOS runs a downloaded app opened where it
/// lies from a hidden, read-only place until it is moved).
pub fn mac_bundle_of(exe: &Path) -> Result<PathBuf> {
    let bundle = exe
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .filter(|b| b.extension().is_some_and(|x| x.eq_ignore_ascii_case("app")))
        .context("Nook is not running from its app, so it cannot update itself")?;
    let text = bundle.to_string_lossy();
    if text.contains("/AppTranslocation/") || text.starts_with("/Volumes/") {
        bail!(
            "Move Nook to the Applications folder and open it from there, so it can update itself"
        );
    }
    Ok(bundle.to_path_buf())
}

/// The Mac's script: wait (a minute at most) for `pid` to exit, move the running app aside, the
/// new one (`staged`) into its place (or the old one back, should that fail), open it, and remove
/// what is left of the update.
pub fn mac_script(update: &Path, staged: &Path, bundle: &Path, pid: u32) -> String {
    let staging = staged.parent().unwrap_or(staged);
    let aside = staging.join("Nook-previous.app");
    let mut s = String::new();
    s.push_str("#!/bin/sh\n");
    s.push_str("# Nook updating itself (nook_core::update::install): wait for the app to exit,\n");
    s.push_str("# put the new app in its place, open it.\n");
    s.push_str(&format!("pid={pid}\n"));
    s.push_str("tries=0\n");
    s.push_str("while kill -0 \"$pid\" 2>/dev/null; do\n");
    s.push_str("  tries=$((tries + 1))\n");
    s.push_str(&format!("  [ \"$tries\" -ge {WAIT_ROUNDS} ] && break\n"));
    s.push_str("  sleep 2\n");
    s.push_str("done\n");
    s.push_str(&format!("app={}\n", sh_quote(bundle)));
    s.push_str(&format!("new={}\n", sh_quote(staged)));
    s.push_str(&format!("aside={}\n", sh_quote(&aside)));
    s.push_str("if mv \"$app\" \"$aside\"; then\n");
    s.push_str("  mv \"$new\" \"$app\" || mv \"$aside\" \"$app\"\n");
    s.push_str("fi\n");
    s.push_str("open \"$app\"\n");
    s.push_str(&format!(
        "rm -rf {} {}\n",
        sh_quote(staging),
        sh_quote(update)
    ));
    s
}

/// A path in single quotes for `/bin/sh`, a quote in it closed, escaped and reopened.
fn sh_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// Unpacks a Mac update beside the running app, checks it, and starts the script that swaps them.
fn launch_mac(update: &Path) -> Result<()> {
    use std::process::Command;

    let exe = std::env::current_exe().context("Nook cannot tell where it runs from")?;
    let bundle = mac_bundle_of(&exe)?;
    let parent = bundle.parent().context("the app has no folder")?;
    let staging = parent.join(MAC_STAGING);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).with_context(|| {
        format!(
            "Nook cannot update itself in {}, which this account cannot change; download the new version from usenook.ai instead",
            parent.display()
        )
    })?;
    // The system's tar keeps what a bundle is made of: links, permissions, the signature.
    let unpacked = Command::new("/usr/bin/tar")
        .arg("-xzf")
        .arg(update)
        .arg("-C")
        .arg(&staging)
        .status()
        .context("could not start tar")?;
    if !unpacked.success() {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("could not unpack {}", update.display());
    }
    let staged = std::fs::read_dir(&staging)?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("app")) && p.is_dir())
        .context("the update holds no app")?;
    let holds = |app: &Path| {
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(app)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    if holds(&bundle) && !holds(&staged) {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("the downloaded app's signature does not hold");
    }
    let dir = update.parent().context("the update has no folder")?;
    let script_path = dir.join(MAC_SCRIPT_NAME);
    std::fs::write(
        &script_path,
        mac_script(update, &staged, &bundle, std::process::id()),
    )
    .with_context(|| format!("could not write {}", script_path.display()))?;
    spawn_detached(&script_path)
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

    #[test]
    fn a_mac_app_is_replaced_only_where_it_was_put() {
        let exe = |p: &str| PathBuf::from(p);
        assert_eq!(
            mac_bundle_of(&exe("/Applications/Nook.app/Contents/MacOS/Nook")).unwrap(),
            PathBuf::from("/Applications/Nook.app")
        );
        assert!(mac_bundle_of(&exe("/Users/me/code/nook/target/release/Nook")).is_err());
        let dmg = mac_bundle_of(&exe("/Volumes/Nook/Nook.app/Contents/MacOS/Nook")).unwrap_err();
        assert!(dmg.to_string().contains("Applications folder"), "{dmg}");
        assert!(mac_bundle_of(&exe(
            "/private/var/folders/x/T/AppTranslocation/1234/d/Nook.app/Contents/MacOS/Nook"
        ))
        .is_err());
    }

    #[test]
    fn the_mac_script_swaps_the_apps_after_nook_exits_and_opens_the_new_one() {
        let s = mac_script(
            Path::new(
                "/Users/me/Library/Application Support/Nook/tmp/Nook-0.6.0-macos-arm64.app.tar.gz",
            ),
            Path::new("/Applications/.nook-update/Nook.app"),
            Path::new("/Applications/Nook's.app"),
            4242,
        );
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[0], "#!/bin/sh");
        assert!(s.contains("pid=4242\n"), "{s}");
        let wait = lines
            .iter()
            .position(|l| l.starts_with("while kill -0"))
            .unwrap();
        let swap = lines
            .iter()
            .position(|l| l.starts_with("if mv \"$app\""))
            .unwrap();
        let open = lines.iter().position(|l| *l == "open \"$app\"").unwrap();
        let clean = lines.iter().position(|l| l.starts_with("rm -rf ")).unwrap();
        assert!(wait < swap && swap < open && open < clean, "{s}");
        // a quote in a path is closed, escaped and reopened
        assert!(s.contains(r"app='/Applications/Nook'\''s.app'"), "{s}");
        assert!(
            s.contains("mv \"$new\" \"$app\" || mv \"$aside\" \"$app\""),
            "{s}"
        );
        assert!(s.contains("'/Applications/.nook-update' "), "{s}");
    }
}

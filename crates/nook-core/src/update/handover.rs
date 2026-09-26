//! Taking over from where an older Nook ran.
//!
//! The Kotlin Nook's updater (`VersionUpdateService.launchInstallerAndExit`, from 0.3.0+ecd64d8 on)
//! runs the installer the update manifest names, waits for it, and then starts
//! `%LOCALAPPDATA%\Nook\app\Nook.exe`, the program it was itself. This app's installer
//! (`src-tauri/windows/hooks.nsh`) uninstalls that app and leaves a `Nook.exe` there, a hard link of
//! (or on another volume a copy of) the installed one, with [`MARKER`] beside it. Started from
//! there, the app hands over to the installed Nook before anything else runs
//! ([`forward_to_installed`]), so the old updater's restart ends in the new app, running once (a
//! start while it runs already only brings its window forward), and nothing runs from the old
//! program folder. The installed Nook removes the pair once the updater is done with it
//! ([`remove_leftovers`]). A build Tauri's stock installer had put in `%LOCALAPPDATA%\Nook`
//! itself, which the installer moves out, is covered the same way.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::install::MAIN_BINARY;

/// What the installer leaves beside a forwarding `Nook.exe` (hooks.nsh `NOOK_HANDOVER_MARKER`).
pub const MARKER: &str = "nook-handover.txt";
/// Passed to the installed Nook by a forwarder, which says the forwarder has done its work.
pub const HANDED_OVER_ARG: &str = "--handed-over";
/// Beside an installed copy (Tauri's NSIS uninstaller); a forwarder never has one.
const UNINSTALLER: &str = "uninstall.exe";
/// How long a forwarder that did not start this app is kept: the Kotlin updater starts it two
/// seconds after the installer is done, so after this nothing will.
const KEEP_UNUSED: Duration = Duration::from_secs(10 * 60);
/// How long removing a forwarder is tried for, in rounds of half a second: it may still be running.
const REMOVE_ROUNDS: u32 = 20;

/// The folders an older Nook ran from, where the installer may leave a forwarder: the Kotlin
/// Nook's `%LOCALAPPDATA%\Nook\app`, and `%LOCALAPPDATA%\Nook`, the stock template's default.
pub fn old_program_dirs() -> Vec<PathBuf> {
    let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) else {
        return Vec::new();
    };
    let old = PathBuf::from(local).join("Nook");
    vec![old.join("app"), old]
}

/// True when `exe` is a forwarder: the installer's marker beside it, and no uninstaller (an
/// installed copy is never one).
pub fn is_forwarder(exe: &Path) -> bool {
    let Some(dir) = exe.parent() else {
        return false;
    };
    exe.file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case(MAIN_BINARY))
        && dir.join(MARKER).is_file()
        && !dir.join(UNINSTALLER).exists()
}

/// Called first thing at start: when this process is a forwarder, starts the installed Nook with
/// the same arguments and [`HANDED_OVER_ARG`] and returns true, and the caller exits. False (the
/// app runs as usual) otherwise, and when there is no installed Nook to hand over to.
pub fn forward_to_installed() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    if !is_forwarder(&exe) {
        return false;
    }
    let target = installed_exe().filter(|t| t.is_file() && !same_file_path(t, &exe));
    let Some(target) = target else {
        tracing::warn!(
            "{} stands in for an installed Nook, but none was found; running from here",
            exe.display()
        );
        return false;
    };
    let args: Vec<std::ffi::OsString> = std::env::args_os()
        .skip(1)
        .filter(|a| a != HANDED_OVER_ARG)
        .collect();
    match spawn(&target, &args) {
        Ok(()) => {
            tracing::info!(
                "Started from {}, where an older Nook was; handed over to {}",
                exe.display(),
                target.display()
            );
            true
        }
        Err(e) => {
            tracing::warn!(
                "Could not start {} from {}: {e}; running from here",
                target.display(),
                exe.display()
            );
            false
        }
    }
}

/// Whether this start came through a forwarder.
pub fn handed_over() -> bool {
    std::env::args_os().any(|a| a == HANDED_OVER_ARG)
}

/// The installed `Nook.exe`: in the folder the installer recorded (`HKCU\Software\Nook\Nook`, Tauri's
/// `MANUPRODUCTKEY`), else in `%LOCALAPPDATA%\Programs\Nook`, where a per-user install goes.
pub fn installed_exe() -> Option<PathBuf> {
    let recorded = registered_install_dir().map(|d| d.join(MAIN_BINARY));
    recorded.filter(|e| e.is_file()).or_else(|| {
        let local = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty())?;
        let exe = PathBuf::from(local)
            .join("Programs")
            .join("Nook")
            .join(MAIN_BINARY);
        exe.is_file().then_some(exe)
    })
}

/// Removes, in the background, the forwarders in `dirs` that are no longer needed: all of them
/// when this start came through one (`handed_over`: the old updater is done with it), else those
/// left more than ten minutes ago. Called by an installed copy only.
pub fn remove_leftovers(dirs: Vec<PathBuf>, handed_over: bool) {
    std::thread::spawn(move || {
        for dir in dirs {
            match remove_leftover(&dir, handed_over, SystemTime::now(), REMOVE_ROUNDS) {
                Ok(true) => tracing::info!(
                    "Removed the Nook.exe the installer had left in {} for the old Nook's updater",
                    dir.display()
                ),
                Ok(false) => {}
                Err(e) => tracing::warn!(
                    "Could not remove the Nook.exe left in {}: {e}; trying again at the next start",
                    dir.display()
                ),
            }
        }
    });
}

/// Removes the forwarder in `dir` when there is one and it is due: its `Nook.exe` (tried `rounds`
/// times, half a second apart, as it may still be running), then the marker, then the folder when
/// that is the Kotlin Nook's `app` folder and empty. True when it removed one.
fn remove_leftover(
    dir: &Path,
    handed_over: bool,
    now: SystemTime,
    rounds: u32,
) -> std::io::Result<bool> {
    let marker = dir.join(MARKER);
    let Ok(meta) = std::fs::metadata(&marker) else {
        return Ok(false);
    };
    if dir.join(UNINSTALLER).exists() {
        return Ok(false);
    }
    let age = meta
        .modified()
        .ok()
        .and_then(|m| now.duration_since(m).ok())
        .unwrap_or_default();
    if !handed_over && age < KEEP_UNUSED {
        return Ok(false);
    }
    let exe = dir.join(MAIN_BINARY);
    let mut round = 0;
    loop {
        match std::fs::remove_file(&exe) {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => {
                round += 1;
                if round >= rounds {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    std::fs::remove_file(&marker)?;
    if dir
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("app"))
    {
        // Only when empty: the Kotlin Nook's own files, should any be left, stay.
        let _ = std::fs::remove_dir(dir);
    }
    Ok(true)
}

fn same_file_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

#[cfg(windows)]
fn registered_install_dir() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};

    let key: Vec<u16> = "Software\\Nook\\Nook\0".encode_utf16().collect();
    let mut buf = vec![0u16; 2048];
    let mut bytes = (buf.len() * 2) as u32;
    // SAFETY: the key is NUL-terminated, the buffer holds `bytes` bytes, and the null value name
    // asks for the key's default value.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            std::ptr::null(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if rc != 0 {
        return None;
    }
    let len = (bytes as usize / 2).min(buf.len());
    let text = &buf[..len];
    let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    let dir = PathBuf::from(std::ffi::OsString::from_wide(&text[..end]));
    (!dir.as_os_str().is_empty()).then_some(dir)
}

#[cfg(not(windows))]
fn registered_install_dir() -> Option<PathBuf> {
    None
}

/// Starts the installed Nook outside any job this process is in, so it outlives the forwarder.
#[cfg(windows)]
fn spawn(exe: &Path, args: &[std::ffi::OsString]) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let start = |flags: u32| {
        let mut c = Command::new(exe);
        c.args(args).arg(HANDED_OVER_ARG).creation_flags(flags);
        if let Some(dir) = exe.parent() {
            c.current_dir(dir);
        }
        c.spawn()
    };
    match start(CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB) {
        Ok(_) => Ok(()),
        Err(_) => start(CREATE_NEW_PROCESS_GROUP).map(|_| ()),
    }
}

#[cfg(not(windows))]
fn spawn(exe: &Path, args: &[std::ffi::OsString]) -> std::io::Result<()> {
    std::process::Command::new(exe)
        .args(args)
        .arg(HANDED_OVER_ARG)
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder as the installer leaves it: Nook.exe and the marker.
    fn forwarder(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let exe = dir.join("Nook.exe");
        std::fs::write(&exe, "exe").unwrap();
        std::fs::write(dir.join(MARKER), "moved").unwrap();
        exe
    }

    #[test]
    fn a_forwarder_is_a_nook_exe_with_the_marker_and_no_uninstaller() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = forwarder(&tmp.path().join("Nook").join("app"));
        assert!(is_forwarder(&exe));
        assert!(!is_forwarder(&exe.with_file_name("Other.exe")));
        std::fs::write(exe.with_file_name(UNINSTALLER), "u").unwrap();
        assert!(!is_forwarder(&exe), "an installed copy is never one");

        let plain = tmp.path().join("Programs").join("Nook");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("Nook.exe"), "exe").unwrap();
        assert!(!is_forwarder(&plain.join("Nook.exe")), "no marker");
    }

    #[test]
    fn the_test_binary_is_not_a_forwarder() {
        assert!(!forward_to_installed());
        assert!(!handed_over());
    }

    #[test]
    fn a_forwarder_goes_once_the_old_updater_is_done_with_it() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("Nook").join("app");
        forwarder(&app);
        let now = SystemTime::now();

        // Just left, and this start did not come through it: the updater may still start it.
        assert!(!remove_leftover(&app, false, now, 1).unwrap());
        assert!(app.join("Nook.exe").is_file());
        // Ten minutes on, nothing will.
        assert!(remove_leftover(&app, false, now + KEEP_UNUSED, 1).unwrap());
        assert!(!app.exists(), "the empty app folder goes too");

        // Through it: at once.
        forwarder(&app);
        assert!(remove_leftover(&app, true, now, 1).unwrap());
        assert!(!app.exists());
        assert!(
            !remove_leftover(&app, true, now, 1).unwrap(),
            "nothing left"
        );
    }

    #[test]
    fn only_the_forwarder_itself_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("Nook");
        forwarder(&old);
        std::fs::create_dir_all(old.join("models")).unwrap();
        std::fs::write(old.join("models").join("m.gguf"), "model").unwrap();
        assert!(remove_leftover(&old, true, SystemTime::now(), 1).unwrap());
        assert!(!old.join("Nook.exe").exists() && !old.join(MARKER).exists());
        assert!(
            old.join("models").join("m.gguf").is_file(),
            "the old data stays"
        );

        // Kotlin files still in its app folder keep it, and without the marker nothing goes.
        let app = old.join("app");
        forwarder(&app);
        std::fs::write(app.join("unins000.dat"), "x").unwrap();
        assert!(remove_leftover(&app, true, SystemTime::now(), 1).unwrap());
        assert!(app.join("unins000.dat").is_file());
        std::fs::write(app.join("Nook.exe"), "kotlin").unwrap();
        assert!(!remove_leftover(&app, true, SystemTime::now(), 1).unwrap());
        assert!(app.join("Nook.exe").is_file());
    }

    #[test]
    fn the_old_program_folders_are_under_local_app_data() {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let old = PathBuf::from(local).join("Nook");
            assert_eq!(vec![old.join("app"), old], old_program_dirs());
        }
    }
}

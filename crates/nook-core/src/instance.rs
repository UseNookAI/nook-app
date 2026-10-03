//! One Nook at a time, even when several start in the same moment, and never a Nook left running
//! without its window.
//!
//! tauri-plugin-single-instance hands a second start over to the running copy only once that copy
//! has made its message window; a copy that starts in the same moment finds the plugin's mutex
//! taken but no window yet, and carries on as a second Nook. That happened on the update to 0.5.21:
//! the update's own restart and two clicks made while Windows checked the new Nook.exe all started
//! within a millisecond, none of the three could make its WebView2 window ("The parameter is
//! incorrect"), and all three stayed on without one, the first holding the single instance, so
//! every later click went to a Nook with no window.
//!
//! So the shell calls [`wait_turn`] before anything else: the first copy takes a mutex of its own,
//! and a copy that finds it taken waits until the first copy's single-instance window is there
//! (the plugin then hands this start over to it) or, if none comes, ends. A Nook whose window
//! never loaded the page starts a fresh copy of itself with [`relaunch`] and ends; the fresh copy
//! waits for it to be gone ([`wait_for_exit`]) before it starts.

use std::time::Duration;

/// The argument a relaunched copy gets: the process it replaces, to wait for.
pub const AFTER_ARG: &str = "--after";
/// Marks a copy started by [`relaunch`], which never relaunches itself again.
pub const RELAUNCHED_ARG: &str = "--relaunched";
/// How long a second copy waits for the first one's window before it gives up and ends.
const FIRST_COPY_WAIT: Duration = Duration::from_secs(20);

/// What [`wait_turn`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turn {
    /// No other copy: this one is Nook.
    First,
    /// Another copy runs and its window is there: the single-instance plugin hands this start to
    /// it and ends this process.
    HandOver,
    /// Another copy is starting but never got its window up: this one should end.
    GiveUp,
}

/// The pid a relaunched copy should wait for, from its arguments.
pub fn after_pid(args: &[String]) -> Option<u32> {
    let at = args.iter().position(|a| a == AFTER_ARG)?;
    args.get(at + 1)?.parse().ok()
}

/// Whether this copy was started by [`relaunch`].
pub fn relaunched(args: &[String]) -> bool {
    args.iter().any(|a| a == RELAUNCHED_ARG)
}

/// The names tauri-plugin-single-instance gives its message window, for `identifier`.
fn plugin_window(identifier: &str) -> (String, String) {
    (format!("{identifier}-sic"), format!("{identifier}-siw"))
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Takes this copy's turn: see the module docs. The mutex stays held until the process ends.
#[cfg(windows)]
pub fn wait_turn(identifier: &str) -> Turn {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;

    let name = wide(&format!("{identifier}-first"));
    // Never closed: Windows lets go of it when this process ends.
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if mutex.is_null() || unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        return Turn::First;
    }
    let (class, window) = plugin_window(identifier);
    let (class, window) = (wide(&class), wide(&window));
    let started = std::time::Instant::now();
    while started.elapsed() < FIRST_COPY_WAIT {
        if !unsafe { FindWindowW(class.as_ptr(), window.as_ptr()) }.is_null() {
            return Turn::HandOver;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Turn::GiveUp
}

#[cfg(not(windows))]
pub fn wait_turn(_identifier: &str) -> Turn {
    Turn::First
}

/// Waits up to `limit` for process `pid` to end (at once when it has, or never existed).
#[cfg(windows)]
pub fn wait_for_exit(pid: u32, limit: Duration) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return;
    }
    unsafe {
        WaitForSingleObject(handle, limit.as_millis().min(u32::MAX as u128) as u32);
        CloseHandle(handle);
    }
}

#[cfg(not(windows))]
pub fn wait_for_exit(_pid: u32, _limit: Duration) {}

/// Starts a fresh copy of this exe that waits for this process to end first. Detached and outside
/// any job this process is in, so it outlives it.
pub fn relaunch() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut command = std::process::Command::new(exe);
    command
        .arg(AFTER_ARG)
        .arg(std::process::id().to_string())
        .arg(RELAUNCHED_ARG)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        if command.spawn().is_ok() {
            return Ok(());
        }
        // A job that forbids breaking away refuses the spawn; start inside it then.
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    command.spawn().map(|_| ())
}

/// A message box, for when there is no window to say it in.
#[cfg(windows)]
pub fn alert(title: &str, text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let (title, text) = (wide(title), wide(text));
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
pub fn alert(title: &str, text: &str) {
    eprintln!("{title}: {text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_relaunched_copy_knows_whom_to_wait_for() {
        let a = args(&["Nook.exe", AFTER_ARG, "4242", RELAUNCHED_ARG]);
        assert_eq!(after_pid(&a), Some(4242));
        assert!(relaunched(&a));
        let plain = args(&["Nook.exe"]);
        assert_eq!(after_pid(&plain), None);
        assert!(!relaunched(&plain));
        assert_eq!(after_pid(&args(&["Nook.exe", AFTER_ARG])), None);
        assert_eq!(after_pid(&args(&["Nook.exe", AFTER_ARG, "x"])), None);
    }

    #[test]
    fn the_plugin_window_is_named_as_the_plugin_names_it() {
        assert_eq!(
            plugin_window("ai.nook.app"),
            ("ai.nook.app-sic".to_string(), "ai.nook.app-siw".to_string())
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_first_copy_goes_first_and_a_second_one_waits_for_its_window() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, WNDCLASSEXW,
            WS_EX_TOOLWINDOW, WS_OVERLAPPED,
        };

        let id = format!("ai.nook.test-{}", std::process::id());
        assert_eq!(wait_turn(&id), Turn::First);

        // A second copy, while the first has its window (a hidden top-level one, as the plugin
        // makes: FindWindowW does not see message-only windows): it hands over at once.
        let (class, window) = plugin_window(&id);
        let (class, window) = (wide(&class), wide(&window));
        unsafe extern "system" fn proc_(
            h: windows_sys::Win32::Foundation::HWND,
            m: u32,
            w: usize,
            l: isize,
        ) -> isize {
            unsafe { DefWindowProcW(h, m, w, l) }
        }
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc_),
            lpszClassName: class.as_ptr(),
            ..unsafe { std::mem::zeroed() }
        };
        unsafe { RegisterClassExW(&wc) };
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class.as_ptr(),
                window.as_ptr(),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null());
        assert_eq!(wait_turn(&id), Turn::HandOver);
        unsafe { DestroyWindow(hwnd) };
    }

    #[cfg(windows)]
    #[test]
    fn waiting_for_a_process_that_is_gone_returns_at_once() {
        let started = std::time::Instant::now();
        // Pid 0 is the idle process, which cannot be opened: nothing to wait for.
        wait_for_exit(0, Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}

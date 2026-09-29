//! What can be recorded: the screens and the windows open on them, as Windows lists them. A
//! screen or a window is named to the capture by its handle (HMONITOR, HWND), which is what
//! FFmpeg's `gfxcapture` takes.

use serde::{Deserialize, Serialize};

/// A screen (a monitor), in the desktop's pixels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Screen {
    /// Windows' handle for it (HMONITOR).
    pub handle: u64,
    /// "Screen 1", in Windows' order.
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// A window that can be recorded: an application's own, on screen, with a title.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    /// Windows' handle for it (HWND).
    pub handle: u64,
    pub title: String,
    /// The program's file name without ".exe": "chrome", "Code".
    pub app: String,
    pub width: u32,
    pub height: u32,
}

/// The titles Nook gives its own recording windows (the area picker, the controls), which are
/// never offered for recording.
pub const OWN_TITLES: [&str; 2] = ["Nook area picker", "Nook recording controls"];

/// The screens, in Windows' order, the main one flagged.
pub fn screens() -> Vec<Screen> {
    #[cfg(windows)]
    {
        imp::screens()
    }
    #[cfg(not(windows))]
    Vec::new()
}

/// The windows that can be recorded, the one in front first.
pub fn windows() -> Vec<Window> {
    #[cfg(windows)]
    {
        imp::windows()
    }
    #[cfg(not(windows))]
    Vec::new()
}

/// The screen (its handle) most of a window is on.
pub fn monitor_of_window(handle: u64) -> Option<u64> {
    #[cfg(windows)]
    {
        imp::monitor_of_window(handle)
    }
    #[cfg(not(windows))]
    {
        let _ = handle;
        None
    }
}

/// The person's Videos folder.
pub fn videos_folder() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        imp::videos_folder()
    }
    #[cfg(not(windows))]
    None
}

/// Keeps a window of Nook's (its handle) out of every screen capture, this recording's too:
/// it still shows on screen. Windows 10 2004 and later; elsewhere it stays in.
pub fn exclude_from_capture(handle: isize) {
    #[cfg(windows)]
    imp::exclude_from_capture(handle);
    #[cfg(not(windows))]
    let _ = handle;
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, RECT};
    use windows_sys::Win32::Graphics::Dwm::{
        DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
    };
    use windows_sys::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowLongW, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, IsWindowVisible, SetWindowDisplayAffinity, GWL_EXSTYLE,
        GW_OWNER, WDA_EXCLUDEFROMCAPTURE, WS_EX_TOOLWINDOW,
    };

    use super::{Screen, Window, OWN_TITLES};

    const MONITORINFOF_PRIMARY: u32 = 1;

    pub(super) fn screens() -> Vec<Screen> {
        unsafe extern "system" fn each(
            monitor: HMONITOR,
            _dc: HDC,
            _rect: *mut RECT,
            data: LPARAM,
        ) -> BOOL {
            // SAFETY: `data` is the Vec below, alive for the whole enumeration.
            let found = unsafe { &mut *(data as *mut Vec<Screen>) };
            let mut info: MONITORINFOEXW = unsafe { std::mem::zeroed() };
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            // SAFETY: a MONITORINFOEXW sized as it says.
            if unsafe {
                GetMonitorInfoW(
                    monitor,
                    &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
                )
            } != 0
            {
                let r = info.monitorInfo.rcMonitor;
                found.push(Screen {
                    handle: monitor as usize as u64,
                    name: format!("Screen {}", found.len() + 1),
                    x: r.left,
                    y: r.top,
                    width: (r.right - r.left).max(0) as u32,
                    height: (r.bottom - r.top).max(0) as u32,
                    primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                });
            }
            1
        }
        let mut found: Vec<Screen> = Vec::new();
        // SAFETY: the callback only writes into `found`.
        unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(each),
                &mut found as *mut Vec<Screen> as LPARAM,
            );
        }
        found
    }

    pub(super) fn windows() -> Vec<Window> {
        unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
            // SAFETY: `data` is the Vec below, alive for the whole enumeration.
            let found = unsafe { &mut *(data as *mut Vec<Window>) };
            if let Some(w) = recordable(hwnd) {
                found.push(w);
            }
            1
        }
        let mut found: Vec<Window> = Vec::new();
        // SAFETY: the callback only writes into `found`.
        unsafe {
            EnumWindows(Some(each), &mut found as *mut Vec<Window> as LPARAM);
        }
        found
    }

    /// The window, when it is one a person would record: shown, not minimised, not cloaked (on
    /// another virtual desktop, or an app parked by Windows), not a tool window or one owned by
    /// another, with a title and a size.
    fn recordable(hwnd: HWND) -> Option<Window> {
        // SAFETY: plain queries on a window handle Windows just gave; a stale one fails them.
        unsafe {
            if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
                return None;
            }
            if !GetWindow(hwnd, GW_OWNER).is_null() {
                return None;
            }
            if GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW != 0 {
                return None;
            }
            let mut cloaked: u32 = 0;
            if DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED as u32,
                &mut cloaked as *mut u32 as *mut c_void,
                std::mem::size_of::<u32>() as u32,
            ) == 0
                && cloaked != 0
            {
                return None;
            }
            let len = GetWindowTextLengthW(hwnd);
            if len <= 0 {
                return None;
            }
            let mut title = vec![0u16; len as usize + 1];
            let got = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
            let title = String::from_utf16_lossy(&title[..got.max(0) as usize])
                .trim()
                .to_string();
            if title.is_empty()
                || title == "Program Manager"
                || OWN_TITLES.contains(&title.as_str())
            {
                return None;
            }
            let mut rect: RECT = std::mem::zeroed();
            if DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                &mut rect as *mut RECT as *mut c_void,
                std::mem::size_of::<RECT>() as u32,
            ) != 0
            {
                return None;
            }
            let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
            if width < 40 || height < 40 {
                return None;
            }
            Some(Window {
                handle: hwnd as usize as u64,
                title,
                app: app_of(hwnd).unwrap_or_default(),
                width: width as u32,
                height: height as u32,
            })
        }
    }

    /// The file name, without ".exe", of the program that owns `hwnd`.
    fn app_of(hwnd: HWND) -> Option<String> {
        // SAFETY: plain calls; the process handle is closed before returning.
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return None;
            }
            let mut buf = vec![0u16; 1024];
            let mut len = buf.len() as u32;
            let ok =
                QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
            CloseHandle(process);
            if ok == 0 {
                return None;
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            std::path::Path::new(&path)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        }
    }

    pub(super) fn monitor_of_window(handle: u64) -> Option<u64> {
        use windows_sys::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONULL};
        // SAFETY: a plain query; a closed window gives no monitor.
        let m = unsafe { MonitorFromWindow(handle as usize as HWND, MONITOR_DEFAULTTONULL) };
        (!m.is_null()).then_some(m as usize as u64)
    }

    pub(super) fn videos_folder() -> Option<std::path::PathBuf> {
        use windows_sys::Win32::UI::Shell::{FOLDERID_Videos, SHGetKnownFolderPath};
        let mut out: *mut u16 = std::ptr::null_mut();
        // SAFETY: the shell allocates the string, freed below.
        let hr =
            unsafe { SHGetKnownFolderPath(&FOLDERID_Videos, 0, std::ptr::null_mut(), &mut out) };
        if hr != 0 || out.is_null() {
            return None;
        }
        // SAFETY: a NUL-terminated wide string from the shell.
        let path = unsafe {
            let len = (0..).take_while(|&i| *out.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(out, len));
            windows_sys::Win32::System::Com::CoTaskMemFree(out as *const c_void);
            s
        };
        Some(std::path::PathBuf::from(path))
    }

    pub(super) fn exclude_from_capture(handle: isize) {
        // SAFETY: a plain call on a window of this process; a wrong handle only fails it.
        let ok = unsafe { SetWindowDisplayAffinity(handle as HWND, WDA_EXCLUDEFROMCAPTURE) };
        if ok == 0 {
            tracing::debug!(
                "This window stays in screen captures: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn this_computer_has_a_main_screen_and_its_windows_have_titles() {
        let screens = screens();
        assert!(!screens.is_empty());
        assert_eq!(
            screens.iter().filter(|s| s.primary).count(),
            1,
            "{screens:?}"
        );
        assert!(screens
            .iter()
            .all(|s| s.width > 0 && s.height > 0 && s.handle != 0));
        assert_eq!(screens[0].name, "Screen 1");
        // A test machine may show no windows at all; those it shows are recordable ones.
        for w in windows() {
            assert!(!w.title.is_empty() && w.handle != 0, "{w:?}");
            assert!(!OWN_TITLES.contains(&w.title.as_str()));
        }
    }
}

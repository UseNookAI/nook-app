//! What the converter finds on the computer: Microsoft Edge, which prints web pages to PDF, and
//! Word, Excel and PowerPoint. Windows lists its programs under "App Paths" in the registry;
//! Office's click-to-run installs are there too.
//!
//! On a Mac the printer is whichever Chromium browser is in Applications (Edge, Chrome, Brave,
//! Chromium: the same headless printing), and Office is never driven: the Mac's Office has no
//! automation Nook can run hidden, so Office files go through LibreOffice.

use std::path::PathBuf;

use super::routes::Have;

/// What the browser that prints to PDF is called in the converter's words.
pub const PRINTER: &str = if cfg!(target_os = "macos") {
    "Chrome or Edge"
} else {
    "Microsoft Edge"
};
/// Its short name, in the list of what a conversion goes through.
pub const PRINTER_SHORT: &str = if cfg!(target_os = "macos") {
    "the browser"
} else {
    "Edge"
};

/// The browser that prints web pages to PDF: Microsoft Edge (on every Windows 10 and 11), or on a
/// Mac the first Chromium browser in Applications.
pub fn edge() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return mac_browser();
    }
    app_path("msedge.exe").or_else(|| {
        let mut places = Vec::new();
        for var in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                places.push(PathBuf::from(base).join(r"Microsoft\Edge\Application\msedge.exe"));
            }
        }
        places.into_iter().find(|p| p.is_file())
    })
}

/// Edge, Chrome, Brave or Chromium in /Applications or ~/Applications, as they print alike.
fn mac_browser() -> Option<PathBuf> {
    const APPS: [(&str, &str); 4] = [
        ("Microsoft Edge.app", "Microsoft Edge"),
        ("Google Chrome.app", "Google Chrome"),
        ("Brave Browser.app", "Brave Browser"),
        ("Chromium.app", "Chromium"),
    ];
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    APPS.iter()
        .flat_map(|(app, exe)| {
            roots
                .iter()
                .map(move |r| r.join(app).join("Contents").join("MacOS").join(exe))
        })
        .find(|p| p.is_file())
}

/// The Office programs on the computer (none a Mac can drive).
pub fn microsoft_office() -> Have {
    if cfg!(target_os = "macos") {
        return Have {
            word: false,
            excel: false,
            powerpoint: false,
        };
    }
    Have {
        word: app_path("winword.exe").is_some(),
        excel: app_path("excel.exe").is_some(),
        powerpoint: app_path("powerpnt.exe").is_some(),
    }
}

/// A program's path from "App Paths" (the machine's, then the user's), when the file is there.
pub fn app_path(exe: &str) -> Option<PathBuf> {
    registry::app_path(exe).filter(|p| p.is_file())
}

#[cfg(windows)]
mod registry {
    use std::path::PathBuf;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
    };

    pub fn app_path(exe: &str) -> Option<PathBuf> {
        let key: Vec<u16> = format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe}")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER]
            .into_iter()
            .find_map(|root| default_value(root, &key))
    }

    fn default_value(root: HKEY, key: &[u16]) -> Option<PathBuf> {
        let mut buf = [0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: a NUL-terminated key, the default value (null name), and a buffer with its
        // length in bytes.
        let r = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                std::ptr::null(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr() as *mut _,
                &mut len,
            )
        };
        if r != ERROR_SUCCESS {
            return None;
        }
        let chars = (len as usize / 2).min(buf.len());
        let s = String::from_utf16_lossy(&buf[..chars]);
        let s = s.trim_end_matches('\0').trim().trim_matches('"');
        (!s.is_empty()).then(|| PathBuf::from(s))
    }
}

#[cfg(not(windows))]
mod registry {
    pub fn app_path(_exe: &str) -> Option<std::path::PathBuf> {
        None
    }
}

#[cfg(test)]
mod tests {
    /// This machine's Edge is found (every Windows 10 and 11 has one).
    #[cfg(windows)]
    #[test]
    fn edge_is_found() {
        let edge = super::edge().expect("Edge");
        assert!(edge.ends_with("msedge.exe"), "{}", edge.display());
    }
}

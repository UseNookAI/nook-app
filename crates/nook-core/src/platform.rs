//! What the messages call the system Nook runs on: Windows, or on a Mac macOS and the Finder.

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

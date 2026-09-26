//! Starting child processes the Windows way: no console window flashes up, and engines
//! (llama-server, whisper-server, sd, ffmpeg...) die with Nook even if it is killed, because each
//! one is put in a kill-on-close job object that only this process holds.
//!
//! Use [`command`] for tokio children and [`std_command`] for blocking ones, then [`adopt`] (or
//! [`spawn_managed`]) any long-lived child so it cannot outlive the app. Short commands (git,
//! verify checks) need no job: they are awaited and killed on drop.

use std::ffi::OsStr;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A tokio command with no console window, stdin closed, and kill-on-drop.
pub fn command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    cmd.stdin(std::process::Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// A blocking command with no console window and stdin closed.
pub fn std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Spawns and adopts in one step.
pub fn spawn_managed(cmd: &mut tokio::process::Command) -> std::io::Result<tokio::process::Child> {
    let child = cmd.spawn()?;
    adopt(&child);
    Ok(child)
}

/// Puts a running child in Nook's kill-on-close job, so it ends when Nook ends.
pub fn adopt(child: &tokio::process::Child) {
    #[cfg(windows)]
    if let Some(handle) = child.raw_handle() {
        job::assign(handle as _);
    }
    #[cfg(not(windows))]
    let _ = child;
}

/// Same as [`adopt`] for a blocking child.
pub fn adopt_std(child: &std::process::Child) {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        job::assign(child.as_raw_handle() as _);
    }
    #[cfg(not(windows))]
    let _ = child;
}

#[cfg(windows)]
mod job {
    use once_cell::sync::Lazy;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    struct Job(HANDLE);
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    static JOB: Lazy<Option<Job>> = Lazy::new(|| unsafe {
        let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if handle.is_null() {
            tracing::warn!("could not create the process job object");
            return None;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            tracing::warn!("could not configure the process job object");
        }
        // The handle is deliberately never closed: the OS closes it when Nook exits, which is
        // what kills the children.
        Some(Job(handle))
    });

    pub fn assign(process: HANDLE) {
        if let Some(job) = JOB.as_ref() {
            if unsafe { AssignProcessToJobObject(job.0, process) } == 0 {
                tracing::debug!(
                    "could not assign a child to the job object: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
    }
}

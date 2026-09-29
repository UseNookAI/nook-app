//! Starting child processes the Windows way: no console window flashes up, and engines
//! (llama-server, whisper-server, sd, ffmpeg...) die with Nook even if it is killed, because each
//! one is put in a kill-on-close job object that only this process holds.
//!
//! macOS has no such job, so there a small reaper does it ([`start_reaper`]): Nook itself started
//! again with [`REAPER_ARG`], told each adopted child's process id down a pipe. When Nook ends,
//! however it ends, the kernel closes the pipe, and the reaper ends what is left of the children
//! (each leads its own process group, so the group goes with it).
//!
//! Use [`command`] for tokio children and [`std_command`] for blocking ones, then [`adopt`] (or
//! [`spawn_managed`]) any long-lived child so it cannot outlive the app. Short commands (git,
//! verify checks) need no job: they are awaited and killed on drop.

use std::ffi::OsStr;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The argument that starts Nook's executable as the reaper of its children (macOS).
pub const REAPER_ARG: &str = "--nook-reaper";

/// The name of a program on disk: `name.exe` on Windows, `name` elsewhere.
pub fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

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

/// Spawns and adopts in one step. Off Windows the child leads a process group of its own, so the
/// reaper can end what it starts too.
pub fn spawn_managed(cmd: &mut tokio::process::Command) -> std::io::Result<tokio::process::Child> {
    #[cfg(unix)]
    cmd.process_group(0);
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
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        reaper::watch(pid);
    }
}

/// Same as [`adopt`] for a blocking child.
pub fn adopt_std(child: &std::process::Child) {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        job::assign(child.as_raw_handle() as _);
    }
    #[cfg(unix)]
    reaper::watch(child.id());
}

/// Starts the reaper that ends the adopted children when Nook ends (macOS; nothing elsewhere).
/// The app calls it once at start; without it (tests) children are only killed on drop.
pub fn start_reaper() {
    #[cfg(unix)]
    reaper::start();
}

/// The reaper's own life, when Nook was started with [`REAPER_ARG`]: never returns.
#[cfg(unix)]
pub fn reaper_main() -> ! {
    reaper::serve()
}

#[cfg(unix)]
mod reaper {
    //! One pid per line on stdin. Each is watched with kqueue so a child that ends is forgotten
    //! (its pid may be given to another program); at end of input, Nook is gone: the rest are
    //! asked to stop (SIGTERM to the process group), then ended (SIGKILL) a moment later.

    use std::io::Write;
    use std::process::{ChildStdin, Stdio};

    use once_cell::sync::OnceCell;
    use parking_lot::Mutex;

    static PIPE: OnceCell<Option<Mutex<ChildStdin>>> = OnceCell::new();

    pub fn start() {
        PIPE.get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            let mut cmd = std::process::Command::new(exe);
            cmd.arg(super::REAPER_ARG)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // Its own group: a signal meant for Nook's does not end the reaper first.
            std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
            match cmd.spawn() {
                Ok(mut child) => child.stdin.take().map(Mutex::new),
                Err(e) => {
                    tracing::warn!("could not start the reaper of engine processes: {e}");
                    None
                }
            }
        });
    }

    pub fn watch(pid: u32) {
        if let Some(Some(pipe)) = PIPE.get() {
            if let Err(e) = writeln!(pipe.lock(), "{pid}") {
                tracing::debug!("could not tell the reaper about {pid}: {e}");
            }
        }
    }

    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    pub fn serve() -> ! {
        use std::collections::HashSet;
        use std::os::fd::AsRawFd;

        let stdin = std::io::stdin();
        let fd = stdin.as_raw_fd();
        let mut alive: HashSet<i32> = HashSet::new();
        let mut pending = String::new();
        // SAFETY: plain kqueue calls on descriptors this process owns; every struct is zeroed
        // before the fields kevent reads are set.
        unsafe {
            let kq = libc::kqueue();
            if kq < 0 {
                std::process::exit(1);
            }
            let mut change: libc::kevent = std::mem::zeroed();
            change.ident = fd as usize;
            change.filter = libc::EVFILT_READ;
            change.flags = libc::EV_ADD;
            libc::kevent(kq, &change, 1, std::ptr::null_mut(), 0, std::ptr::null());
            loop {
                let mut event: libc::kevent = std::mem::zeroed();
                let n = libc::kevent(kq, std::ptr::null(), 0, &mut event, 1, std::ptr::null());
                if n <= 0 {
                    continue;
                }
                if event.filter == libc::EVFILT_PROC {
                    alive.remove(&(event.ident as i32));
                    continue;
                }
                let mut buf = [0u8; 512];
                let read = libc::read(fd, buf.as_mut_ptr().cast(), buf.len());
                if read <= 0 {
                    break;
                }
                pending.push_str(&String::from_utf8_lossy(&buf[..read as usize]));
                while let Some(end) = pending.find('\n') {
                    let line: String = pending.drain(..=end).collect();
                    let Ok(pid) = line.trim().parse::<i32>() else {
                        continue;
                    };
                    let mut watch: libc::kevent = std::mem::zeroed();
                    watch.ident = pid as usize;
                    watch.filter = libc::EVFILT_PROC;
                    watch.flags = libc::EV_ADD | libc::EV_ONESHOT;
                    watch.fflags = libc::NOTE_EXIT;
                    // Fails when the child has already ended: nothing to watch then.
                    if libc::kevent(kq, &watch, 1, std::ptr::null_mut(), 0, std::ptr::null()) == 0 {
                        alive.insert(pid);
                    }
                }
            }
            for pid in &alive {
                libc::kill(-pid, libc::SIGTERM);
                libc::kill(*pid, libc::SIGTERM);
            }
            if !alive.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(1500));
            }
            for pid in &alive {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(*pid, libc::SIGKILL);
            }
        }
        std::process::exit(0)
    }

    #[cfg(not(any(target_os = "macos", target_os = "freebsd")))]
    pub fn serve() -> ! {
        // Linux has PR_SET_PDEATHSIG for this; Nook does not ship there.
        std::process::exit(0)
    }
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

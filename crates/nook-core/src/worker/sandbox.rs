//! Limits Windows itself puts on the commands a worker runs (its checks: tests, builds, scripts in
//! the repository). The allowlist says which commands may start, but what a command then does is
//! its own code: a test the worker wrote, a build script, a package's install step. So each runs as
//! a low-integrity process with its privileges dropped: Windows lets it write only where things
//! are labelled low, which Nook makes the scratch copy and the checks' own caches (temporary
//! files, package and build caches, under LocalLow), and nowhere else. Not the person's
//! repository, not their other files, not their registry settings, not the clipboard. The
//! command still reads what the person can read and still has the network (a build fetches its
//! packages), so what the worker is allowed to run stays the allowlist's to say.
//!
//! [`prepare`] labels a scratch copy once; [`spawn`] starts a command in it the same way on every
//! call, with the environment [`environment`] gives (caches moved to LocalLow); [`Sandboxed`] is
//! the running command: its output pipes, its exit, and its job (the command and everything it
//! started), terminated when it runs too long or the worker stops.
//!
//! `NOOK_UNSANDBOXED_CHECKS=1` in Nook's own environment runs checks as before, for a toolchain
//! that cannot work this way; nothing a repository contains can turn the sandbox off.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

/// Whether checks run sandboxed: always, unless Nook was started with `NOOK_UNSANDBOXED_CHECKS=1`.
pub fn enabled() -> bool {
    cfg!(windows) && std::env::var("NOOK_UNSANDBOXED_CHECKS").map_or(true, |v| v.trim() != "1")
}

/// Where the checks' caches live: `%USERPROFILE%\AppData\LocalLow\Nook\checks`, a folder low
/// processes may write to by Windows' own label.
pub fn caches() -> PathBuf {
    local_low().join("Nook").join("checks")
}

fn local_low() -> PathBuf {
    #[cfg(windows)]
    if let Some(p) = imp::known_local_low() {
        return p;
    }
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("AppData")
        .join("LocalLow")
}

/// What a check's environment changes, over `base`: temporary files and the package and build
/// caches that would otherwise be written in the person's profile go to the checks' caches.
/// Only what `base` does not set already.
pub fn environment(base: &BTreeMap<String, String>) -> Vec<(String, String)> {
    let root = caches();
    let at = |p: &str| root.join(p).display().to_string();
    let wanted = [
        ("TEMP", at("tmp")),
        ("TMP", at("tmp")),
        ("npm_config_cache", at("npm")),
        ("YARN_CACHE_FOLDER", at("yarn")),
        ("GRADLE_USER_HOME", at("gradle")),
        ("GOCACHE", at("go\\cache")),
        ("GOMODCACHE", at("go\\mod")),
        ("GOPATH", at("go\\path")),
        ("PIP_CACHE_DIR", at("pip")),
        ("XDG_CACHE_HOME", at("cache")),
        ("NUGET_PACKAGES", at("nuget")),
        ("DOTNET_CLI_HOME", at("dotnet")),
        ("CARGO_HOME", at("cargo")),
    ];
    let set = |k: &str| base.keys().any(|b| b.eq_ignore_ascii_case(k));
    wanted
        .into_iter()
        .filter(|(k, _)| !set(k))
        // TEMP and TMP always move: a low process cannot write the profile's own temp folder.
        .chain(
            [("TEMP", at("tmp")), ("TMP", at("tmp"))]
                .into_iter()
                .filter(|(k, _)| set(k)),
        )
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

/// Labels `dir` (and all it holds, and all made in it later) low, so a sandboxed check can write
/// there. Makes the checks' cache folders too. Cheap to call again.
pub fn prepare(dir: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        for sub in [
            "tmp", "npm", "yarn", "gradle", "go", "pip", "cache", "nuget", "dotnet", "cargo",
        ] {
            std::fs::create_dir_all(caches().join(sub))?;
        }
        imp::label_low(dir)?;
    }
    #[cfg(not(windows))]
    let _ = dir;
    Ok(())
}

#[cfg(windows)]
pub use imp::{spawn, Sandboxed};

#[cfg(windows)]
mod imp {
    use std::collections::BTreeMap;
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::{Path, PathBuf};

    use anyhow::{anyhow, bail, Result};
    use windows_sys::Win32::Foundation::{
        GetLastError, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Security::Authorization::{
        TreeSetNamedSecurityInfoW, SE_FILE_OBJECT, TREE_SEC_INFO_SET,
    };
    use windows_sys::Win32::Security::{
        AddMandatoryAce, CreateRestrictedToken, CreateWellKnownSid, GetLengthSid, InitializeAcl,
        SetTokenInformation, TokenIntegrityLevel, WinLowLabelSid, ACL, ACL_REVISION,
        CONTAINER_INHERIT_ACE, DISABLE_MAX_PRIVILEGE, LABEL_SECURITY_INFORMATION,
        OBJECT_INHERIT_ACE, SECURITY_ATTRIBUTES, SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES,
        TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL,
        TOKEN_QUERY,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicUIRestrictions,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_BASIC_UI_RESTRICTIONS,
        JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS,
        JOB_OBJECT_UILIMIT_EXITWINDOWS, JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES,
        JOB_OBJECT_UILIMIT_READCLIPBOARD, JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
        JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, OpenProcessToken,
        ResumeThread, WaitForSingleObject, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, INFINITE, PROCESS_INFORMATION,
        STARTF_USESTDHANDLES, STARTUPINFOW,
    };
    use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppDataLow, SHGetKnownFolderPath};

    // winnt.h's, which windows-sys keeps in a module of its own.
    const SE_GROUP_INTEGRITY: u32 = 0x20;
    const SYSTEM_MANDATORY_LABEL_NO_WRITE_UP: u32 = 0x1;

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn last_error(what: &str) -> anyhow::Error {
        anyhow!("{what}: {}", std::io::Error::last_os_error())
    }

    pub(super) fn known_local_low() -> Option<PathBuf> {
        let mut out: *mut u16 = std::ptr::null_mut();
        // SAFETY: the shell allocates the string, freed below with CoTaskMemFree's equal.
        let hr = unsafe {
            SHGetKnownFolderPath(&FOLDERID_LocalAppDataLow, 0, std::ptr::null_mut(), &mut out)
        };
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
        Some(PathBuf::from(path))
    }

    /// The low integrity label's SID, in a buffer of its own.
    fn low_sid() -> Result<Vec<u8>> {
        let mut sid = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut size = sid.len() as u32;
        // SAFETY: the buffer holds the largest SID there is.
        let ok = unsafe {
            CreateWellKnownSid(
                WinLowLabelSid,
                std::ptr::null_mut(),
                sid.as_mut_ptr() as *mut c_void,
                &mut size,
            )
        };
        if ok == 0 {
            return Err(last_error("Could not make the low label"));
        }
        sid.truncate(size as usize);
        Ok(sid)
    }

    /// Gives `dir` and everything below it a low label that new files and folders inherit.
    pub(super) fn label_low(dir: &Path) -> Result<()> {
        let mut sid = low_sid()?;
        let acl_size = std::mem::size_of::<ACL>() + 64 + sid.len();
        let mut acl = vec![0u8; acl_size];
        // SAFETY: plain Win32 calls on buffers this function owns, sized above.
        unsafe {
            if InitializeAcl(acl.as_mut_ptr() as *mut ACL, acl_size as u32, ACL_REVISION) == 0 {
                return Err(last_error("Could not make the label's list"));
            }
            // Write-up is refused: a low process writes here, nothing below low reaches up.
            if AddMandatoryAce(
                acl.as_mut_ptr() as *mut ACL,
                ACL_REVISION,
                OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
                SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
                sid.as_mut_ptr() as *mut c_void,
            ) == 0
            {
                return Err(last_error("Could not add the low label"));
            }
            let path = wide(dir.as_os_str());
            let err = TreeSetNamedSecurityInfoW(
                path.as_ptr() as *mut u16,
                SE_FILE_OBJECT,
                LABEL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
                acl.as_ptr() as *const ACL,
                TREE_SEC_INFO_SET,
                None,
                0,
                std::ptr::null_mut(),
            );
            if err != 0 {
                bail!(
                    "Could not label {} for sandboxed checks: {}",
                    dir.display(),
                    std::io::Error::from_raw_os_error(err as i32)
                );
            }
        }
        Ok(())
    }

    /// A token like Nook's own with every privilege but one dropped and a low integrity level.
    fn low_token() -> Result<OwnedHandle> {
        let mut own: HANDLE = std::ptr::null_mut();
        // SAFETY: plain Win32 calls; every handle opened here is owned by an OwnedHandle.
        unsafe {
            if OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
                &mut own,
            ) == 0
            {
                return Err(last_error("Could not read Nook's own token"));
            }
            let own = OwnedHandle::from_raw_handle(own as _);
            let mut restricted: HANDLE = std::ptr::null_mut();
            if CreateRestrictedToken(
                own.as_raw_handle() as HANDLE,
                DISABLE_MAX_PRIVILEGE,
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                &mut restricted,
            ) == 0
            {
                return Err(last_error("Could not make a restricted token"));
            }
            let restricted = OwnedHandle::from_raw_handle(restricted as _);
            let mut sid = low_sid()?;
            let label = TOKEN_MANDATORY_LABEL {
                Label: SID_AND_ATTRIBUTES {
                    Sid: sid.as_mut_ptr() as *mut c_void,
                    Attributes: SE_GROUP_INTEGRITY,
                },
            };
            let size = std::mem::size_of::<TOKEN_MANDATORY_LABEL>() as u32
                + GetLengthSid(sid.as_mut_ptr() as *mut c_void);
            if SetTokenInformation(
                restricted.as_raw_handle() as HANDLE,
                TokenIntegrityLevel,
                &label as *const _ as *const c_void,
                size,
            ) == 0
            {
                return Err(last_error("Could not lower the token's integrity"));
            }
            Ok(restricted)
        }
    }

    /// A pipe whose write end the child inherits: (read end for Nook, write end for the child).
    fn pipe() -> Result<(OwnedHandle, OwnedHandle)> {
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let (mut read, mut write): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: plain Win32 calls; the handles are owned right away.
        unsafe {
            if CreatePipe(&mut read, &mut write, &sa, 0) == 0 {
                return Err(last_error("Could not make a pipe"));
            }
            let (read, write) = (
                OwnedHandle::from_raw_handle(read as _),
                OwnedHandle::from_raw_handle(write as _),
            );
            // Nook's end is not inherited.
            SetHandleInformation(read.as_raw_handle() as HANDLE, HANDLE_FLAG_INHERIT, 0);
            Ok((read, write))
        }
    }

    fn null_input() -> Result<OwnedHandle> {
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let name = wide(OsStr::new("NUL"));
        // SAFETY: opens the null device; the handle is owned right away.
        let h = unsafe {
            CreateFileW(
                name.as_ptr(),
                FILE_GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &sa,
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return Err(last_error("Could not open NUL"));
        }
        // SAFETY: a valid handle just opened.
        Ok(unsafe { OwnedHandle::from_raw_handle(h as _) })
    }

    /// One argument as Windows' C runtime reads it back.
    pub(crate) fn quote(arg: &str) -> String {
        if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
            return arg.to_string();
        }
        let mut out = String::from("\"");
        let mut slashes = 0usize;
        for c in arg.chars() {
            match c {
                '\\' => slashes += 1,
                '"' => {
                    out.push_str(&"\\".repeat(slashes * 2 + 1));
                    out.push('"');
                    slashes = 0;
                }
                _ => {
                    out.push_str(&"\\".repeat(slashes));
                    slashes = 0;
                    out.push(c);
                }
            }
        }
        out.push_str(&"\\".repeat(slashes * 2));
        out.push('"');
        out
    }

    /// `program` found the way a command line finds it, in the child's PATH (and with its
    /// extensions), never in the folder the command runs in: a `cargo.exe` the worker wrote into
    /// the scratch copy must not be what "cargo" starts.
    pub(crate) fn find(program: &str, env: &BTreeMap<String, String>) -> Result<PathBuf> {
        let p = Path::new(program);
        if p.is_absolute() {
            return Ok(p.to_path_buf());
        }
        if program.contains(['/', '\\']) {
            bail!("A command's program is named by itself or by its whole path: {program}");
        }
        let get = |k: &str| {
            env.iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(k))
                .map(|(_, v)| v.clone())
        };
        let exts: Vec<String> = if p.extension().is_some() {
            vec![String::new()]
        } else {
            get("PATHEXT")
                .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into())
                .split(';')
                .filter(|e| !e.is_empty())
                .map(|e| e.to_ascii_lowercase())
                .collect()
        };
        for dir in std::env::split_paths(&get("PATH").unwrap_or_default()) {
            if dir.as_os_str().is_empty() || !dir.is_absolute() {
                continue;
            }
            for e in &exts {
                let candidate = dir.join(format!("{program}{e}"));
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
        bail!("Cannot find the program \"{program}\"")
    }

    /// The command line and the program to start: a batch file runs through cmd.exe, with its
    /// arguments kept to what cmd.exe cannot read as anything but text.
    pub(crate) fn command_line(program: &Path, args: &[String]) -> Result<(PathBuf, String)> {
        let ext = program
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if ext == "cmd" || ext == "bat" {
            if let Some(bad) = args.iter().find(|a| {
                a.contains(['%', '!', '^', '"', '&', '|', '<', '>', '(', ')', '\n', '\r'])
            }) {
                bail!("That argument cannot be given to a batch file safely: {bad}");
            }
            let system = std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            let cmd = system.join("System32").join("cmd.exe");
            let mut inner = quote(&program.display().to_string());
            for a in args {
                inner.push(' ');
                inner.push_str(&quote(a));
            }
            return Ok((cmd, format!("cmd.exe /d /e:ON /v:OFF /s /c \"{inner}\"")));
        }
        let mut line = quote(&program.display().to_string());
        for a in args {
            line.push(' ');
            line.push_str(&quote(a));
        }
        Ok((program.to_path_buf(), line))
    }

    fn environment_block(env: &BTreeMap<String, String>) -> Vec<u16> {
        // Sorted without regard to case, as Windows keeps it.
        let mut pairs: Vec<(&String, &String)> = env.iter().collect();
        pairs.sort_by_key(|(k, _)| k.to_uppercase());
        let mut block: Vec<u16> = Vec::new();
        for (k, v) in pairs {
            block.extend(OsStr::new(&format!("{k}={v}")).encode_wide());
            block.push(0);
        }
        block.push(0);
        block
    }

    /// A command running sandboxed: its output, its exit, and its job.
    pub struct Sandboxed {
        process: OwnedHandle,
        job: OwnedHandle,
        pub stdout: Option<std::fs::File>,
        pub stderr: Option<std::fs::File>,
        armed: bool,
    }

    // The handles are only waited on, read, terminated and closed, which any thread may do.
    unsafe impl Send for Sandboxed {}
    unsafe impl Sync for Sandboxed {}

    impl Sandboxed {
        /// Waits for the command to end; its exit code.
        pub async fn wait(&self) -> Result<i32> {
            let process = self.process.try_clone()?;
            tokio::task::spawn_blocking(move || {
                let h = process.as_raw_handle() as HANDLE;
                // SAFETY: the handle is open until `process` drops, after this.
                unsafe {
                    if WaitForSingleObject(h, INFINITE) != WAIT_OBJECT_0 {
                        return Err(last_error("Could not wait for the command"));
                    }
                    let mut code = 0u32;
                    if GetExitCodeProcess(h, &mut code) == 0 {
                        return Err(last_error("Could not read the command's exit code"));
                    }
                    Ok(code as i32)
                }
            })
            .await
            .map_err(|e| anyhow!("{e}"))?
        }

        /// Stops the command and everything it started.
        pub fn kill(&mut self) {
            self.armed = false;
            // SAFETY: the job handle is open until drop.
            unsafe { TerminateJobObject(self.job.as_raw_handle() as HANDLE, 1) };
        }

        /// The command ended by itself: what it left running on purpose (a Gradle daemon) stays.
        pub fn disarm(&mut self) {
            self.armed = false;
        }
    }

    impl Drop for Sandboxed {
        fn drop(&mut self) {
            if self.armed {
                self.kill();
            }
        }
    }

    /// Starts `program` with `args` in `cwd` as a low-integrity process with its privileges
    /// dropped, in a job of its own (no clipboard, no desktop switching, no system settings),
    /// with `env` as its whole environment. `cwd` must have been [`prepare`](super::prepare)d.
    pub fn spawn(
        program: &str,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<Sandboxed> {
        let exe = find(program, env).map_err(|e| {
            anyhow!(
                "Cannot run program \"{program}\" (in directory \"{}\"): {e}",
                cwd.display()
            )
        })?;
        let (application, line) = command_line(&exe, args)?;
        let token = low_token()?;
        let (out_read, out_write) = pipe()?;
        let (err_read, err_write) = pipe()?;
        let input = null_input()?;
        let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        si.dwFlags = STARTF_USESTDHANDLES;
        si.hStdInput = input.as_raw_handle() as HANDLE;
        si.hStdOutput = out_write.as_raw_handle() as HANDLE;
        si.hStdError = err_write.as_raw_handle() as HANDLE;
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let application = wide(application.as_os_str());
        let mut line = wide(OsStr::new(&line));
        let cwd_w = wide(cwd.as_os_str());
        let mut block = environment_block(env);
        // SAFETY: every pointer is to a buffer that lives past the call; the handles returned
        // are owned right away.
        let started = unsafe {
            CreateProcessAsUserW(
                token.as_raw_handle() as HANDLE,
                application.as_ptr(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                CREATE_SUSPENDED
                    | CREATE_NO_WINDOW
                    | CREATE_UNICODE_ENVIRONMENT
                    | CREATE_NEW_PROCESS_GROUP,
                block.as_mut_ptr() as *const c_void,
                cwd_w.as_ptr(),
                &si,
                &mut pi,
            )
        };
        drop((out_write, err_write, input));
        if started == 0 {
            // SAFETY: reads the thread's last error only.
            let code = unsafe { GetLastError() };
            return Err(anyhow!(
                "Cannot run program \"{}\" (in directory \"{}\"): {}",
                exe.display(),
                cwd.display(),
                std::io::Error::from_raw_os_error(code as i32)
            ));
        }
        // SAFETY: handles from a started process, owned from here on.
        let (process, thread) = unsafe {
            (
                OwnedHandle::from_raw_handle(pi.hProcess as _),
                OwnedHandle::from_raw_handle(pi.hThread as _),
            )
        };
        // SAFETY: plain Win32 calls on handles owned above.
        let job = unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                windows_sys::Win32::System::Threading::TerminateProcess(
                    process.as_raw_handle() as HANDLE,
                    1,
                );
                return Err(last_error("Could not make the command's job"));
            }
            let job = OwnedHandle::from_raw_handle(job as _);
            let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
                UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
                    | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                    | JOB_OBJECT_UILIMIT_EXITWINDOWS
                    | JOB_OBJECT_UILIMIT_GLOBALATOMS
                    | JOB_OBJECT_UILIMIT_HANDLES
                    | JOB_OBJECT_UILIMIT_READCLIPBOARD
                    | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                    | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
            };
            SetInformationJobObject(
                job.as_raw_handle() as HANDLE,
                JobObjectBasicUIRestrictions,
                &ui as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
            );
            if AssignProcessToJobObject(
                job.as_raw_handle() as HANDLE,
                process.as_raw_handle() as HANDLE,
            ) == 0
            {
                windows_sys::Win32::System::Threading::TerminateProcess(
                    process.as_raw_handle() as HANDLE,
                    1,
                );
                return Err(last_error("Could not put the command in its job"));
            }
            ResumeThread(thread.as_raw_handle() as HANDLE);
            job
        };
        drop(thread);
        Ok(Sandboxed {
            process,
            job,
            stdout: Some(std::fs::File::from(out_read)),
            stderr: Some(std::fs::File::from(err_read)),
            armed: true,
        })
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::io::Read;

    fn run(program: &str, args: &[&str], cwd: &Path) -> (i32, String) {
        let mut env: BTreeMap<String, String> = std::env::vars().collect();
        env.extend(environment(&env));
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut child = spawn(program, &args, cwd, &env).unwrap();
            let mut out = String::new();
            if let Some(mut o) = child.stdout.take() {
                let _ = o.read_to_string(&mut out);
            }
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut out);
            }
            let code = child.wait().await.unwrap();
            child.disarm();
            (code, out)
        })
    }

    #[test]
    fn a_check_writes_in_its_scratch_copy_and_nowhere_else() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = tmp.path().join("scratch");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(scratch.join("before.txt"), "made before the label").unwrap();
        prepare(&scratch).unwrap();

        let (code, out) = run("cmd", &["/c", "echo inside> inside.txt"], &scratch);
        assert_eq!(code, 0, "{out}");
        assert!(scratch.join("inside.txt").is_file());
        // A file that was there before the label can be changed too.
        let (code, out) = run("cmd", &["/c", "echo changed> before.txt"], &scratch);
        assert_eq!(code, 0, "{out}");

        // (cmd.exe reads its command line raw: no quotes inside, so the paths have no spaces.)
        let target = outside.join("escaped.txt");
        assert!(!target.display().to_string().contains(' '));
        let line = format!("echo escaped> {}", target.display());
        let (code, out) = run("cmd", &["/c", &line], &scratch);
        assert_ne!(code, 0, "writing outside the scratch copy must fail: {out}");
        assert!(out.contains("Access is denied"), "{out}");
        assert!(!target.exists());
        // The same command writes there when it is not sandboxed: the refusal is the sandbox's.
        let plain = std::process::Command::new("cmd")
            .args(["/c", &line])
            .current_dir(&scratch)
            .status()
            .unwrap();
        assert!(plain.success() && target.exists());

        // Nor the person's registry settings.
        let (code, out) = run(
            "reg",
            &[
                "add",
                r"HKCU\Software\NookSandboxTest",
                "/v",
                "x",
                "/d",
                "1",
                "/f",
            ],
            &scratch,
        );
        assert_ne!(code, 0, "writing HKCU must fail: {out}");

        // The checks' temporary folder is theirs to write.
        let (code, out) = run("cmd", &["/c", r"echo t> %TEMP%\nook-check.txt"], &scratch);
        assert_eq!(code, 0, "{out}");
    }

    /// The toolchains a worker's checks use, sandboxed as a check runs them, in a project of
    /// their own: a Rust crate with no dependencies, an npm test script, a Python script. The
    /// person's own CARGO_HOME is left out, as it is on a computer that never set one.
    /// `cargo test -p nook-core builds_with_the_real_toolchains -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs cargo, node and python on PATH"]
    fn builds_with_the_real_toolchains() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("project");
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::write(
            p.join("Cargo.toml"),
            "[package]\nname = \"sandboxed\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
        )
        .unwrap();
        std::fs::write(
            p.join("src").join("lib.rs"),
            "pub fn two() -> u8 { 2 }\n#[test]\nfn t() { assert_eq!(two(), 2); }\n",
        )
        .unwrap();
        std::fs::write(
            p.join("package.json"),
            r#"{"name":"sandboxed","version":"1.0.0","scripts":{"test":"node -e \"require('fs').writeFileSync('node-ran.txt','ok')\""}}"#,
        )
        .unwrap();
        std::fs::write(p.join("check.py"), "open('py-ran.txt', 'w').write('ok')\n").unwrap();
        prepare(&p).unwrap();

        let mut env: BTreeMap<String, String> = std::env::vars().collect();
        env.retain(|k, _| {
            !k.eq_ignore_ascii_case("CARGO_HOME") && !k.eq_ignore_ascii_case("CARGO_TARGET_DIR")
        });
        env.extend(environment(&env));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let go = |program: &str, args: &[&str]| {
            let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            rt.block_on(async {
                let mut child = spawn(program, &args, &p, &env).unwrap();
                let (mut o, mut e) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
                let reader = std::thread::spawn(move || {
                    let mut s = String::new();
                    let _ = e.read_to_string(&mut s);
                    s
                });
                let mut out = String::new();
                let _ = o.read_to_string(&mut out);
                out.push_str(&reader.join().unwrap());
                let code = child.wait().await.unwrap();
                child.disarm();
                (code, out)
            })
        };
        let (code, out) = go("cargo", &["test", "--offline"]);
        println!("cargo test: exit {code}\n{out}");
        assert_eq!(code, 0);
        let (code, out) = go("npm", &["test"]);
        println!("npm test: exit {code}\n{out}");
        assert_eq!(code, 0);
        assert!(p.join("node-ran.txt").is_file());
        let (code, out) = go("python", &["check.py"]);
        println!("python: exit {code}\n{out}");
        assert_eq!(code, 0);
        assert!(p.join("py-ran.txt").is_file());
    }

    #[test]
    fn a_program_in_the_scratch_copy_is_not_what_a_name_starts() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("cmd.exe"), b"not a program").unwrap();
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let found = imp::find("cmd", &env).unwrap();
        assert!(!found.starts_with(tmp.path()), "{found:?}");
        assert!(imp::find("sub\\tool.exe", &env).is_err());
    }

    #[test]
    fn arguments_reach_the_program_as_they_were() {
        assert_eq!(imp::quote("plain"), "plain");
        assert_eq!(imp::quote("two words"), "\"two words\"");
        assert_eq!(imp::quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(
            imp::quote(r"C:\dir with space\"),
            r#""C:\dir with space\\""#
        );
        let (app, line) = imp::command_line(Path::new(r"C:\n\npm.cmd"), &["test".into()]).unwrap();
        assert!(app.ends_with("cmd.exe"));
        assert_eq!(line, r#"cmd.exe /d /e:ON /v:OFF /s /c "C:\n\npm.cmd test""#);
        assert!(imp::command_line(Path::new(r"C:\n\npm.cmd"), &["%PATH%".into()]).is_err());
    }
}

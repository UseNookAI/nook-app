//! The commands a local worker may run inside a scratch copy of a repository. From `nook.json` at
//! the repository root when present (`{"verify": ["gradlew :agent:compileJava", ...]}`, each entry
//! a prefix the worker may complete with arguments), else a built-in set per ecosystem detected
//! from the files present. Nothing outside the list runs, ever.
//!
//! Ports `worker/VerifyCommands.java`. A command runs without a shell, its output (stdout and
//! stderr together) kept to the tail. When it runs past its time, or the worker is stopped while
//! it runs, its whole process tree is stopped: on Windows the command is put in a job object of
//! its own and the job is terminated (a build's grandchildren, a Gradle daemon started by it, go
//! with it, even when their parent has already exited); where a job cannot be made,
//! `taskkill /T /F` stops the tree Windows still links to it. Elsewhere the command leads its own
//! process group, which is killed.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Seconds a verification may take before it is stopped.
pub const TIMEOUT_SECONDS: u64 = 600;
const OUTPUT_TAIL: usize = 6000;
/// Bytes of output kept while a command runs: the tail shown is a small part of this. (The
/// original kept everything and cut afterwards; a chatty ten-minute build need not fill memory.)
const OUTPUT_KEEP: usize = 256 * 1024;

pub struct VerifyCommands {
    allowed: Vec<Regex>,
    examples: Vec<String>,
    source: String,
}

/// How a command ended: its exit code and the tail of what it printed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResult {
    pub exit_code: i32,
    pub output: String,
    pub seconds: u64,
}

impl VerifyResult {
    pub fn passed(&self) -> bool {
        self.exit_code == 0
    }

    pub fn summary(&self) -> String {
        format!("exit {} in {} s", self.exit_code, self.seconds)
    }
}

static DOUBLE_QUOTED: Lazy<Regex> = Lazy::new(|| pattern(r#""([A-Za-z0-9_.:/\\=-]+)""#));
static SINGLE_QUOTED: Lazy<Regex> = Lazy::new(|| pattern(r"'([A-Za-z0-9_.:/\\=-]+)'"));

fn pattern(p: &str) -> Regex {
    Regex::new(p).expect("a valid pattern")
}

/// A whole-command pattern (Java's `matches()`).
fn whole(p: &str) -> Option<Regex> {
    Regex::new(&format!("^(?:{p})$")).ok()
}

impl VerifyCommands {
    /// Where the list came from: "nook.json" or the ecosystems detected.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Human-readable forms of what is allowed, for the worker's tool description and error messages.
    pub fn examples(&self) -> &[String] {
        &self.examples
    }

    /// The first example, if any.
    pub fn first_example(&self) -> Option<&str> {
        self.examples.first().map(String::as_str)
    }

    /// The command as the allowlist and the runner read it: a leading `./` or `.\` dropped, and
    /// quotes removed around an argument made only of letters, digits and `_ . : / \ = -`, where
    /// they change nothing. M4 through /nook on 2026-09-23 asked for
    /// `./gradlew :agent:test --tests "ai.nook.agent.mcp.McpJsonTest"`, was refused, and asked
    /// again without them: a request spent on a spelling.
    pub fn canonical(command: &str) -> String {
        let mut s = command.trim();
        if s.starts_with("./") || s.starts_with(".\\") {
            s = &s[2..];
        }
        let s = DOUBLE_QUOTED.replace_all(s, "$1");
        SINGLE_QUOTED.replace_all(&s, "$1").into_owned()
    }

    pub fn permits(&self, command: &str) -> bool {
        let c = command.trim();
        !c.is_empty() && self.allowed.iter().any(|p| p.is_match(c))
    }

    /// A one-line description of the allowlist for a model, e.g. for the tool's description.
    pub fn describe(&self) -> String {
        format!(
            "Allowed commands ({}): {}",
            self.source,
            self.examples.join(" | ")
        )
    }

    /// Reads `nook.json` or detects the ecosystems of the repository at `root`.
    pub fn for_repository(root: &Path) -> VerifyCommands {
        if let Some(v) = from_nook_json(root) {
            return v;
        }
        let mut allowed = Vec::new();
        let mut examples: Vec<String> = Vec::new();
        let mut eco = Vec::new();
        let has = |name: &str| root.join(name).exists();
        if has("gradlew") || has("gradlew.bat") || has("build.gradle") || has("build.gradle.kts") {
            eco.push("gradle");
            // A task of the whole build ("gradlew test") and a task of one module
            // ("gradlew :agent:test") are both ordinary things to ask for.
            let task = "(compileJava|compileKotlin|compileTestJava|compileTestKotlin|classes|testClasses|build|check|test|assemble)";
            let tail = r"( --tests [A-Za-z0-9_.*$]+)?( -x [A-Za-z0-9_:-]+)*";
            allowed.push(format!(
                r"gradlew(\.bat)?( (:[A-Za-z0-9_-]+)*:?{task})+{tail}"
            ));
            examples.extend(
                [
                    "gradlew test",
                    "gradlew :module:compileJava",
                    "gradlew :module:test --tests <pattern>",
                ]
                .map(String::from),
            );
        }
        if has("package.json") {
            eco.push("npm");
            allowed.push("(npm|pnpm|yarn) (test|run (test|build|lint|typecheck|check))( -- [A-Za-z0-9_./=-]+)*".to_string());
            allowed.push("npx (tsc|jest|vitest|eslint)( [A-Za-z0-9_./=-]+)*".to_string());
            examples.extend(["npm test", "npm run build"].map(String::from));
        }
        if has("Cargo.toml") {
            eco.push("cargo");
            allowed.push("cargo (build|check|test|clippy)( [A-Za-z0-9_./=-]+)*".to_string());
            examples.push("cargo test".to_string());
        }
        if has("pyproject.toml") || has("setup.py") || has("pytest.ini") || has("requirements.txt")
        {
            eco.push("python");
            allowed.push("(pytest|python -m pytest)( [A-Za-z0-9_./:=-]+)*".to_string());
            examples.push("pytest tests/".to_string());
        }
        if has("go.mod") {
            eco.push("go");
            allowed.push("go (build|test|vet)( [A-Za-z0-9_./=-]+)*".to_string());
            examples.push("go test ./...".to_string());
        }
        // scripts in the repository itself, run with their own interpreter
        allowed.push(r"python [A-Za-z0-9_./\\-]+\.py( [A-Za-z0-9_./\\=-]+)*".to_string());
        examples.push("python <script in the repository> [args]".to_string());
        let source = if eco.is_empty() {
            "built-in (no ecosystem detected)".to_string()
        } else {
            format!("built-in for {}", eco.join(", "))
        };
        VerifyCommands {
            allowed: allowed.iter().filter_map(|p| whole(p)).collect(),
            examples,
            source,
        }
    }

    /// Runs an allowed command in `cwd`; the result carries the exit code and the tail of the output.
    pub async fn run(
        &self,
        command: &str,
        cwd: &Path,
        env: &HashMap<String, String>,
    ) -> Result<VerifyResult> {
        self.run_within(command, cwd, env, Duration::from_secs(TIMEOUT_SECONDS))
            .await
    }

    pub(crate) async fn run_within(
        &self,
        command: &str,
        cwd: &Path,
        env: &HashMap<String, String>,
        limit: Duration,
    ) -> Result<VerifyResult> {
        if !self.permits(command) {
            bail!("command not allowed: {command}");
        }
        let mut parts: Vec<String> = command
            .trim()
            // Java's \s
            .split([' ', '\t', '\n', '\u{0B}', '\u{0C}', '\r'])
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        if parts[0].starts_with("gradlew") {
            let wrapper = cwd.join(if cfg!(windows) {
                "gradlew.bat"
            } else {
                "gradlew"
            });
            parts[0] = wrapper.to_string_lossy().into_owned();
            parts.push("--console=plain".to_string());
            parts.push("-q".to_string());
            // A daemon stays three hours by default, about a gigabyte each; turns that verify with
            // Gradle in scratch copies must not leave a pile of them behind. Ten idle minutes still
            // spans a worker's runs.
            parts.push("-Dorg.gradle.daemon.idletimeout=600000".to_string());
        } else {
            parts[0] = program(&parts[0]);
        }
        let mut cmd = crate::process::command(&parts[0]);
        cmd.args(&parts[1..])
            .current_dir(cwd)
            .envs(env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);
        let started = Instant::now();
        let mut child = cmd.spawn().map_err(|e| {
            anyhow!(
                "Cannot run program \"{}\" (in directory \"{}\"): {e}",
                parts[0],
                cwd.display()
            )
        })?;
        let mut tree = tree::Tree::adopt(&child);
        let output = Arc::new(Mutex::new(Tail::default()));
        let mut readers = Vec::new();
        if let Some(out) = child.stdout.take() {
            readers.push(tokio::spawn(collect(out, output.clone())));
        }
        if let Some(err) = child.stderr.take() {
            readers.push(tokio::spawn(collect(err, output.clone())));
        }
        let status = match tokio::time::timeout(limit, child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                tree.kill();
                let _ = child.kill().await;
                readers.iter().for_each(|r| r.abort());
                return Ok(VerifyResult {
                    exit_code: -1,
                    output: format!(
                        "the command ran for more than {} seconds and was stopped",
                        limit.as_secs()
                    ),
                    seconds: started.elapsed().as_secs(),
                });
            }
        };
        tree.disarm();
        // What is still in the pipes; a grandchild that holds them open gets five seconds.
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            for r in readers.iter_mut() {
                let _ = r.await;
            }
        })
        .await;
        readers.iter().for_each(|r| r.abort());
        let text = output.lock().text();
        Ok(VerifyResult {
            exit_code: status.code().unwrap_or(-1),
            output: text,
            seconds: started.elapsed().as_secs(),
        })
    }
}

fn from_nook_json(root: &Path) -> Option<VerifyCommands> {
    let cfg = root.join("nook.json");
    if !cfg.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&cfg).ok()?;
    let n: serde_json::Value = serde_json::from_str(crate::settings::strip_bom_str(&text)).ok()?;
    let entries: Vec<&serde_json::Value> = match n.get("verify") {
        Some(serde_json::Value::Array(a)) => a.iter().collect(),
        Some(serde_json::Value::Object(o)) => o.values().collect(),
        _ => Vec::new(),
    };
    let mut allowed = Vec::new();
    let mut examples = Vec::new();
    for v in entries {
        let text = match v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            _ => String::new(),
        };
        let entry = text.trim();
        if entry.is_empty() {
            continue;
        }
        // an entry allows itself and any arguments after it
        if let Some(p) = whole(&format!("{}( [^&|;<>`$]*)?", regex::escape(entry))) {
            allowed.push(p);
            examples.push(entry.to_string());
        }
    }
    (!allowed.is_empty()).then(|| VerifyCommands {
        allowed,
        examples,
        source: "nook.json".to_string(),
    })
}

/// The program to start for a command's first word. On Windows, `npm`, `pnpm`, `yarn` and `npx`
/// are .cmd scripts, which starting a program by its bare name does not find (Java's
/// ProcessBuilder did not either, so the original's `npm test` could not start): a name without
/// an extension that has no .exe on PATH is looked up as .cmd, then .bat. Anything else is left
/// for the system to find as it always did.
fn program(name: &str) -> String {
    #[cfg(windows)]
    {
        if Path::new(name).extension().is_none() && !name.contains(['/', '\\']) {
            if let Some(path) = std::env::var_os("PATH") {
                let dirs: Vec<std::path::PathBuf> = std::env::split_paths(&path).collect();
                if !dirs.iter().any(|d| d.join(format!("{name}.exe")).is_file()) {
                    for ext in ["cmd", "bat"] {
                        if let Some(script) = dirs
                            .iter()
                            .map(|d| d.join(format!("{name}.{ext}")))
                            .find(|f| f.is_file())
                        {
                            return script.to_string_lossy().into_owned();
                        }
                    }
                }
            }
        }
    }
    name.to_string()
}

/// The end of a command's output, stdout and stderr in the order they arrived.
#[derive(Default)]
struct Tail {
    bytes: Vec<u8>,
}

impl Tail {
    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > 2 * OUTPUT_KEEP {
            let drop = self.bytes.len() - OUTPUT_KEEP;
            self.bytes.drain(..drop);
        }
    }

    /// The last [`OUTPUT_TAIL`] characters, marked with … when there was more.
    fn text(&self) -> String {
        let text = String::from_utf8_lossy(&self.bytes);
        let n = text.chars().count();
        let text = if n > OUTPUT_TAIL {
            format!(
                "…{}",
                text.chars().skip(n - OUTPUT_TAIL).collect::<String>()
            )
        } else {
            text.into_owned()
        };
        text.trim().to_string()
    }
}

async fn collect(mut from: impl AsyncRead + Unpin, into: Arc<Mutex<Tail>>) {
    let mut buf = vec![0u8; 16 * 1024];
    while let Ok(n) = from.read(&mut buf).await {
        if n == 0 {
            break;
        }
        into.lock().push(&buf[..n]);
    }
}

/// A command's whole process tree, stopped on [`Tree::kill`] or when dropped still armed (the
/// worker was stopped while the command ran). Disarmed once the command has exited, so what it
/// left running on purpose (a Gradle daemon) stays.
#[cfg(windows)]
mod tree {
    use std::process::Stdio;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
    };

    pub(super) struct Tree {
        job: HANDLE,
        pid: Option<u32>,
        armed: bool,
    }

    // The job handle is only terminated and closed, which any thread may do.
    unsafe impl Send for Tree {}
    unsafe impl Sync for Tree {}

    impl Tree {
        pub(super) fn adopt(child: &tokio::process::Child) -> Tree {
            let pid = child.id();
            // SAFETY: plain Win32 calls on handles this function owns or borrows from a live child.
            let mut job: HANDLE = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if !job.is_null() {
                let assigned = child
                    .raw_handle()
                    .is_some_and(|h| unsafe { AssignProcessToJobObject(job, h as HANDLE) } != 0);
                if !assigned {
                    tracing::debug!(
                        "could not put a verification in a job: {}",
                        std::io::Error::last_os_error()
                    );
                    unsafe { CloseHandle(job) };
                    job = std::ptr::null_mut();
                }
            }
            Tree {
                job,
                pid,
                armed: true,
            }
        }

        pub(super) fn disarm(&mut self) {
            self.armed = false;
        }

        pub(super) fn kill(&mut self) {
            self.armed = false;
            if !self.job.is_null() {
                // SAFETY: the job handle is open until drop.
                unsafe { TerminateJobObject(self.job, 1) };
            } else if let Some(pid) = self.pid {
                let _ = crate::process::std_command("taskkill")
                    .args(["/T", "/F", "/PID", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            if self.armed {
                self.kill();
            }
            if !self.job.is_null() {
                // SAFETY: closed once, here. Processes still in the job keep running: the job was
                // not made to kill on close.
                unsafe { CloseHandle(self.job) };
            }
        }
    }
}

#[cfg(not(windows))]
mod tree {
    pub(super) struct Tree {
        pid: Option<u32>,
        armed: bool,
    }

    impl Tree {
        pub(super) fn adopt(child: &tokio::process::Child) -> Tree {
            Tree {
                pid: child.id(),
                armed: true,
            }
        }

        pub(super) fn disarm(&mut self) {
            self.armed = false;
        }

        /// The command leads its own process group (see `process_group(0)`): kill all of it.
        pub(super) fn kill(&mut self) {
            self.armed = false;
            if let Some(pid) = self.pid {
                let _ = crate::process::std_command("kill")
                    .args(["-KILL", "--", &format!("-{pid}")])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            if self.armed {
                self.kill();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            std::fs::write(dir.path().join(name), content).unwrap();
        }
        dir
    }

    #[test]
    fn the_gradle_allowlist_takes_a_task_of_the_whole_build() {
        // "gradlew test" is what a person asks for; before 2026-09-22 only ":module:task" matched
        // and the standard verification was refused (QA finding).
        let dir = repo(&[("gradlew", ""), ("build.gradle", "")]);
        let allow = VerifyCommands::for_repository(dir.path());

        assert!(allow.permits("gradlew test"), "{}", allow.describe());
        assert!(allow.permits("gradlew build"));
        assert!(allow.permits("gradlew test --tests FooTest"));
        assert!(allow.permits("gradlew check -x test"));
        assert!(
            allow.permits("gradlew :agent:test"),
            "a module task still works"
        );
        assert!(allow.permits("gradlew test check"), "two tasks at once");
        assert!(
            !allow.permits("gradlew publish"),
            "a task outside the list is still refused"
        );
        assert!(
            !allow.permits("gradlew test; rm -rf /"),
            "and so is anything appended to one"
        );
    }

    #[test]
    fn a_verify_command_is_read_in_its_plain_spelling() {
        let dir = repo(&[(
            "nook.json",
            r#"{"verify": ["gradlew :agent:test --tests"]}"#,
        )]);
        let allow = VerifyCommands::for_repository(dir.path());
        let asked = VerifyCommands::canonical(
            "./gradlew :agent:test --tests \"ai.nook.agent.mcp.McpJsonTest\"",
        );
        assert_eq!(
            "gradlew :agent:test --tests ai.nook.agent.mcp.McpJsonTest",
            asked
        );
        assert!(allow.permits(&asked));
        assert_eq!(
            "gradlew test --tests \"a b\"",
            VerifyCommands::canonical("gradlew test --tests \"a b\""),
            "a quoted space stays quoted"
        );
        assert_eq!(
            "gradlew test --tests \"*Test\"",
            VerifyCommands::canonical("gradlew test --tests \"*Test\""),
            "and so does a pattern"
        );
        assert_eq!(
            "gradlew test \"x;y\"",
            VerifyCommands::canonical("gradlew test \"x;y\""),
            "a shell character keeps its quotes"
        );
        assert_eq!(
            "python verify.py",
            VerifyCommands::canonical(".\\python 'verify.py'")
        );
    }

    #[test]
    fn ecosystems_are_detected_from_the_files_present() {
        let dir = repo(&[("build.gradle", ""), ("package.json", "{}")]);
        let v = VerifyCommands::for_repository(dir.path());
        assert!(
            v.source().contains("gradle") && v.source().contains("npm"),
            "{}",
            v.source()
        );
        assert!(v.permits("gradlew :agent:compileJava"));
        assert!(v.permits("gradlew :agent:test --tests ai.nook.agent.mcp.GitIgnoreTest"));
        assert!(v.permits("npm test"));
        assert!(v.permits("npm run build"));
        assert!(v.permits("python tools/check.py --json out.json"));
        assert!(!v.permits("gradlew clean"));
        assert!(!v.permits("npm install evil"));
        assert!(!v.permits("python -c print(1)"));
        assert!(!v.permits("gradlew :agent:test; rm -rf ."));
    }

    #[test]
    fn nook_json_decides_and_takes_arguments_but_no_shell() {
        let dir = repo(&[
            (
                "nook.json",
                r#"{"verify": ["python verify.py", " ", "cargo test -p core"]}"#,
            ),
            ("Cargo.toml", ""),
        ]);
        let v = VerifyCommands::for_repository(dir.path());
        assert_eq!("nook.json", v.source());
        assert_eq!(["python verify.py", "cargo test -p core"], v.examples());
        assert_eq!(Some("python verify.py"), v.first_example());
        assert_eq!(
            "Allowed commands (nook.json): python verify.py | cargo test -p core",
            v.describe()
        );
        assert!(v.permits("  python verify.py  "));
        assert!(v.permits("python verify.py --fast tests/a.py"));
        assert!(v.permits("cargo test -p core parser::"));
        assert!(
            !v.permits("cargo test"),
            "only what nook.json lists: the built-in set is not added"
        );
        assert!(!v.permits("python verify.pyx"), "an entry is a whole word");
        for shell in [
            "python verify.py && del x",
            "python verify.py | more",
            "python verify.py > out",
            "python verify.py `x`",
            "python verify.py $HOME",
        ] {
            assert!(!v.permits(shell), "{shell}");
        }
        assert!(!v.permits(""));
    }

    #[test]
    fn a_nook_json_without_entries_falls_back_to_the_built_in_set() {
        for content in [
            "{not json",
            r#"{"verify": []}"#,
            r#"{"verify": "python x.py"}"#,
            r#"{"javaHome": "C:\\jdk"}"#,
        ] {
            let dir = repo(&[("nook.json", content), ("go.mod", "")]);
            let v = VerifyCommands::for_repository(dir.path());
            assert_eq!("built-in for go", v.source(), "{content}");
            assert!(v.permits("go test ./..."));
        }
        let empty = tempfile::tempdir().unwrap();
        let v = VerifyCommands::for_repository(empty.path());
        assert_eq!("built-in (no ecosystem detected)", v.source());
        assert_eq!("Allowed commands (built-in (no ecosystem detected)): python <script in the repository> [args]", v.describe());
    }

    #[test]
    fn the_output_keeps_its_tail() {
        let mut t = Tail::default();
        t.push(b"  short  \n");
        assert_eq!("short", t.text());
        let mut long = Tail::default();
        for i in 0..200_000 {
            long.push(format!("line {i}\n").as_bytes());
        }
        let text = long.text();
        assert!(text.starts_with('…'));
        assert_eq!(
            OUTPUT_TAIL,
            text.chars().count(),
            "the mark and the tail, its final newline trimmed"
        );
        assert!(text.ends_with("line 199999"));
        assert!(long.bytes.len() <= 2 * OUTPUT_KEEP);
    }

    #[tokio::test]
    async fn a_command_off_the_list_never_runs() {
        let dir = repo(&[("nook.json", r#"{"verify": ["python verify.py"]}"#)]);
        let v = VerifyCommands::for_repository(dir.path());
        let e = v
            .run("python -c \"print(1)\"", dir.path(), &HashMap::new())
            .await
            .unwrap_err();
        assert_eq!("command not allowed: python -c \"print(1)\"", e.to_string());
    }

    #[cfg(windows)]
    mod windows {
        use super::*;

        fn cmd_repo(files: &[(&str, &str)]) -> (tempfile::TempDir, VerifyCommands) {
            let mut all = vec![("nook.json", r#"{"verify": ["cmd /c"]}"#)];
            all.extend_from_slice(files);
            let dir = repo(&all);
            let v = VerifyCommands::for_repository(dir.path());
            (dir, v)
        }

        #[tokio::test]
        async fn runs_and_reports_exit_code_and_both_outputs() {
            let (dir, v) =
                cmd_repo(&[("both.cmd", "@echo out\r\n@echo oops 1>&2\r\n@exit /b 3\r\n")]);
            // .\ because this machine's cmd may not look in the current folder
            // (NoDefaultCurrentDirectoryInExePath)
            let r = v
                .run("cmd /c .\\both.cmd", dir.path(), &HashMap::new())
                .await
                .unwrap();
            assert_eq!(3, r.exit_code, "{}", r.output);
            assert!(!r.passed());
            assert!(
                r.output.contains("out") && r.output.contains("oops"),
                "{}",
                r.output
            );
            assert!(r.summary().starts_with("exit 3 in "), "{}", r.summary());

            let env = HashMap::from([("JAVA_HOME".to_string(), "C:\\jdk-for-test".to_string())]);
            let r = v
                .run("cmd /c echo %JAVA_HOME%", dir.path(), &env)
                .await
                .unwrap();
            assert_eq!(
                (0, "C:\\jdk-for-test"),
                (r.exit_code, r.output.as_str()),
                "the environment reaches the command"
            );
        }

        #[tokio::test]
        async fn a_long_output_comes_back_as_its_tail() {
            let (dir, v) = cmd_repo(&[(
                "many.cmd",
                "@for /l %%i in (1,1,3000) do @echo line %%i\r\n",
            )]);
            let r = v
                .run("cmd /c .\\many.cmd", dir.path(), &HashMap::new())
                .await
                .unwrap();
            assert!(r.passed(), "{}", r.output);
            assert!(
                r.output.starts_with('…'),
                "{}",
                r.output.chars().take(40).collect::<String>()
            );
            assert!(r.output.ends_with("line 3000"));
            assert!(r.output.chars().count() <= OUTPUT_TAIL + 1);
        }

        #[tokio::test]
        async fn gradlew_is_the_repositorys_own_wrapper() {
            let dir = repo(&[("gradlew", ""), ("gradlew.bat", "@echo wrapper %*\r\n")]);
            let v = VerifyCommands::for_repository(dir.path());
            let r = v
                .run(
                    "gradlew :agent:test --tests FooTest",
                    dir.path(),
                    &HashMap::new(),
                )
                .await
                .unwrap();
            assert!(r.passed(), "{}", r.output);
            for part in [
                "wrapper",
                ":agent:test",
                "--tests",
                "FooTest",
                "--console",
                "plain",
                "-q",
                "idletimeout",
                "600000",
            ] {
                assert!(r.output.contains(part), "{part} in {}", r.output);
            }
        }

        #[tokio::test]
        async fn a_program_that_is_not_there_says_so() {
            let dir = repo(&[("nook.json", r#"{"verify": ["no-such-program-for-nook"]}"#)]);
            let v = VerifyCommands::for_repository(dir.path());
            let e = v
                .run("no-such-program-for-nook", dir.path(), &HashMap::new())
                .await
                .unwrap_err();
            assert!(
                e.to_string()
                    .starts_with("Cannot run program \"no-such-program-for-nook\""),
                "{e}"
            );
        }

        /// The pid a script's PowerShell grandchild wrote, once it has.
        async fn grandchild(pid_file: &Path) -> u32 {
            for _ in 0..200 {
                if let Some(pid) = std::fs::read_to_string(pid_file)
                    .ok()
                    .and_then(|s| s.trim().parse().ok())
                {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            panic!("the grandchild never started");
        }

        async fn gone(pid: u32) -> bool {
            for _ in 0..60 {
                let mut sys = sysinfo::System::new();
                let p = sysinfo::Pid::from_u32(pid);
                sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[p]), true);
                if sys.process(p).is_none() {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            false
        }

        const SLEEPER: &str =
            "Set-Content -Path pid.txt -Value $PID\r\nStart-Sleep -Seconds 120\r\n";
        const TREE: &str =
            "@powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File sleep.ps1\r\n";

        #[tokio::test]
        async fn a_command_past_its_time_is_stopped_with_its_whole_tree() {
            let (dir, v) = cmd_repo(&[("tree.cmd", TREE), ("sleep.ps1", SLEEPER)]);
            let pid_file = dir.path().join("pid.txt");
            let env = HashMap::new();
            let run = v.run_within(
                "cmd /c .\\tree.cmd",
                dir.path(),
                &env,
                Duration::from_secs(8),
            );
            let (r, pid) = tokio::join!(run, grandchild(&pid_file));
            let r = r.unwrap();
            assert_eq!(-1, r.exit_code);
            assert_eq!(
                "the command ran for more than 8 seconds and was stopped",
                r.output
            );
            assert!(
                gone(pid).await,
                "the grandchild {pid} was stopped with the command"
            );
        }

        #[tokio::test]
        async fn stopping_the_worker_stops_the_tree() {
            let (dir, v) = cmd_repo(&[("tree.cmd", TREE), ("sleep.ps1", SLEEPER)]);
            let pid_file = dir.path().join("pid.txt");
            let env = HashMap::new();
            let mut run = Box::pin(v.run("cmd /c .\\tree.cmd", dir.path(), &env));
            let pid = tokio::select! {
                _ = &mut run => panic!("the command ended by itself"),
                pid = grandchild(&pid_file) => pid,
            };
            drop(run);
            assert!(
                gone(pid).await,
                "the grandchild {pid} was stopped when the run was dropped"
            );
        }
    }
}

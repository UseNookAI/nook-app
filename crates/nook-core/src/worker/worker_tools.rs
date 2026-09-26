//! The tools a local worker has over its scratch copy: search, paged read, list, exact-text
//! edit, replace-all, write, and one allowed command. The smallest set a worker on an 8 GB card
//! gets by with: without search a rename across a module cannot be done, without an exact edit a
//! small model rewrites whole files from a partial read. Every path stays inside the copy. With
//! web access on, the [`WebTools`] join them.
//!
//! Ports `worker/WorkerTools.java`. The file tools run on tokio's blocking pool (a search reads
//! every file under a folder); their state (locks, what the worker wrote, the edit count) sits
//! behind a mutex so the blocking call can own a handle to it. Lengths in messages are counted in
//! characters where the original counted UTF-16 units.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};

use super::path_policy::{absolute, PathPolicy};
use super::verify_commands::VerifyCommands;
use super::web_tools::WebTools;

pub const READ_WINDOW: i64 = 120;
pub const MAX_MATCHES: usize = 60;
pub const OUTPUT_CHARS: usize = 6000;
const SKIP_DIRS: &[&str] = &[
    ".git",
    "build",
    ".gradle",
    "node_modules",
    "target",
    "out",
    ".idea",
    "dist",
    ".venv",
    "__pycache__",
];
/// Files larger than this are not searched or rewritten.
const MAX_TEXT_FILE: u64 = 2_000_000;

/// replace_all's pointer for the worker when it left literals alone.
pub const LITERAL_HINT: &str = "(edit one with edit_file if it really names the symbol)";

const CODE_EXTENSIONS: &[&str] = &[
    "java", "kt", "kts", "groovy", "gradle", "scala", "js", "jsx", "ts", "tsx", "mjs", "cjs", "py",
    "go", "rs", "cs", "c", "h", "cpp", "hpp", "cc", "swift", "dart", "php", "rb",
];

/// `String.format("%n")`: the original numbered read_file's lines with the platform's line
/// separator, so on Windows each line of a read ends with CRLF.
const LINE_SEPARATOR: &str = if cfg!(windows) { "\r\n" } else { "\n" };

static LINE_NUMBER: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[ \t\n\x0B\x0C\r]*[0-9]+\| ?").expect("a valid pattern"));
static WHITESPACE_RUN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[ \t\n\x0B\x0C\r]+").expect("a valid pattern"));
static GRADLEW_BAT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^gradlew\.bat\b").expect("a valid pattern"));
static IDENTIFIER: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*$").expect("a valid pattern"));

pub struct WorkerTools {
    files: Arc<Mutex<FileTools>>,
    verify: Arc<VerifyCommands>,
    env: HashMap<String, String>,
    last_verification: Option<String>,
    last_command: Option<String>,
    /// Edits and writes when the last command ran; a command's result is only good for the tree
    /// it ran on.
    last_run_mutations: i64,
    last_run_passed: bool,
    /// web_search and read_page, when the person lets the worker on the web; None otherwise.
    web: Option<WebTools>,
}

/// The file side of the tools: what the blocking pool works on.
struct FileTools {
    root: PathBuf,
    /// Edits and writes so far.
    mutations: i64,
    /// Paths the worker may not change: the checks that say when the task is done.
    locked: Vec<Regex>,
    locked_as_given: Vec<String>,
    /// Files the worker wrote or edited, in this turn or in earlier ones not yet applied. An
    /// address in one of them is the worker's own and does not count as shown: otherwise a page
    /// could have it write "https://example.com/?k=" and a secret into a file, read the file back
    /// and open that.
    written: HashSet<String>,
    /// Whether the web tools are on, so text read from the person's files is passed to them.
    web: bool,
}

impl WorkerTools {
    pub fn new(root: &Path, verify: VerifyCommands, env: HashMap<String, String>) -> WorkerTools {
        WorkerTools {
            files: Arc::new(Mutex::new(FileTools {
                root: absolute(root),
                mutations: 0,
                locked: Vec::new(),
                locked_as_given: Vec::new(),
                written: HashSet::new(),
                web: false,
            })),
            verify: Arc::new(verify),
            env,
            last_verification: None,
            last_command: None,
            last_run_mutations: -1,
            last_run_passed: false,
            web: None,
        }
    }

    /// Locks paths against edit_file and write_file: a file, a folder (everything under it) or a
    /// glob (`*` within a folder, `**` across folders), relative to the root. A worker held to a
    /// test it may rewrite is held to nothing; small models under a must-pass rule weaken the check
    /// before they fix the code.
    pub fn lock<I, S>(self, patterns: I) -> WorkerTools
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        {
            let mut f = self.files.lock();
            for raw in patterns {
                let mut p = raw.as_ref().trim().replace('\\', "/");
                while let Some(rest) = p.strip_prefix("./") {
                    p = rest.to_string();
                }
                if p.ends_with('/') {
                    p.pop();
                }
                if p.is_empty() {
                    continue;
                }
                if let Ok(rx) = Regex::new(&format!("^(?:{}(/.*)?)$", glob_to_regex(&p))) {
                    f.locked.push(rx);
                    f.locked_as_given.push(p);
                }
            }
        }
        self
    }

    pub fn locked_patterns(&self) -> Vec<String> {
        self.files.lock().locked_as_given.clone()
    }

    /// Gives the worker the web tools as well.
    pub fn with_web(mut self, tools: WebTools) -> WorkerTools {
        self.files.lock().web = true;
        self.web = Some(tools);
        self
    }

    pub fn has_web(&self) -> bool {
        self.web.is_some()
    }

    /// The web tools, when the worker has them.
    pub fn web(&self) -> Option<&WebTools> {
        self.web.as_ref()
    }

    /// Files an earlier turn wrote that are not applied yet (repository-relative).
    pub fn written_before<I, S>(self, paths: I) -> WorkerTools
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        {
            let mut f = self.files.lock();
            for p in paths {
                f.written.insert(file_key(p.as_ref()));
            }
        }
        self
    }

    /// Whether a repository-relative path is locked.
    pub fn is_locked(&self, rel: &str) -> bool {
        self.files.lock().is_locked(rel)
    }

    /// The output of the last run_command, for the result.
    pub fn last_verification(&self) -> Option<&str> {
        self.last_verification.as_deref()
    }

    pub fn last_command(&self) -> Option<&str> {
        self.last_command.as_deref()
    }

    /// Edits and writes so far.
    pub fn mutations(&self) -> i64 {
        self.files.lock().mutations
    }

    /// True when the last command exited 0 and nothing was edited or written since.
    pub fn last_run_still_holds(&self) -> bool {
        self.last_command.is_some()
            && self.last_run_passed
            && self.last_run_mutations == self.mutations()
    }

    pub fn last_run_passed(&self) -> bool {
        self.last_run_passed
    }

    /// True when a command has run and nothing was edited or written since, whatever its result.
    pub fn last_run_on_current_tree(&self) -> bool {
        self.last_command.is_some() && self.last_run_mutations == self.mutations()
    }

    pub fn permits(&self, command: &str) -> bool {
        self.verify.permits(command)
    }

    /// The same command in a different spelling: whitespace, a leading `./` or `.\`, and
    /// `gradlew` against `gradlew.bat` do not make two commands different.
    pub fn same_command(a: &str, b: &str) -> bool {
        normalise_command(a) == normalise_command(b)
    }

    /// The tool definitions in the OpenAI shape llama-server understands.
    pub fn definitions(&self) -> Vec<Value> {
        let mut tools = vec![
            fn_def(
                "search_files",
                &format!("Find lines matching a regular expression in one file or in every file under a folder (case-sensitive). Returns path:line: text for up to {MAX_MATCHES} matches. Use it to find definitions, every caller of a symbol, and the line numbers to read."),
                &[("pattern", "string"), ("path", "string")],
                &["pattern"],
                &[("path", "a file, or a folder to search; '.' for the whole repository (default)")],
            ),
            fn_def(
                "read_file",
                &format!("Read lines of a file with line numbers, at most {READ_WINDOW} per call. Read the lines a search pointed at, not whole files: the context is small and old reads are dropped."),
                &[("path", "string"), ("start", "integer"), ("end", "integer")],
                &["path"],
                &[
                    ("start", "first line, 1-based (default 1)"),
                    ("end", &format!("last line inclusive (default start+{})", READ_WINDOW - 1)),
                ],
            ),
            fn_def(
                "list_dir",
                "List the files and folders in a directory (one level).",
                &[("path", "string")],
                &["path"],
                &[("path", "directory, '.' for the root")],
            ),
            fn_def(
                "edit_file",
                "Replace one exact occurrence of old_text in a file with new_text. old_text must match the file exactly once, including whitespace; include enough surrounding lines to make it unique. Keep edits small.",
                &[("path", "string"), ("old_text", "string"), ("new_text", "string")],
                &["path", "old_text", "new_text"],
                &[],
            ),
            fn_def(
                "replace_all",
                "Replace every occurrence of old_text with new_text in one file or in every file under a folder, in one call: use it to rename a symbol, where edit_file would take one call per occurrence. A single identifier is matched as a whole word, so longer names that contain it are left alone. It also changes comments and strings, so give the narrowest folder the task names, one call per folder, never '.' unless the task says the whole repository. Returns the count per file.",
                &[("old_text", "string"), ("new_text", "string"), ("path", "string")],
                &["old_text", "new_text", "path"],
                &[("path", "the file or the narrowest folder the task names")],
            ),
            fn_def(
                "write_file",
                "Create a new file with the given content. For existing files use edit_file.",
                &[("path", "string"), ("content", "string")],
                &["path", "content"],
                &[],
            ),
            fn_def(
                "run_command",
                &format!("Run one allowed command in the repository and return its exit code and output. {}", self.verify.describe()),
                &[("command", "string")],
                &["command"],
                &[],
            ),
        ];
        if let Some(web) = &self.web {
            web.add_definitions(&mut tools);
        }
        tools
    }

    /// Runs one tool call; every failure comes back as text the worker can act on.
    pub async fn call(&mut self, name: &str, args: &Value) -> String {
        if let Some(web) = self.web.as_mut() {
            if web.handles(name) {
                return web.call(name, args).await;
            }
        }
        match name {
            "search_files" | "read_file" | "list_dir" | "edit_file" | "replace_all"
            | "write_file" => {
                let files = self.files.clone();
                let tool = name.to_string();
                let args = args.clone();
                let done = tokio::task::spawn_blocking(move || {
                    let mut shown = Vec::new();
                    let out = files.lock().call(&tool, &args, &mut shown);
                    (out, shown)
                })
                .await;
                match done {
                    Ok((out, shown)) => {
                        if let Some(web) = self.web.as_mut() {
                            for text in &shown {
                                web.allow_from(text);
                            }
                        }
                        out
                    }
                    Err(e) => format!("error: {e}"),
                }
            }
            "run_command" => self
                .run(args)
                .await
                .unwrap_or_else(|e| format!("error: {e}")),
            _ => format!("error: unknown tool {name}"),
        }
    }

    async fn run(&mut self, args: &Value) -> Result<String> {
        let cmd = text(args, "command").unwrap_or_default();
        if !self.verify.permits(&cmd) {
            return Ok(format!(
                "error: command not allowed. {}",
                self.verify.describe()
            ));
        }
        let root = self.files.lock().root.clone();
        let r = self.verify.run(&cmd, &root, &self.env).await?;
        self.last_command = Some(cmd.trim().to_string());
        let out = format!("{}\n{}", r.summary(), r.output);
        self.last_verification = Some(out.clone());
        self.last_run_mutations = self.mutations();
        self.last_run_passed = r.passed();
        Ok(cut_chars(&out, OUTPUT_CHARS))
    }
}

impl FileTools {
    fn call(&mut self, name: &str, args: &Value, shown: &mut Vec<String>) -> String {
        let out = match name {
            "search_files" => self.search(args, shown),
            "read_file" => self.read(args, shown),
            "list_dir" => self.list(args),
            "edit_file" => self.edit(args),
            "replace_all" => self.replace_all(args),
            "write_file" => self.write(args),
            _ => Ok(format!("error: unknown tool {name}")),
        };
        out.unwrap_or_else(|e| format!("error: {e}"))
    }

    fn is_locked(&self, rel: &str) -> bool {
        let mut r = rel.replace('\\', "/");
        while let Some(rest) = r.strip_prefix("./") {
            r = rest.to_string();
        }
        self.locked.iter().any(|p| p.is_match(&r))
    }

    /// Addresses in a file the person's repository holds, not one the worker wrote, may be opened
    /// with read_page.
    fn shown_in(&self, rel: &str, text: &str, shown: &mut Vec<String>) {
        if self.web && !self.written.contains(&file_key(rel)) {
            shown.push(text.to_string());
        }
    }

    /// A file as the disk names it, so "NOTES.TXT" and "src/../notes.txt" are the notes.txt the
    /// worker wrote: Windows opens all three.
    fn rel(&self, p: &Path) -> String {
        let real_root = PathPolicy::real(&self.root);
        let real = PathPolicy::real(p);
        relative(&real_root, &real).unwrap_or_else(|| real.to_string_lossy().into_owned())
    }

    fn within(&self, rel: Option<&str>) -> Result<PathBuf> {
        let rel = match rel {
            Some(r) if !r.trim().is_empty() => r,
            _ => ".",
        };
        let p = absolute(&self.root.join(rel.replace('\\', "/")));
        if !inside(&p, &self.root) {
            bail!("path leaves the repository: {rel}");
        }
        // and where it really points: a link inside the copy is not a way out of it
        let real = PathPolicy::real(&p);
        if !inside(&real, &PathPolicy::real(&self.root)) {
            bail!("path leaves the repository through a link: {rel}");
        }
        Ok(p)
    }

    fn refuse_if_locked(&self, p: &Path) -> Option<String> {
        let rel = relative(&self.root, p).unwrap_or_default();
        if !self.is_locked(&rel) {
            return None;
        }
        Some(format!("error: {rel} is locked: it is part of the check that says when the task is done. Change the code it checks, not the check."))
    }

    fn search(&mut self, args: &Value, shown: &mut Vec<String>) -> Result<String> {
        let path = text(args, "path");
        let base = self.within(path.as_deref())?;
        if !base.is_dir() && !base.is_file() {
            return Ok(format!(
                "error: no such file or directory: {}",
                shown_arg(&path)
            ));
        }
        let rx = match Regex::new(&text(args, "pattern").unwrap_or_default()) {
            Ok(rx) => rx,
            Err(e) => return Ok(format!("error: bad pattern: {}", pattern_error(&e))),
        };
        let mut hits: Vec<String> = Vec::new();
        let mut files = 0usize;
        for (f, size) in regular_files_under(&base, &self.root, false) {
            let rel = relative(&self.root, &f).unwrap_or_default();
            if rel.split('/').any(|part| SKIP_DIRS.contains(&part)) {
                continue;
            }
            if size > MAX_TEXT_FILE {
                continue;
            }
            let Some(content) = read_text(&f) else {
                continue; // binary
            };
            files += 1;
            for (i, l) in content.split('\n').enumerate() {
                // Java's `$` matched before a final line terminator and its `.` did not match
                // one: a CRLF file's lines are matched without their `\r`
                let matched = l.strip_suffix('\r').unwrap_or(l);
                if rx.is_match(matched) {
                    let t = l.trim();
                    let hit = format!("{rel}:{}: {}", i + 1, take_chars(t, 200));
                    self.shown_in(&rel, &hit, shown);
                    hits.push(hit);
                    if hits.len() >= MAX_MATCHES {
                        return Ok(format!(
                            "{}+ matches (showing {MAX_MATCHES}) in {files}+ files searched:\n{}",
                            hits.len(),
                            hits.join("\n")
                        ));
                    }
                }
            }
        }
        Ok(if hits.is_empty() {
            format!("no matches in {files} files")
        } else {
            format!(
                "{} matches in {files} files searched:\n{}",
                hits.len(),
                hits.join("\n")
            )
        })
    }

    fn read(&mut self, args: &Value, shown: &mut Vec<String>) -> Result<String> {
        let path = text(args, "path");
        let p = self.within(path.as_deref())?;
        if !p.is_file() {
            return Ok(format!("error: no such file: {}", shown_arg(&path)));
        }
        let content = read_utf8(&p)?;
        let lines: Vec<&str> = content.split('\n').collect();
        let n = lines.len() as i64;
        let start = as_int(args.get("start"), 1).max(1);
        let end =
            n.min(as_int(args.get("end"), start + READ_WINDOW - 1).min(start + READ_WINDOW - 1));
        let mut sb = format!("{} lines {start}-{end} of {n}\n", shown_arg(&path));
        let mut i = start;
        while i <= end {
            sb.push_str(&format!(
                "{i:>5}| {}{LINE_SEPARATOR}",
                lines[(i - 1) as usize]
            ));
            i += 1;
        }
        let rel = self.rel(&p);
        self.shown_in(&rel, &sb, shown);
        Ok(sb)
    }

    fn list(&mut self, args: &Value) -> Result<String> {
        let path = text(args, "path");
        let p = self.within(path.as_deref())?;
        if !p.is_dir() {
            return Ok(format!("error: no such directory: {}", shown_arg(&path)));
        }
        let mut children: Vec<PathBuf> = std::fs::read_dir(&p)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        children.sort_by(|a, b| {
            path_order(
                &a.file_name().unwrap_or_default().to_string_lossy(),
                &b.file_name().unwrap_or_default().to_string_lossy(),
            )
        });
        let mut names = Vec::new();
        for c in children {
            let n = c
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if SKIP_DIRS.contains(&n.as_str()) {
                continue;
            }
            names.push(if c.is_dir() { format!("{n}/") } else { n });
        }
        Ok(names.join("\n"))
    }

    fn edit(&mut self, args: &Value) -> Result<String> {
        let path = text(args, "path");
        let p = self.within(path.as_deref())?;
        if let Some(refused) = self.refuse_if_locked(&p) {
            return Ok(refused);
        }
        if !p.is_file() {
            return Ok(format!("error: no such file: {}", shown_arg(&path)));
        }
        let old = text(args, "old_text");
        let neu = text(args, "new_text");
        let Some(old) = old.filter(|o| !o.is_empty()) else {
            return Ok("error: old_text is empty".to_string());
        };
        let neu = neu.unwrap_or_default();
        // The worker copies from read_file, whose lines carry "  123| " prefixes; a model that
        // copies them along is forgiven.
        let mut old = strip_line_numbers(&old);
        let mut neu = strip_line_numbers(&neu);
        let content = read_utf8(&p)?;
        let mut n = count(&content, &old);
        if n == 0 {
            // A second chance for line endings: the model writes \n, a Windows file has \r\n (or
            // the reverse). The file keeps its own endings and nothing but the edit changes. This
            // used to normalise the whole file instead (every ending and trailing space), and Nook
            // Code's sandbox saw a CRLF README end a run at its call budget on "not found"
            // (2026-09-23).
            let nl = if content.contains("\r\n") {
                "\r\n"
            } else {
                "\n"
            };
            let old_nl = old.replace("\r\n", "\n").replace('\n', nl);
            if count(&content, &old_nl) == 1 {
                old = old_nl;
                neu = neu.replace("\r\n", "\n").replace('\n', nl);
                n = 1;
            } else {
                // a third for trailing spaces and indentation: the lines match once the
                // whitespace around them is ignored; the file's line endings are kept
                if let Some(indented) = replace_ignoring_indent(&content, &old, &neu) {
                    std::fs::write(&p, indented)?;
                    self.mutations += 1;
                    let key = file_key(&self.rel(&p));
                    self.written.insert(key);
                    return Ok(format!(
                        "edited {} (indentation adjusted): replaced {} chars with {}",
                        shown_arg(&path),
                        old.chars().count(),
                        neu.chars().count()
                    ));
                }
                return Ok("error: old_text was not found in the file; read the lines again and copy them exactly".to_string());
            }
        }
        if n > 1 {
            return Ok(format!("error: old_text occurs {n} times; include more surrounding lines so it is unique, or use replace_all to change every one"));
        }
        std::fs::write(&p, content.replace(&old, &neu))?;
        self.mutations += 1;
        let key = file_key(&self.rel(&p));
        self.written.insert(key);
        Ok(format!(
            "edited {}: replaced {} chars with {}",
            shown_arg(&path),
            old.chars().count(),
            neu.chars().count()
        ))
    }

    /// Every occurrence of a text in a file or under a folder, in one call. The 2026-09-23 gated
    /// run had the worker rename a method with one edit_file per occurrence: twenty-four edits,
    /// the 8k context full at call 32, the gate not passed. A rename is a replacement, not a
    /// conversation. A single identifier is matched as a whole word; locked files are left alone
    /// and named.
    fn replace_all(&mut self, args: &Value) -> Result<String> {
        let path = text(args, "path");
        let Some(old) = text(args, "old_text").filter(|o| !o.is_empty()) else {
            return Ok("error: old_text is empty".to_string());
        };
        let neu = text(args, "new_text").unwrap_or_default();
        let base = self.within(path.as_deref())?;
        if !base.is_dir() && !base.is_file() {
            return Ok(format!(
                "error: no such file or directory: {}",
                shown_arg(&path)
            ));
        }
        let identifier = IDENTIFIER.is_match(&old);
        let mut changed = Vec::new();
        let mut refused = Vec::new();
        let mut total = 0usize;
        let mut in_strings = 0usize;
        for f in self.text_files_under(&base) {
            let Some(content) = read_text(&f) else {
                continue; // binary
            };
            // In code, an identifier inside a string literal is data, not a use: the independent
            // review of 2026-09-23 rejected two gated renames that had changed a test's
            // looksLikeAName("isDenied") to "denies".
            let quoted = if identifier {
                string_literal_mask(
                    &content,
                    &f.file_name().unwrap_or_default().to_string_lossy(),
                )
            } else {
                None
            };
            let mut out = String::with_capacity(content.len());
            let mut last = 0usize;
            let mut n = 0usize;
            for (start, end) in occurrences(&content, &old, identifier) {
                if quoted.as_ref().is_some_and(|q| q[start]) {
                    in_strings += 1;
                    continue;
                }
                out.push_str(&content[last..start]);
                out.push_str(&neu);
                last = end;
                n += 1;
            }
            if n == 0 {
                continue;
            }
            out.push_str(&content[last..]);
            let rel = relative(&self.root, &f).unwrap_or_default();
            if self.is_locked(&rel) {
                refused.push(rel);
                continue;
            }
            std::fs::write(&f, out)?;
            self.mutations += 1;
            self.written.insert(file_key(&rel));
            total += n;
            changed.push(format!("{rel} ({n})"));
        }
        if total == 0 && refused.is_empty() {
            return Ok(format!(
                "no occurrences of {old}{} under {}",
                if identifier { " as a whole word" } else { "" },
                path.as_deref().unwrap_or(".")
            ));
        }
        let mut sb = format!(
            "replaced {total}{} in {}{}",
            if total == 1 {
                " occurrence"
            } else {
                " occurrences"
            },
            changed.len(),
            if changed.len() == 1 {
                " file"
            } else {
                " files"
            }
        );
        if !changed.is_empty() {
            sb.push_str(": ");
            sb.push_str(&changed.join(", "));
        }
        if !refused.is_empty() {
            sb.push_str("; left alone, locked: ");
            sb.push_str(&refused.join(", "));
        }
        if in_strings > 0 {
            sb.push_str(&format!(
                "; left alone, inside string literals: {in_strings} {LITERAL_HINT}"
            ));
        }
        Ok(sb)
    }

    /// The regular text-sized files under a path, not through links and not in build or tool
    /// folders.
    fn text_files_under(&self, base: &Path) -> Vec<PathBuf> {
        if base.is_file() {
            return vec![base.to_path_buf()];
        }
        regular_files_under(base, &self.root, true)
            .into_iter()
            .filter(|(_, size)| *size <= MAX_TEXT_FILE)
            .map(|(f, _)| f)
            .collect()
    }

    fn write(&mut self, args: &Value) -> Result<String> {
        let path = text(args, "path");
        let p = self.within(path.as_deref())?;
        if let Some(refused) = self.refuse_if_locked(&p) {
            return Ok(refused);
        }
        let content = text(args, "content").unwrap_or_default();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, &content)?;
        self.mutations += 1;
        let key = file_key(&self.rel(&p));
        self.written.insert(key);
        Ok(format!(
            "wrote {} ({} chars)",
            shown_arg(&path),
            content.chars().count()
        ))
    }
}

/// The regular files under `base` in the order a directory listing gives them (names without
/// case, as NTFS keeps them), not into links. `skip_named`: folders named in [`SKIP_DIRS`] are not
/// entered (the root itself excepted). Unreadable folders are passed over.
fn regular_files_under(base: &Path, root: &Path, skip_named: bool) -> Vec<(PathBuf, u64)> {
    let mut found = Vec::new();
    let walker = walkdir::WalkDir::new(base)
        .follow_links(false)
        .sort_by(|a, b| {
            path_order(
                &a.file_name().to_string_lossy(),
                &b.file_name().to_string_lossy(),
            )
        })
        .into_iter()
        .filter_entry(|e| {
            if !e.file_type().is_dir() {
                return true;
            }
            // not into a link: the copy the worker may touch ends where the link leads away
            if PathPolicy::is_link(e.path()) {
                return false;
            }
            if skip_named && !same_path(e.path(), root) {
                let name = e.file_name().to_string_lossy();
                if SKIP_DIRS.contains(&name.as_ref()) {
                    return false;
                }
            } else if !skip_named && e.depth() > 0 {
                // a file under one of these is left out by its path anyway; no need to read it
                let name = e.file_name().to_string_lossy();
                if SKIP_DIRS.contains(&name.as_ref()) {
                    return false;
                }
            }
            true
        });
    for e in walker.filter_map(|e| e.ok()) {
        if e.file_type().is_file() {
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            found.push((e.into_path(), size));
        }
    }
    found
}

/// Directory-listing order: without case on Windows (as NTFS and Java's Windows paths sort), by
/// the name elsewhere.
fn path_order(a: &str, b: &str) -> std::cmp::Ordering {
    if cfg!(windows) {
        a.to_uppercase()
            .cmp(&b.to_uppercase())
            .then_with(|| a.cmp(b))
    } else {
        a.cmp(b)
    }
}

/// Where `old` occurs in `content`, left to right and not overlapping. A single identifier
/// counts only as a whole word: no letter, digit, `_` or `$` right before or after it (the
/// original's look-around pattern).
fn occurrences(content: &str, old: &str, identifier: bool) -> Vec<(usize, usize)> {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let bytes = content.as_bytes();
    let mut out = Vec::new();
    let mut from = 0usize;
    while from <= content.len() {
        let Some(at) = content[from..].find(old).map(|i| i + from) else {
            break;
        };
        let end = at + old.len();
        if identifier {
            let before_ok = at == 0 || !word(bytes[at - 1]);
            let after_ok = end >= bytes.len() || !word(bytes[end]);
            if !(before_ok && after_ok) {
                // the next possible start: one character on
                from = at + content[at..].chars().next().map_or(1, char::len_utf8);
                continue;
            }
        }
        out.push((at, end));
        from = end.max(at + 1);
    }
    out
}

/// Which bytes of a code file sit inside a string or character literal (`"…"`, `'…'`, `"""…"""`,
/// a backquoted template), with comments skipped so a quote in a comment opens nothing. None for
/// a file that is not code, where every occurrence counts. (Every delimiter is ASCII, so a byte
/// scan finds what the original's character scan found.)
pub(crate) fn string_literal_mask(s: &str, file_name: &str) -> Option<Vec<bool>> {
    let ext = match file_name.rfind('.') {
        Some(dot) => file_name[dot + 1..].to_lowercase(),
        None => String::new(),
    };
    if !CODE_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    let hash_comments = ext == "py" || ext == "rb";
    let b = s.as_bytes();
    let n = b.len();
    let mut mask = vec![false; n + 1];
    let mut i = 0usize;
    while i < n {
        let c = b[i];
        if !hash_comments && c == b'/' && i + 1 < n && b[i + 1] == b'/' {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
        } else if !hash_comments && c == b'/' && i + 1 < n && b[i + 1] == b'*' {
            i = match s[i + 2..].find("*/") {
                Some(end) => i + 2 + end + 2,
                None => n,
            };
        } else if hash_comments && c == b'#' {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
        } else if (c == b'"' || c == b'\'') && i + 2 < n && b[i + 1] == c && b[i + 2] == c {
            let fence = if c == b'"' { "\"\"\"" } else { "'''" };
            let stop = match s[i + 3..].find(fence) {
                Some(end) => i + 3 + end + 3,
                None => n,
            };
            for m in mask.iter_mut().take(stop).skip(i) {
                *m = true;
            }
            i = stop;
        } else if c == b'"' || c == b'\'' || (c == b'`' && !hash_comments) {
            let mut k = i + 1;
            while k < n && b[k] != c && b[k] != b'\n' {
                if b[k] == b'\\' {
                    k += 1;
                }
                k += 1;
            }
            let stop = n.min(k + 1);
            for m in mask.iter_mut().take(stop).skip(i) {
                *m = true;
            }
            i = stop;
        } else {
            i += 1;
        }
    }
    Some(mask)
}

/// Drops a read_file line-number prefix from every line when every non-blank line carries one.
pub(crate) fn strip_line_numbers(t: &str) -> String {
    let normal = t.replace("\r\n", "\n");
    let lines: Vec<&str> = normal.split('\n').collect();
    let mut numbered = 0;
    let mut non_blank = 0;
    for l in &lines {
        if l.trim().is_empty() {
            continue;
        }
        non_blank += 1;
        if LINE_NUMBER.is_match(l) {
            numbered += 1;
        }
    }
    if non_blank == 0 || numbered < non_blank {
        return t.to_string();
    }
    lines
        .iter()
        .map(|l| LINE_NUMBER.replace(l, "").into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Finds the one run of lines that equals `old` line by line once leading whitespace is ignored,
/// and replaces it with `neu` re-indented by the difference between the file's first line and
/// old_text's first line. None when there is no such run or more than one.
pub(crate) fn replace_ignoring_indent(content: &str, old: &str, neu: &str) -> Option<String> {
    let nl = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let normal = content.replace("\r\n", "\n");
    let lines: Vec<&str> = normal.split('\n').collect();
    let old_normal = old.replace("\r\n", "\n");
    let mut old_lines: Vec<&str> = old_normal.split('\n').collect();
    while old_lines.last().is_some_and(|l| l.trim().is_empty()) {
        old_lines.pop();
    }
    if old_lines.is_empty() {
        return None;
    }
    let mut at = None;
    let mut matches = 0;
    let mut i = 0;
    while i + old_lines.len() <= lines.len() {
        if (0..old_lines.len()).all(|j| lines[i + j].trim() == old_lines[j].trim()) {
            matches += 1;
            at = Some(i);
        }
        i += 1;
    }
    let at = at.filter(|_| matches == 1)?;
    let delta = indent_of(lines[at]) as i64 - indent_of(old_lines[0]) as i64;
    let mut out: Vec<String> = lines[..at].iter().map(|l| l.to_string()).collect();
    let neu_normal = neu.replace("\r\n", "\n");
    for l in neu_normal.split('\n') {
        // no shift to make (the lines differed only in trailing spaces): new text as written,
        // tabs kept
        if delta == 0 || l.trim().is_empty() {
            out.push(if l.trim().is_empty() {
                String::new()
            } else {
                l.to_string()
            });
            continue;
        }
        let ind = (indent_of(l) as i64 + delta).max(0) as usize;
        out.push(format!("{}{}", " ".repeat(ind), l.trim_start()));
    }
    out.extend(lines[at + old_lines.len()..].iter().map(|l| l.to_string()));
    Some(out.join(nl))
}

fn indent_of(l: &str) -> usize {
    l.bytes().take_while(|b| *b == b' ' || *b == b'\t').count()
}

pub(crate) fn glob_to_regex(glob: &str) -> String {
    let chars: Vec<char> = glob.chars().collect();
    let mut sb = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '*' {
            if i + 1 < chars.len() && chars[i + 1] == '*' {
                sb.push_str(".*");
                i += 1;
                // "**/" also matches no folder at all
                if i + 1 < chars.len() && chars[i + 1] == '/' {
                    i += 1;
                }
            } else {
                sb.push_str("[^/]*");
            }
        } else if c == '?' {
            sb.push_str("[^/]");
        } else {
            sb.push_str(&regex::escape(&c.to_string()));
        }
        i += 1;
    }
    sb
}

pub(crate) fn normalise_command(c: &str) -> String {
    let mut s = WHITESPACE_RUN.replace_all(c.trim(), " ").into_owned();
    if s.starts_with("./") || s.starts_with(".\\") {
        s = s[2..].to_string();
    }
    let s = GRADLEW_BAT.replace(&s, "gradlew").into_owned();
    // Only the program's name is case-blind. The arguments are not: "--tests FooTest" and
    // "--tests footest" are different commands, and treating them as one let a worker's run of
    // the second be reported as verification of the first.
    match s.find(' ') {
        None => s.to_lowercase(),
        Some(sp) => format!("{}{}", s[..sp].to_lowercase(), &s[sp..]),
    }
}

/// One tool in the OpenAI shape: `props` names and types, `notes` descriptions of some of them.
pub(crate) fn fn_def(
    name: &str,
    description: &str,
    props: &[(&str, &str)],
    required: &[&str],
    notes: &[(&str, &str)],
) -> Value {
    let mut p = Map::new();
    for (key, ty) in props {
        let mut prop = Map::new();
        prop.insert("type".into(), json!(ty));
        if let Some((_, note)) = notes.iter().find(|(k, _)| k == key) {
            prop.insert("description".into(), json!(note));
        }
        p.insert(key.to_string(), Value::Object(prop));
    }
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": p,
                "required": required,
            }
        }
    })
}

// ---------------------------------------------------------------------- Jackson's readings

/// An argument as text (`asText` of a present, non-null value): numbers and booleans spelled out,
/// objects and arrays empty; None when missing or null.
pub(crate) fn text(args: &Value, key: &str) -> Option<String> {
    match args.get(key)? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => Some(String::new()),
    }
}

/// `asText(default)`: a present, non-null value as text, else the default.
pub(crate) fn as_text(v: Option<&Value>, default: &str) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Null) | None => default.to_string(),
        Some(_) => String::new(),
    }
}

/// `asInt(default)`: numbers truncated, numeric text parsed, booleans 1 and 0; the default when
/// missing, null or not a number.
pub(crate) fn as_int(v: Option<&Value>, default: i64) -> i64 {
    match v {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(default),
        Some(Value::String(s)) => {
            let t = s.trim();
            t.parse::<i64>()
                .ok()
                .or_else(|| t.parse::<f64>().ok().map(|f| f.trunc() as i64))
                .unwrap_or(default)
        }
        Some(Value::Bool(b)) => i64::from(*b),
        _ => default,
    }
}

// ---------------------------------------------------------------------- helpers

/// The argument as the original printed it into a message (`"null"` when missing).
fn shown_arg(v: &Option<String>) -> &str {
    v.as_deref().unwrap_or("null")
}

fn file_key(rel: &str) -> String {
    rel.replace('\\', "/").to_lowercase()
}

/// `p` inside `root` (or `root` itself), component by component; without case on Windows.
fn inside(p: &Path, root: &Path) -> bool {
    let mut parts = p.components();
    root.components().all(|c| {
        parts
            .next()
            .is_some_and(|x| same_component(x.as_os_str(), c.as_os_str()))
    })
}

fn same_component(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.components().count() == b.components().count() && inside(a, b)
}

/// `p` relative to `root` with `/` between the parts, or None when it is not inside.
fn relative(root: &Path, p: &Path) -> Option<String> {
    if !inside(p, root) {
        return None;
    }
    let parts: Vec<String> = p
        .components()
        .skip(root.components().count())
        .filter(|c| !matches!(c, Component::CurDir))
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

/// A file's text, or None when it is not UTF-8 (binary) or cannot be read.
fn read_text(f: &Path) -> Option<String> {
    String::from_utf8(std::fs::read(f).ok()?).ok()
}

/// A file's text; an error when it is not UTF-8 (the original's MalformedInputException).
fn read_utf8(f: &Path) -> Result<String> {
    let bytes = std::fs::read(f)?;
    String::from_utf8(bytes).map_err(|_| anyhow!("the file is not UTF-8 text"))
}

/// The one line of a regex syntax error that says what is wrong.
fn pattern_error(e: &regex::Error) -> String {
    let s = e.to_string();
    s.lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("error: ").map(str::to_string))
        .unwrap_or_else(|| s.lines().last().unwrap_or("").trim().to_string())
}

fn count(text: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    text.matches(needle).count()
}

fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The first `n` characters of `s`.
pub(crate) fn cut_chars(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((at, _)) => s[..at].to_string(),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_lock_files_folders_and_patterns() {
        assert_eq!("[^/]*\\.md", glob_to_regex("*.md"));
        assert_eq!("spec/.*[^/]*\\.md", glob_to_regex("spec/**/*.md"));
    }

    #[test]
    fn line_numbers_are_dropped_only_when_every_line_has_one() {
        assert_eq!(
            "  go();\n  done();",
            strip_line_numbers("   4|   go();\n   5|   done();")
        );
        assert_eq!(
            "   4|   go();\nplain",
            strip_line_numbers("   4|   go();\nplain")
        );
    }

    #[test]
    fn a_literal_in_code_is_masked_and_a_comment_opens_nothing() {
        let s = "a(\"x\") // it's\nb('y')";
        let m = string_literal_mask(s, "A.java").unwrap();
        assert!(m[2] && m[4] && !m[6]);
        assert!(!m[s.find("it's").unwrap() + 2], "the quote in a comment");
        assert!(m[s.find("'y'").unwrap()]);
        assert!(string_literal_mask(s, "notes.txt").is_none());
    }

    #[test]
    fn whole_words_only_for_an_identifier() {
        let s = "isDenied isDeniedAsWritten $isDenied isDenied";
        assert_eq!(vec![(0, 8), (37, 45)], occurrences(s, "isDenied", true));
        assert_eq!(4, occurrences(s, "isDenied", false).len());
    }

    #[test]
    fn jackson_readings() {
        let v = json!({"a": 5, "b": "7", "c": 2.9, "d": null, "e": true, "f": "x", "g": [1]});
        assert_eq!(5, as_int(v.get("a"), 1));
        assert_eq!(7, as_int(v.get("b"), 1));
        assert_eq!(2, as_int(v.get("c"), 1));
        assert_eq!(1, as_int(v.get("d"), 1));
        assert_eq!(1, as_int(v.get("e"), 0));
        assert_eq!(9, as_int(v.get("f"), 9));
        assert_eq!(9, as_int(v.get("missing"), 9));
        assert_eq!(Some("5".to_string()), text(&v, "a"));
        assert_eq!(None, text(&v, "d"));
        assert_eq!(Some(String::new()), text(&v, "g"));
        assert_eq!("dflt", as_text(v.get("d"), "dflt"));
    }
}

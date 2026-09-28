//! Ports `runtime/EngineProcess.java`: one `llama-server` process serving one model.

use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Child;

use super::backend::Backend;
use super::inference_client::InferenceClient;
use super::model_registry::now_iso;

/// Where an engine is in its life. Serialized as the Java constant name (`"READY"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum State {
    Starting,
    Ready,
    Stopping,
    Stopped,
    Failed,
}

/// How a model is placed on the engine.
///
/// - `gpu_layers`: layers on the GPU; 999 for all of them, -1 to let llama.cpp fit them itself
///   (Vulkan, or experts in RAM)
/// - `ctx_per_slot`: context tokens one request sees; llama-server's `--ctx-size` is the total
///   over `--parallel` slots, so the launcher passes [`Plan::ctx_total`]
/// - `slots`: parallel requests, each with `ctx_per_slot`
/// - `tensor_split`: llama.cpp `--tensor-split` proportions for several GPUs, or None
/// - `embedding`: serve embeddings only (`--embedding`); required for pooling models such as
///   nomic-embed, which llama-server otherwise refuses to run
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub gpu_layers: i32,
    pub ctx_per_slot: u32,
    pub slots: u32,
    pub kv_quantized: bool,
    pub flash_attention: bool,
    pub tensor_split: Option<String>,
    pub embedding: bool,
    pub experts_in_ram: bool,
}

impl Plan {
    pub fn new(
        gpu_layers: i32,
        ctx_per_slot: u32,
        slots: u32,
        kv_quantized: bool,
        flash_attention: bool,
    ) -> Plan {
        Plan {
            gpu_layers,
            ctx_per_slot,
            slots,
            kv_quantized,
            flash_attention,
            tensor_split: None,
            embedding: false,
            experts_in_ram: false,
        }
    }

    /// The same plan split across several GPUs (None: one device).
    pub fn with_split(mut self, tensor_split: Option<String>) -> Plan {
        self.tensor_split = tensor_split;
        self
    }

    /// The one conversion from a request's context to the engine's: `--ctx-size` is every
    /// slot's share added up.
    pub fn ctx_total(&self) -> u32 {
        self.ctx_per_slot.saturating_mul(self.slots.max(1))
    }

    /// The same placement for an embedding model.
    pub fn as_embedding(&self) -> Plan {
        Plan {
            kv_quantized: false,
            flash_attention: false,
            embedding: true,
            experts_in_ram: false,
            ..self.clone()
        }
    }

    /// A mixture-of-experts model that does not fit the card: attention and the shared layers on
    /// the GPU, the expert weights in system RAM (`--cpu-moe`), which runs a 30B-class model at
    /// the speed of its active parameters instead of paging layers.
    pub fn with_experts_in_ram(&self) -> Plan {
        Plan {
            gpu_layers: -1,
            slots: 1,
            experts_in_ram: true,
            ..self.clone()
        }
    }
}

/// `--n-gpu-layers`: the count, or `auto` when llama.cpp fits the layers itself.
pub fn gpu_layers_argument(plan: &Plan) -> String {
    if plan.gpu_layers < 0 {
        "auto".to_string()
    } else {
        plan.gpu_layers.to_string()
    }
}

/// The command line for a plan (the executable first), so a test can hold the launcher to the
/// plan's units: the context passed is the total, the slots are the plan's.
pub fn command(
    exe: &Path,
    model_file: &Path,
    port: u16,
    api_key: &str,
    plan: &Plan,
) -> Vec<String> {
    let mut cmd: Vec<String> = vec![
        exe.display().to_string(),
        "--model".into(),
        model_file.display().to_string(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "--api-key".into(),
        api_key.to_string(),
        "--ctx-size".into(),
        plan.ctx_total().to_string(),
        "--parallel".into(),
        plan.slots.to_string(),
        "--n-gpu-layers".into(),
        gpu_layers_argument(plan),
        "--no-webui".into(),
        "--reasoning-format".into(),
        "none".into(),
        "--metrics".into(),
    ];
    if plan.embedding {
        // Pooling models need embedding mode, and a non-causal model must fit each input in one
        // physical batch, so the batch sizes follow the context a request has.
        cmd.push("--embedding".into());
        cmd.push("--batch-size".into());
        cmd.push(plan.ctx_per_slot.to_string());
        cmd.push("--ubatch-size".into());
        cmd.push(plan.ctx_per_slot.to_string());
    }
    if plan.experts_in_ram {
        cmd.push("--cpu-moe".into());
    }
    if plan.gpu_layers < 0 {
        // Vulkan, or experts in RAM: let llama.cpp fit what stays on the card.
        cmd.extend(["--fit", "on", "--fit-target", "1024"].map(String::from));
    }
    if plan.flash_attention {
        cmd.push("--flash-attn".into());
        cmd.push("on".into());
    }
    if let Some(split) = plan.tensor_split.as_ref().filter(|_| plan.gpu_layers > 0) {
        cmd.push("--split-mode".into());
        cmd.push("layer".into());
        cmd.push("--tensor-split".into());
        cmd.push(split.clone());
    }
    if plan.kv_quantized {
        cmd.extend(["--cache-type-k", "q8_0", "--cache-type-v", "q8_0"].map(String::from));
    }
    cmd
}

/// Reads the engine's own context per slot and slot count out of its `/props` reply
/// (`default_generation_settings.n_ctx`, `total_slots`); -1 for a field it lacks.
pub fn context_from_props(props: Option<&Value>) -> (i32, i32) {
    let Some(props) = props else {
        return (-1, -1);
    };
    let int = |v: Option<&Value>| {
        v.and_then(Value::as_i64)
            .and_then(|n| i32::try_from(n).ok())
            .unwrap_or(-1)
    };
    (
        int(props.pointer("/default_generation_settings/n_ctx")),
        int(props.get("total_slots")),
    )
}

/// None when the engine's own numbers agree with the plan (or were not reported), else one
/// sentence saying how they differ: the check the planner's units are held to on every load.
pub fn context_mismatch(
    plan: &Plan,
    engine_ctx_per_slot: i32,
    engine_slots: i32,
) -> Option<String> {
    let mut sb = String::new();
    if engine_ctx_per_slot > 0 && engine_ctx_per_slot as i64 != plan.ctx_per_slot as i64 {
        sb.push_str(&format!(
            "the engine gives each request {engine_ctx_per_slot} tokens of context, the plan said {}",
            plan.ctx_per_slot
        ));
    }
    if engine_slots > 0 && engine_slots as i64 != plan.slots as i64 {
        if !sb.is_empty() {
            sb.push_str("; ");
        }
        sb.push_str(&format!(
            "the engine runs {engine_slots} slots, the plan said {}",
            plan.slots
        ));
    }
    (!sb.is_empty()).then_some(sb)
}

/// Why llama-server gave up on a model while it started, from the fatal lines llama.cpp writes to
/// its log (`llama_model_load: error loading model: unknown model architecture: 'x'`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadFailure {
    /// This engine build does not know the model's architecture (`unknown model architecture`).
    UnknownArchitecture(String),
    /// The card or the machine ran out of memory for the weights, the cache or the buffers.
    OutOfMemory,
    /// Any other refusal: a file that is not a GGUF, is cut short, or lacks the keys or tensors
    /// its architecture needs (`error loading model: <why>`); the engine's words when it gave any.
    Refused(Option<String>),
}

impl LoadFailure {
    /// True when starting the engine again cannot help: the same build reads the same file the
    /// same way. Memory can come free, and a failure without a reason may have been anything.
    pub fn permanent(&self) -> bool {
        match self {
            LoadFailure::UnknownArchitecture(_) => true,
            LoadFailure::OutOfMemory => false,
            LoadFailure::Refused(why) => why.is_some(),
        }
    }
}

/// The failure one start's log names, or None when it names none. `log` is the text after that
/// start's `=== ... starting` line ([`start_log`]); the first fatal line of the most telling kind
/// wins, since llama.cpp repeats a failure on its way out (`failed to load model`, `exiting due
/// to model loading error`).
pub fn load_failure(log: &str) -> Option<LoadFailure> {
    const ARCH: &str = "unknown model architecture: '";
    const ERROR: &str = "error loading model: ";
    // cudaMalloc's "out of memory", ggml's "failed to allocate CUDA0 buffer" and "unable to
    // allocate", and Vulkan's device memory errors.
    const MEMORY: [&str; 5] = [
        "out of memory",
        "failed to allocate",
        "unable to allocate",
        "ErrorOutOfDeviceMemory",
        "Device memory allocation of size",
    ];
    let lines: Vec<&str> = log.lines().collect();
    if let Some(arch) = lines.iter().find_map(|l| {
        let at = l.find(ARCH)? + ARCH.len();
        let rest = &l[at..];
        Some(rest[..rest.find('\'').unwrap_or(rest.len())].to_string())
    }) {
        return Some(LoadFailure::UnknownArchitecture(arch));
    }
    if lines.iter().any(|l| MEMORY.iter().any(|m| l.contains(m))) {
        return Some(LoadFailure::OutOfMemory);
    }
    if let Some(why) = lines.iter().find_map(|l| {
        let at = l.find(ERROR)? + ERROR.len();
        Some(l[at..].trim().to_string())
    }) {
        return Some(LoadFailure::Refused(Some(why).filter(|w| !w.is_empty())));
    }
    lines
        .iter()
        .any(|l| l.contains("failed to load model"))
        .then_some(LoadFailure::Refused(None))
}

/// An engine that exited before it answered: its exit code, what its log says about why, and the
/// message with the log's tail (`Engine exited with code 1. ...`).
#[derive(Clone, Debug)]
pub struct EngineExited {
    pub code: i32,
    pub cause: Option<LoadFailure>,
    pub message: String,
}

impl std::fmt::Display for EngineExited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EngineExited {}

/// What the latest start wrote to a log that keeps every start: the text after its last
/// `=== ... starting` line, read from the end of the file only (a start's failure is short, the
/// log of a model used for months is not).
pub(crate) fn start_log(file: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    const WINDOW: u64 = 256 << 10;
    let Ok(mut f) = std::fs::File::open(file) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if f.seek(SeekFrom::Start(len.saturating_sub(WINDOW))).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if f.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    match text.rfind("\n=== ") {
        Some(at) => text[at + 1..].to_string(),
        None => text.into_owned(),
    }
}

struct Inner {
    child: Option<Child>,
    state: State,
    started_at: Option<DateTime<Utc>>,
    failure: Option<String>,
}

/// One `llama-server` process serving one model on a random loopback port with a per-launch API
/// key. The process is started through [`crate::process`], so no console window flashes and it
/// dies with the app even if the app is killed, and its output goes to a per-model log under the
/// runtime logs directory.
pub struct EngineProcess {
    model_id: String,
    model_file: PathBuf,
    backend: Backend,
    exe: PathBuf,
    log_file: PathBuf,
    plan: Plan,
    port: u16,
    api_key: String,
    client: InferenceClient,
    in_flight: AtomicI64,
    last_used: Mutex<DateTime<Utc>>,
    /// What the engine reported about itself after start (`/props`): -1 until read.
    engine_ctx_per_slot: AtomicI32,
    engine_slots: AtomicI32,
    inner: Mutex<Inner>,
    /// One start at a time (the Java method was `synchronized`).
    starting: tokio::sync::Mutex<()>,
}

impl EngineProcess {
    /// An engine for `model_file`, run by `exe` (`llama-server.exe` in the backend's bin folder),
    /// logging to `<log_dir>\<model_id>.log`. Picks the port and the key; starts nothing.
    pub fn new(
        model_id: &str,
        model_file: &Path,
        backend: Backend,
        exe: &Path,
        log_dir: &Path,
        plan: Plan,
    ) -> Result<EngineProcess> {
        let port = free_port()?;
        let api_key = random_key();
        std::fs::create_dir_all(log_dir)
            .with_context(|| format!("Could not create {}", log_dir.display()))?;
        Ok(EngineProcess {
            model_id: model_id.to_string(),
            model_file: model_file.to_path_buf(),
            backend,
            exe: exe.to_path_buf(),
            log_file: log_dir.join(format!("{model_id}.log")),
            plan,
            port,
            client: InferenceClient::new(format!("http://127.0.0.1:{port}"), Some(api_key.clone())),
            api_key,
            in_flight: AtomicI64::new(0),
            last_used: Mutex::new(Utc::now()),
            engine_ctx_per_slot: AtomicI32::new(-1),
            engine_slots: AtomicI32::new(-1),
            inner: Mutex::new(Inner {
                child: None,
                state: State::Stopped,
                started_at: None,
                failure: None,
            }),
            starting: tokio::sync::Mutex::new(()),
        })
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }
    pub fn backend(&self) -> Backend {
        self.backend
    }
    pub fn plan(&self) -> &Plan {
        &self.plan
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn state(&self) -> State {
        self.inner.lock().state
    }
    pub fn failure(&self) -> Option<String> {
        self.inner.lock().failure.clone()
    }
    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        self.inner.lock().started_at
    }
    pub fn last_used(&self) -> DateTime<Utc> {
        *self.last_used.lock()
    }
    pub fn in_flight(&self) -> u32 {
        self.in_flight.load(Ordering::SeqCst).max(0) as u32
    }
    pub fn client(&self) -> &InferenceClient {
        &self.client
    }
    pub fn log_file(&self) -> &Path {
        &self.log_file
    }

    /// The context per request the engine itself reports, or -1 when it has not said.
    pub fn engine_ctx_per_slot(&self) -> i32 {
        self.engine_ctx_per_slot.load(Ordering::SeqCst)
    }

    /// The slot count the engine itself reports, or -1 when it has not said.
    pub fn engine_slots(&self) -> i32 {
        self.engine_slots.load(Ordering::SeqCst)
    }

    pub fn touch(&self) {
        *self.last_used.lock() = Utc::now();
    }
    pub fn begin_request(&self) {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        self.touch();
    }
    pub fn end_request(&self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        self.touch();
    }

    pub fn is_alive(&self) -> bool {
        match self.inner.lock().child.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Launches the server and waits until it reports healthy or fails.
    pub async fn start(&self, timeout: Duration) -> Result<()> {
        let _one = self.starting.lock().await;
        {
            let mut inner = self.inner.lock();
            if matches!(inner.state, State::Ready | State::Starting) {
                return Ok(());
            }
            inner.state = State::Starting;
            inner.failure = None;
        }
        if !self.exe.exists() {
            return Err(self.fail(format!("Engine binary missing: {}", self.exe.display())));
        }
        let cmd = command(
            &self.exe,
            &self.model_file,
            self.port,
            &self.api_key,
            &self.plan,
        );
        append_log(
            &self.log_file,
            &format!(
                "\n=== {} starting {}\n",
                now_iso(),
                redacted(&cmd).join(" ")
            ),
        );
        let (out, err) = match log_stdio(&self.log_file) {
            Ok(s) => s,
            Err(e) => return Err(self.fail(format!("{e:#}"))),
        };
        let mut c = crate::process::command(&cmd[0]);
        c.args(&cmd[1..])
            .env("LLAMA_LOG_COLORS", "0")
            .stdout(out)
            .stderr(err);
        if let Some(dir) = self.exe.parent() {
            c.current_dir(dir);
        }
        let child = match crate::process::spawn_managed(&mut c) {
            Ok(child) => child,
            Err(e) => {
                return Err(self.fail(format!(
                    "Could not start the engine {}: {e}",
                    self.exe.display()
                )))
            }
        };
        {
            let mut inner = self.inner.lock();
            inner.child = Some(child);
            inner.started_at = Some(Utc::now());
        }

        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.exit_status() {
                Exited::Running => {}
                Exited::Code(code) => {
                    let failure = format!("Engine exited with code {code}. {}", self.log_tail(40));
                    let cause = load_failure(&start_log(&self.log_file));
                    self.mark_failed(&failure);
                    return Err(EngineExited {
                        code,
                        cause,
                        message: failure,
                    }
                    .into());
                }
                Exited::Stopped => {
                    return Err(self.fail("The engine was stopped while it started.".into()))
                }
            }
            if self.client.health().await {
                self.inner.lock().state = State::Ready;
                self.touch();
                self.read_context().await;
                tracing::info!(
                    "Engine for {} ready on port {} (ngl={}, ctx={} per request x {} slots = {}, engine says ctx={} slots={}, backend={})",
                    self.model_id,
                    self.port,
                    self.plan.gpu_layers,
                    self.plan.ctx_per_slot,
                    self.plan.slots,
                    self.plan.ctx_total(),
                    self.engine_ctx_per_slot(),
                    self.engine_slots(),
                    self.backend.id()
                );
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        self.stop().await;
        let failure = format!(
            "Engine did not become healthy within {} s. {}",
            timeout.as_secs(),
            self.log_tail(20)
        );
        Err(self.fail(failure))
    }

    fn fail(&self, failure: String) -> anyhow::Error {
        self.mark_failed(&failure);
        anyhow::anyhow!(failure)
    }

    fn mark_failed(&self, failure: &str) {
        let mut inner = self.inner.lock();
        inner.state = State::Failed;
        inner.failure = Some(failure.to_string());
    }

    fn exit_status(&self) -> Exited {
        match self.inner.lock().child.as_mut() {
            None => Exited::Stopped,
            Some(c) => match c.try_wait() {
                Ok(None) => Exited::Running,
                Ok(Some(status)) => Exited::Code(status.code().unwrap_or(-1)),
                Err(_) => Exited::Code(-1),
            },
        }
    }

    /// Asks the engine for its own context and slot numbers; a failure leaves them unknown.
    async fn read_context(&self) {
        match self.client.props().await {
            Ok(props) => {
                let (ctx, slots) = context_from_props(Some(&props));
                self.engine_ctx_per_slot.store(ctx, Ordering::SeqCst);
                self.engine_slots.store(slots, Ordering::SeqCst);
            }
            Err(e) => tracing::debug!("Engine for {} did not answer /props: {e}", self.model_id),
        }
    }

    /// None when the engine agrees with its plan (or has not said), else one sentence on the
    /// difference.
    pub fn context_mismatch(&self) -> Option<String> {
        context_mismatch(&self.plan, self.engine_ctx_per_slot(), self.engine_slots())
    }

    /// Stops the process (and anything it started) and waits for it to go.
    pub async fn stop(&self) {
        let child = {
            let mut inner = self.inner.lock();
            match inner.child.take() {
                None => {
                    inner.state = State::Stopped;
                    return;
                }
                Some(c) => {
                    inner.state = State::Stopping;
                    c
                }
            }
        };
        terminate(child).await;
        self.inner.lock().state = State::Stopped;
        tracing::info!("Engine for {} stopped", self.model_id);
    }

    /// Last lines of the engine log, for error messages.
    pub fn log_tail(&self, lines: usize) -> String {
        log_tail(&self.log_file, lines)
    }

    /// Kills the process behind the engine's back, as a crash would.
    #[cfg(test)]
    pub(crate) async fn crash_for_test(&self) {
        let pid = self.inner.lock().child.as_ref().and_then(|c| c.id());
        if let Some(pid) = pid {
            let _ = tokio::task::spawn_blocking(move || kill_descendants(pid)).await;
        }
        if let Some(c) = self.inner.lock().child.as_mut() {
            let _ = c.start_kill();
        }
        for _ in 0..250 {
            if !self.is_alive() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl std::fmt::Debug for EngineProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineProcess")
            .field("model_id", &self.model_id)
            .field("port", &self.port)
            .field("state", &self.state())
            .field("plan", &self.plan)
            .finish()
    }
}

enum Exited {
    Running,
    Code(i32),
    Stopped,
}

// ------------------------------------------------------------------ shared by the engines

/// A free port on the loopback interface.
pub(crate) fn free_port() -> Result<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .context("Could not find a free port for the engine")?;
    Ok(listener.local_addr()?.port())
}

/// A per-launch API key: 24 random bytes as hex.
fn random_key() -> String {
    hex::encode(rand::random::<[u8; 24]>())
}

/// The command line with the API key hidden, for the log.
pub(crate) fn redacted(cmd: &[String]) -> Vec<String> {
    let mut out = cmd.to_vec();
    if let Some(i) = out.iter().position(|a| a == "--api-key") {
        if i + 1 < out.len() {
            out[i + 1] = "***".into();
        }
    }
    out
}

/// Appends text to a log file, creating it; a failure is only logged.
pub(crate) fn append_log(file: &Path, text: &str) {
    use std::io::Write;
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .and_then(|mut f| f.write_all(text.as_bytes()));
    if let Err(e) = result {
        tracing::debug!("Could not write {}: {e}", file.display());
    }
}

/// Standard output and error of a child, both appended to one log file.
pub(crate) fn log_stdio(file: &Path) -> Result<(Stdio, Stdio)> {
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .with_context(|| format!("Could not open {}", file.display()))?;
    let err = out
        .try_clone()
        .with_context(|| format!("Could not open {}", file.display()))?;
    Ok((Stdio::from(out), Stdio::from(err)))
}

/// The last `lines` lines of a text file joined with `\n`; "" when it cannot be read.
pub(crate) fn log_tail(file: &Path, lines: usize) -> String {
    let Ok(bytes) = std::fs::read(file) else {
        return String::new();
    };
    // Java's readAllLines: \n, \r\n and \r each end a line; a final line end starts none.
    let text = String::from_utf8_lossy(&bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut all: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        all.pop();
    }
    let from = all.len().saturating_sub(lines);
    all[from..].join("\n")
}

/// Ends a child and everything it started, then waits for it: the original put each engine in
/// a job object of its own and closed it on stop, which ended the whole tree. Waits five seconds,
/// kills again, and waits five more.
pub(crate) async fn terminate(mut child: Child) {
    kill_tree(&mut child).await;
    if tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .is_err()
    {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    }
}

/// Kills the child's descendants, then the child (the original's
/// `descendants().forEach(destroyForcibly)` before `destroyForcibly`).
pub(crate) async fn kill_tree(child: &mut Child) {
    if let Some(pid) = child.id() {
        let _ = tokio::task::spawn_blocking(move || kill_descendants(pid)).await;
    }
    let _ = child.start_kill();
}

fn kill_descendants(pid: u32) {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let processes = sys.processes();
    let mut tree = vec![Pid::from_u32(pid)];
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i];
        for (p, proc_) in processes {
            if proc_.parent() == Some(parent) && !tree.contains(p) {
                tree.push(*p);
            }
        }
        i += 1;
    }
    for p in tree.iter().skip(1) {
        if let Some(proc_) = processes.get(p) {
            proc_.kill();
        }
    }
}

/// A double as Java's `String.valueOf(double)` writes the values Nook formats: `6.0`, `23.5`.
pub(crate) fn java_double(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e7 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    fn arg_after(cmd: &[String], flag: &str) -> i64 {
        let i = cmd
            .iter()
            .position(|a| a == flag)
            .unwrap_or_else(|| panic!("{flag} missing from {cmd:?}"));
        cmd[i + 1].parse().unwrap()
    }

    fn launch(plan: &Plan) -> Vec<String> {
        command(
            Path::new("llama-server.exe"),
            Path::new("m.gguf"),
            41001,
            "key",
            plan,
        )
    }

    /// The launcher is held to the plan's units: a request's context is what the plan says, the
    /// engine's `--ctx-size` is that times the slots, and the engine's own report is compared
    /// with the plan after every start (the Codex report of 2026-09-22, P4).
    #[test]
    fn the_engine_is_launched_with_the_total_and_each_request_keeps_its_share() {
        let two = Plan::new(15, 4096, 2, true, true);
        assert_eq!(two.ctx_total(), 8192, "two slots of 4096");
        let cmd = launch(&two);
        assert_eq!(
            arg_after(&cmd, "--ctx-size"),
            8192,
            "the engine gets the total and divides it between the slots"
        );
        assert_eq!(arg_after(&cmd, "--parallel"), 2);
        let ngl = cmd.iter().position(|a| a == "--n-gpu-layers").unwrap();
        assert_eq!(cmd[ngl + 1], "15");
        assert!(
            cmd.contains(&"--cache-type-k".into())
                && cmd.contains(&"q8_0".into())
                && cmd.contains(&"--flash-attn".into())
        );

        let four = Plan::new(999, 8192, 4, true, true);
        assert_eq!(
            arg_after(&launch(&four), "--ctx-size"),
            32768,
            "four slots of 8192, as the footprint budgeted"
        );
        assert_eq!(arg_after(&launch(&four), "--parallel"), 4);

        let one = Plan::new(999, 8192, 1, false, false);
        assert_eq!(
            arg_after(&launch(&one), "--ctx-size"),
            8192,
            "one slot: the two units coincide"
        );

        let embedding = Plan::new(999, 2048, 1, false, false).as_embedding();
        let emb = launch(&embedding);
        assert!(emb.contains(&"--embedding".into()));
        assert_eq!(
            arg_after(&emb, "--batch-size"),
            2048,
            "an input must fit one request's context"
        );
        assert_eq!(arg_after(&emb, "--ubatch-size"), 2048);
        assert!(
            !emb.contains(&"--cache-type-k".into()),
            "an embedding plan runs its KV at f16"
        );

        let moe = Plan::new(999, 4096, 2, true, true).with_experts_in_ram();
        let cpu_moe = launch(&moe);
        assert!(cpu_moe.contains(&"--cpu-moe".into()) && cpu_moe.contains(&"--fit".into()));
        assert_eq!(moe.slots, 1, "experts in RAM run one slot");
        assert_eq!(arg_after(&cpu_moe, "--ctx-size"), 4096);
        let ngl = cpu_moe.iter().position(|a| a == "--n-gpu-layers").unwrap();
        assert_eq!(cpu_moe[ngl + 1], "auto");

        let split = Plan::new(20, 4096, 1, false, false).with_split(Some("4096,2048".into()));
        let s = launch(&split);
        let i = s.iter().position(|a| a == "--tensor-split").unwrap();
        assert_eq!(s[i + 1], "4096,2048");
        let auto = Plan::new(-1, 4096, 1, false, false).with_split(Some("1,1".into()));
        assert!(
            !launch(&auto).contains(&"--tensor-split".into()),
            "llama.cpp splits a fitted model itself"
        );
    }

    #[test]
    fn the_engines_own_numbers_are_read_and_compared_with_the_plan() {
        let props = json!({"default_generation_settings":{"id":0,"n_ctx":4096,"params":{}},
            "total_slots":2,"model_path":"m.gguf"});
        assert_eq!(context_from_props(Some(&props)), (4096, 2));
        assert_eq!(
            context_from_props(Some(&json!({}))),
            (-1, -1),
            "an engine that says nothing"
        );
        assert_eq!(context_from_props(None), (-1, -1));

        let plan = Plan::new(15, 4096, 2, true, true);
        assert_eq!(context_mismatch(&plan, 4096, 2), None, "agreement");
        assert_eq!(
            context_mismatch(&plan, -1, -1),
            None,
            "nothing reported is not a mismatch"
        );
        let m = context_mismatch(&plan, 2048, 2).unwrap();
        assert!(m.contains("2048") && m.contains("4096"), "{m}");
        let s = context_mismatch(&plan, 4096, 1).unwrap();
        assert!(s.contains("1 slots") && s.contains("said 2"), "{s}");
        assert!(
            context_mismatch(&plan, 2048, 1).unwrap().contains("; "),
            "both differences, one sentence"
        );
    }

    #[test]
    fn the_key_stays_out_of_the_log_and_doubles_read_as_in_java() {
        let cmd = launch(&Plan::new(0, 512, 1, false, false));
        let shown = redacted(&cmd);
        let i = shown.iter().position(|a| a == "--api-key").unwrap();
        assert_eq!(shown[i + 1], "***");
        assert_eq!(random_key().len(), 48);
        assert_ne!(random_key(), random_key());
        assert_eq!(java_double(6.0), "6.0");
        assert_eq!(java_double(23.5), "23.5");
        assert_eq!(java_double(0.2), "0.2");
    }

    #[test]
    fn the_log_tail_is_the_last_lines() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x.log");
        assert_eq!(log_tail(&f, 3), "", "no log yet");
        std::fs::write(&f, "one\r\ntwo\nthree\rfour\n").unwrap();
        assert_eq!(log_tail(&f, 2), "three\nfour");
        assert_eq!(log_tail(&f, 10), "one\ntwo\nthree\nfour");
    }

    /// A fake engine for the tests: a batch file that prints its arguments, appends them to
    /// `args.txt` beside itself, and stays up for two minutes (or exits with `exit_code`), while
    /// the HTTP side is served in the test process on the port the engine was given (see
    /// [`fake_llama_server`]).
    #[cfg(windows)]
    pub(crate) fn fake_engine(dir: &Path, name: &str, exit_code: Option<i32>) -> PathBuf {
        match exit_code {
            Some(code) => fake_engine_saying(dir, name, &["loading failed"], code),
            None => fake_engine_script(dir, name, "ping -n 120 127.0.0.1 >nul\r\n"),
        }
    }

    /// A fake engine that prints `lines` (a llama.cpp fatal line, say) and exits with `code`.
    #[cfg(windows)]
    pub(crate) fn fake_engine_saying(dir: &Path, name: &str, lines: &[&str], code: i32) -> PathBuf {
        let said: String = lines.iter().map(|l| format!("echo {l}\r\n")).collect();
        fake_engine_script(dir, name, &format!("{said}exit /b {code}\r\n"))
    }

    /// The redirection goes before the `echo`: after it, arguments ending in a digit (`-t 2`,
    /// as a four-core CI runner's speech engine is given) would be read as `2>>`, the error
    /// stream's redirection, and never reach `args.txt`.
    #[cfg(windows)]
    fn fake_engine_script(dir: &Path, name: &str, tail: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!("@echo off\r\necho fake engine %*\r\n>>\"%~dp0args.txt\" echo %*\r\n{tail}"),
        )
        .unwrap();
        path
    }

    #[cfg(windows)]
    #[test]
    fn a_fake_engine_keeps_arguments_that_end_in_a_digit() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_engine_saying(dir.path(), "whisper-server.cmd", &[], 0);
        let status = std::process::Command::new(&exe)
            .args(["--port", "49452", "-t", "2"])
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
        assert_eq!(args.trim(), "--port 49452 -t 2");
    }

    /// What llama.cpp b10752 wrote when asked to load the DeepSeek V4 vision encoder (2026-09-26).
    pub(crate) const UNKNOWN_ARCHITECTURE: &[&str] = &[
        "0.00.282.050 I srv    load_model: loading model 'C:\\models\\DeepSeek-V4-Flash-Vision-Encoder.gguf'",
        "0.00.284.297 E llama_model_load: error loading model: unknown model architecture: 'deepseek4-vision'",
        "0.00.284.303 E llama_model_load_from_file_impl: failed to load model",
        "0.00.284.349 E common_fit_params: encountered an error while trying to fit params to free device memory: failed to load model",
        "0.00.285.400 E srv    load_model: failed to load model, 'C:\\models\\DeepSeek-V4-Flash-Vision-Encoder.gguf'",
        "0.00.287.377 E srv  llama_server: exiting due to model loading error",
    ];

    #[test]
    fn the_log_says_why_a_model_did_not_load() {
        assert_eq!(
            load_failure(&UNKNOWN_ARCHITECTURE.join("\n")),
            Some(LoadFailure::UnknownArchitecture("deepseek4-vision".into()))
        );
        let hparams = "llama_model_load: error loading model: error loading model hyperparameters: key not found in model: clip.context_length\nllama_model_load_from_file_impl: failed to load model";
        assert_eq!(
            load_failure(hparams),
            Some(LoadFailure::Refused(Some(
                "error loading model hyperparameters: key not found in model: clip.context_length"
                    .into()
            )))
        );
        // Qwen's prediction heads, which b10752 knows the architecture of (2026-09-26).
        let heads = "0.00.672.118 E llama_model_load: error loading model: check_tensor_dims: tensor 'token_embd.weight' not found\n0.00.672.126 E llama_model_load_from_file_impl: failed to load model";
        let f = load_failure(heads).unwrap();
        assert_eq!(
            f,
            LoadFailure::Refused(Some(
                "check_tensor_dims: tensor 'token_embd.weight' not found".into()
            ))
        );
        assert!(f.permanent());
        for oom in [
            "ggml_backend_cuda_buffer_type_alloc_buffer: allocating 12345.67 MiB on device 0: cudaMalloc failed: out of memory\nalloc_tensor_range: failed to allocate CUDA0 buffer of size 12945887744\nllama_model_load: error loading model: unable to allocate CUDA0 buffer",
            "llama_init_from_model: failed to initialize the context: failed to allocate compute pp buffers",
            "ggml_vulkan: Device memory allocation of size 4294967296 failed.\nvk::Device::allocateMemory: ErrorOutOfDeviceMemory",
        ] {
            let f = load_failure(oom);
            assert_eq!(f, Some(LoadFailure::OutOfMemory), "{oom}");
            assert!(!f.unwrap().permanent(), "memory can come free");
        }
        let bare = load_failure("srv    load_model: failed to load model, 'x.gguf'").unwrap();
        assert_eq!(bare, LoadFailure::Refused(None));
        assert!(!bare.permanent(), "no reason: it may have been anything");
        assert_eq!(
            load_failure("main: server is listening on http://127.0.0.1:1\n"),
            None
        );
        assert_eq!(load_failure(""), None);
    }

    #[test]
    fn only_the_latest_start_of_a_log_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("m.log");
        assert_eq!(start_log(&f), "", "no log yet");
        std::fs::write(
            &f,
            "\n=== 2026-09-25T21:52:55Z starting llama-server.exe --model a.gguf\nllama_model_load: error loading model: unknown model architecture: 'old'\n\n=== 2026-09-26T09:00:00Z starting llama-server.exe --model b.gguf\nmain: loading model\n",
        )
        .unwrap();
        let latest = start_log(&f);
        assert!(latest.starts_with("=== 2026-09-26T09:00:00Z"), "{latest}");
        assert_eq!(
            load_failure(&latest),
            None,
            "the earlier start's failure is not this one's"
        );
    }

    /// What a fake llama-server answers: `/health`, `/props` with the given context and slots,
    /// `/completion` with timings, and a chat completion. Checks the bearer key.
    pub(crate) fn fake_llama_router(api_key: Option<String>, n_ctx: i32, slots: i32) -> Router {
        let authorized = move |headers: &HeaderMap| match &api_key {
            None => true,
            Some(k) => headers
                .get("authorization")
                .and_then(|h| h.to_str().ok())
                .is_some_and(|h| h == format!("Bearer {k}")),
        };
        let a1 = authorized.clone();
        let a2 = authorized.clone();
        Router::new()
            .route("/health", get(|| async { Json(json!({"status": "ok"})) }))
            .route(
                "/props",
                get(move |headers: HeaderMap| {
                    let ok = a1(&headers);
                    async move {
                        if !ok {
                            return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({})));
                        }
                        (
                            axum::http::StatusCode::OK,
                            Json(json!({"default_generation_settings": {"n_ctx": n_ctx}, "total_slots": slots})),
                        )
                    }
                }),
            )
            .route(
                "/completion",
                axum::routing::post(move |headers: HeaderMap| {
                    let ok = a2(&headers);
                    async move {
                        if !ok {
                            return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({})));
                        }
                        (
                            axum::http::StatusCode::OK,
                            Json(json!({"content": "…", "timings": {"prompt_n": 512, "prompt_per_second": 812.26,
                                "predicted_n": 300, "predicted_per_second": 31.04}})),
                        )
                    }
                }),
            )
    }

    /// Serves [`fake_llama_router`] on `port` (the one an engine was given).
    pub(crate) async fn fake_llama_server(
        port: u16,
        api_key: Option<String>,
        n_ctx: i32,
        slots: i32,
    ) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap();
        let router = fake_llama_router(api_key, n_ctx, slots);
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn starts_on_a_loopback_port_reads_its_numbers_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_engine(&dir.path().join("bin"), "llama-server.cmd", None);
        let plan = Plan::new(999, 4096, 2, true, true);
        let engine = EngineProcess::new(
            "qwen3-8b",
            &dir.path().join("m.gguf"),
            Backend::Cuda,
            &exe,
            &dir.path().join("logs"),
            plan,
        )
        .unwrap();
        assert_eq!(engine.state(), State::Stopped);
        assert!(!engine.is_alive());
        assert_eq!(engine.engine_ctx_per_slot(), -1);
        fake_llama_server(engine.port(), Some(engine.api_key.clone()), 2048, 2).await;

        engine.start(Duration::from_secs(20)).await.unwrap();
        assert_eq!(engine.state(), State::Ready);
        assert!(engine.is_alive());
        assert!(engine.started_at().is_some());
        assert_eq!(
            engine.engine_ctx_per_slot(),
            2048,
            "read from /props with the key"
        );
        assert_eq!(engine.engine_slots(), 2);
        let mismatch = engine.context_mismatch().unwrap();
        assert!(
            mismatch.contains("2048") && mismatch.contains("4096"),
            "{mismatch}"
        );
        engine.start(Duration::from_secs(1)).await.unwrap(); // already up: nothing to do

        engine.begin_request();
        assert_eq!(engine.in_flight(), 1);
        engine.end_request();
        assert_eq!(engine.in_flight(), 0);

        let log = std::fs::read_to_string(engine.log_file()).unwrap();
        assert!(log.contains("starting"), "{log}");
        assert!(
            log.contains("--api-key ***"),
            "the key is not logged: {log}"
        );
        assert!(log.contains("--ctx-size 8192"), "{log}");
        assert_eq!(
            engine.log_file(),
            dir.path().join("logs").join("qwen3-8b.log")
        );

        engine.stop().await;
        assert_eq!(engine.state(), State::Stopped);
        assert!(!engine.is_alive());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn an_engine_that_exits_fails_with_its_log() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_engine(dir.path(), "llama-server.cmd", Some(3));
        let engine = EngineProcess::new(
            "m",
            &dir.path().join("m.gguf"),
            Backend::Cpu,
            &exe,
            dir.path(),
            Plan::new(0, 512, 1, false, false),
        )
        .unwrap();
        let err = engine
            .start(Duration::from_secs(20))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("Engine exited with code 3. "), "{err}");
        assert!(err.contains("loading failed"), "the log's tail: {err}");
        assert_eq!(engine.state(), State::Failed);
        assert_eq!(engine.failure().as_deref(), Some(err.as_str()));
    }

    /// An engine that prints llama.cpp's fatal line and exits fails with the reason read from its
    /// log, beside the log's tail.
    #[cfg(windows)]
    #[tokio::test]
    async fn an_engine_that_exits_says_why_from_its_log() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_engine_saying(dir.path(), "llama-server.cmd", UNKNOWN_ARCHITECTURE, 1);
        let engine = EngineProcess::new(
            "encoder",
            &dir.path().join("DeepSeek-V4-Flash-Vision-Encoder.gguf"),
            Backend::Cuda,
            &exe,
            &dir.path().join("logs"),
            Plan::new(999, 4096, 1, false, false),
        )
        .unwrap();
        // An earlier start of another build that failed otherwise is not this start's reason.
        append_log(
            engine.log_file(),
            "\n=== 2026-09-25T10:00:00Z starting\nllama_model_load: error loading model: missing tensor 'x'\n",
        );
        let err = engine.start(Duration::from_secs(20)).await.unwrap_err();
        let exited = err.downcast_ref::<EngineExited>().expect("an EngineExited");
        assert_eq!(exited.code, 1);
        assert_eq!(
            exited.cause,
            Some(LoadFailure::UnknownArchitecture("deepseek4-vision".into()))
        );
        assert!(
            err.to_string().starts_with("Engine exited with code 1. "),
            "{err}"
        );
        assert_eq!(engine.state(), State::Failed);
        assert_eq!(engine.failure(), Some(err.to_string()));
    }

    #[tokio::test]
    async fn a_missing_binary_fails_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("llama-server.exe");
        let engine = EngineProcess::new(
            "m",
            &dir.path().join("m.gguf"),
            Backend::Cpu,
            &exe,
            dir.path(),
            Plan::new(0, 512, 1, false, false),
        )
        .unwrap();
        let err = engine
            .start(Duration::from_secs(5))
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, format!("Engine binary missing: {}", exe.display()));
        assert_eq!(engine.state(), State::Failed);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn an_engine_that_never_answers_is_stopped_after_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_engine(dir.path(), "llama-server.cmd", None);
        let engine = EngineProcess::new(
            "m",
            &dir.path().join("m.gguf"),
            Backend::Cpu,
            &exe,
            dir.path(),
            Plan::new(0, 512, 1, false, false),
        )
        .unwrap();
        let err = engine
            .start(Duration::from_secs(1))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("Engine did not become healthy within 1 s. "),
            "{err}"
        );
        assert!(err.contains("fake engine"), "the log's tail: {err}");
        assert_eq!(engine.state(), State::Failed);
        assert!(!engine.is_alive(), "stopped");
    }
}

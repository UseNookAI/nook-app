//! Anonymous usage statistics: how often each tool is used and how often it fails, sent once a
//! day so we can see which tools people rely on and which ones let them down. On unless the person
//! turns it off (Settings › General › Usage statistics), and nothing is sent before the notice
//! that says so has been on screen (`USAGE_NOTICE_SHOWN`).
//!
//! A report holds this and nothing else ([`Report`]): a random install number made on the first
//! start, the app's version, Windows or macOS, the update channel, the graphics card's maker and
//! memory, and for each tool the uses and failures since the last report. Tools are a fixed list
//! ([`Tool`]); no file, text, prompt, path, model name or error message is ever in a report.
//!
//! Counts wait in `<home>\data\usage.json` until a report carrying them has been accepted, so a day
//! offline loses nothing. Turning the reports off drops what was waiting and counts nothing more.
//!
//! Where reports go: the `NOOK_RS_USAGE_URL` environment variable (a web address, or `off`), else
//! the host the build was stamped with (`NOOK_RS_USAGE_BASE`, which tools/publish.ps1 sets for the
//! builds it publishes to the download host). A build from source, or for a test feed, sends nothing.
//!
//! Most tools are counted by the command that starts them ([`Usage::used`], [`Usage::outcome`]).
//! Flow runs, conversions and video clips start from several commands and fail long after they
//! started, so those are counted from the event bus instead: a run counts when it is first seen
//! queued or going, and as a failure when it then ends failed.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

use crate::build_info::BuildInfo;
use crate::events::{self, topic};
use crate::flow::service::{READ_ALOUD, SUMMARIZE, TRANSCRIBE, TRANSLATE_AUDIO};
use crate::runtime::{GpuDevice, GpuInventory};
use crate::settings::{
    strip_bom, write_atomic, Settings, SHARE_USAGE, UPDATE_CHANNEL, USAGE_NOTICE_SHOWN,
};
use crate::Home;

/// The host stamped into the build, if any; reports go to `<base>/v1/ping`.
pub const STAMPED_BASE: Option<&str> = option_env!("NOOK_RS_USAGE_BASE");
/// Environment variable that points reports elsewhere, or turns them off with `off`.
pub const USAGE_URL_ENV: &str = "NOOK_RS_USAGE_URL";
/// The first report waits this long after start, so it never slows the window down.
const FIRST_DELAY: Duration = Duration::from_secs(120);
/// How often the app looks whether a report is due.
const CHECK_EVERY: Duration = Duration::from_secs(3600);
/// One report a day.
const INTERVAL: chrono::TimeDelta = chrono::TimeDelta::hours(24);
const TIMEOUT: Duration = Duration::from_secs(15);
/// No count goes above this; the server refuses larger ones.
const MAX_COUNT: u32 = 100_000;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// What is counted. The keys are the only names a report can carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    /// A request typed into the Nooklet finder.
    FinderSearch,
    /// A speech translation (the translate flow).
    Translate,
    Transcribe,
    Summarize,
    ReadAloud,
    /// A conversion (one job, however many files).
    Convert,
    /// A PDF opened in the editor.
    PdfOpen,
    /// An edited PDF saved.
    PdfSave,
    /// A screen recording started.
    ScreenRecord,
    /// A live stream started.
    ScreenStream,
    /// A Code session started.
    CodeSession,
    /// A request sent to the Code worker in a session.
    CodeTurn,
    /// The worker's changes applied to the repository.
    CodeApply,
    /// A video clip queued.
    VideoClip,
    /// A spoken prompt (the microphone button).
    VoicePrompt,
    /// A model download started.
    ModelDownload,
}

impl Tool {
    pub const ALL: [Tool; 16] = [
        Tool::FinderSearch,
        Tool::Translate,
        Tool::Transcribe,
        Tool::Summarize,
        Tool::ReadAloud,
        Tool::Convert,
        Tool::PdfOpen,
        Tool::PdfSave,
        Tool::ScreenRecord,
        Tool::ScreenStream,
        Tool::CodeSession,
        Tool::CodeTurn,
        Tool::CodeApply,
        Tool::VideoClip,
        Tool::VoicePrompt,
        Tool::ModelDownload,
    ];

    /// `<tool>.<action>`, as the report and the admin panel name it.
    pub fn key(self) -> &'static str {
        match self {
            Tool::FinderSearch => "finder.search",
            Tool::Translate => "translate.run",
            Tool::Transcribe => "transcribe.run",
            Tool::Summarize => "summarize.run",
            Tool::ReadAloud => "read-aloud.run",
            Tool::Convert => "convert.run",
            Tool::PdfOpen => "pdf.open",
            Tool::PdfSave => "pdf.save",
            Tool::ScreenRecord => "screen.record",
            Tool::ScreenStream => "screen.stream",
            Tool::CodeSession => "code.session",
            Tool::CodeTurn => "code.turn",
            Tool::CodeApply => "code.apply",
            Tool::VideoClip => "video.clip",
            Tool::VoicePrompt => "voice.prompt",
            Tool::ModelDownload => "models.download",
        }
    }

    fn of_key(key: &str) -> Option<Tool> {
        Tool::ALL.into_iter().find(|t| t.key() == key)
    }

    /// The tool a flow run belongs to; None for a flow this list does not know.
    fn of_flow(flow: &str) -> Option<Tool> {
        match flow {
            TRANSLATE_AUDIO => Some(Tool::Translate),
            TRANSCRIBE => Some(Tool::Transcribe),
            SUMMARIZE => Some(Tool::Summarize),
            READ_ALOUD => Some(Tool::ReadAloud),
            _ => None,
        }
    }
}

/// One tool's uses since the last report, and how many of them failed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Count {
    pub uses: u32,
    pub failed: u32,
}

/// The graphics card the models run on: its maker and memory, nothing that names the card.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Gpu {
    /// nvidia, amd, intel, apple, other or none.
    pub vendor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_gb: Option<f64>,
}

/// One report, exactly as it is sent.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub install: String,
    pub version: String,
    pub os: String,
    pub channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu: Option<Gpu>,
    pub tools: BTreeMap<String, Count>,
}

/// For Settings: whether reports are on, whether this build sends them at all, when the last one
/// went, and the next one as it stands.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    pub enabled: bool,
    pub sends: bool,
    pub last_sent: Option<DateTime<Utc>>,
    pub next: Report,
}

/// What [`Usage::send_now`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sent {
    Sent,
    /// The server refused it as malformed; its counts were dropped all the same.
    Refused,
    /// Nothing went out, and why: "off", "no address", "notice not shown".
    Skipped(&'static str),
}

/// What usage.json keeps.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Stored {
    install: String,
    counts: BTreeMap<String, Count>,
    last_sent: Option<DateTime<Utc>>,
}

pub struct Usage {
    file: PathBuf,
    settings: Arc<Settings>,
    endpoint: Option<String>,
    client: reqwest::Client,
    stored: Mutex<Stored>,
    /// Runs seen while they were going, by id, with their tool: each counts once when first
    /// seen going and once more if it then fails.
    live: Mutex<HashMap<String, Tool>>,
    inventory: Mutex<Option<Arc<GpuInventory>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Usage {
    /// The app's statistics: usage.json in the home's data folder, the address from the
    /// environment or the build. Nothing is sent until [`Usage::start`].
    pub fn new(home: &Home, settings: Arc<Settings>) -> Arc<Usage> {
        let env = std::env::var(USAGE_URL_ENV).ok();
        let endpoint = choose_endpoint(STAMPED_BASE, env.as_deref());
        Usage::open(home.data_dir().join("usage.json"), settings, endpoint)
    }

    pub fn open(file: PathBuf, settings: Arc<Settings>, endpoint: Option<String>) -> Arc<Usage> {
        let stored = load(&file);
        let usage = Usage {
            file,
            settings,
            endpoint,
            client: client(),
            stored: Mutex::new(stored),
            live: Mutex::new(HashMap::new()),
            inventory: Mutex::new(None),
            tasks: Mutex::new(Vec::new()),
        };
        usage.save();
        Arc::new(usage)
    }

    /// Whether reports are on (the person's switch).
    pub fn enabled(&self) -> bool {
        self.settings.get_bool(SHARE_USAGE)
    }

    /// A use of `tool`.
    pub fn used(&self, tool: Tool) {
        self.add(tool, 1, 0);
    }

    /// A use of `tool` that ended as `result` says: an error counts as a failed use.
    pub fn outcome<T, E>(&self, tool: Tool, result: &Result<T, E>) {
        self.add(tool, 1, u32::from(result.is_err()));
    }

    fn add(&self, tool: Tool, uses: u32, failed: u32) {
        if !self.enabled() {
            return;
        }
        {
            let mut stored = self.stored.lock();
            let count = stored.counts.entry(tool.key().to_string()).or_default();
            count.uses = count.uses.saturating_add(uses).min(MAX_COUNT);
            count.failed = count.failed.saturating_add(failed).min(count.uses);
        }
        self.save();
    }

    /// Turns the reports on or off; off drops the counts that were waiting.
    pub fn set_enabled(&self, on: bool) -> Result<()> {
        self.settings.set(SHARE_USAGE, on.to_string())?;
        if !on {
            self.stored.lock().counts.clear();
            self.live.lock().clear();
            self.save();
        }
        Ok(())
    }

    /// The notice has been on screen: reports may go from now on.
    pub fn notice_seen(&self) -> Result<()> {
        self.settings.set(USAGE_NOTICE_SHOWN, "true")
    }

    /// Erase everything: a new install number, no counts, nothing sent yet.
    pub fn reset(&self) {
        *self.stored.lock() = Stored {
            install: new_install(),
            ..Stored::default()
        };
        self.live.lock().clear();
        self.save();
    }

    /// The next report as it stands, with the graphics card when the inventory is known.
    pub async fn report(&self) -> Report {
        let inventory = self.inventory.lock().clone();
        let gpu = match inventory {
            Some(inventory) => Some(gpu_of(&inventory.snapshot().await.devices)),
            None => None,
        };
        let stored = self.stored.lock();
        let channel = match self.settings.get(UPDATE_CHANNEL).as_deref() {
            Some("dev") => "dev",
            _ => "stable",
        };
        Report {
            install: stored.install.clone(),
            version: BuildInfo::current().version,
            os: os_name().to_string(),
            channel: channel.to_string(),
            gpu,
            tools: stored.counts.clone(),
        }
    }

    pub async fn overview(&self) -> Overview {
        let last_sent = self.stored.lock().last_sent;
        Overview {
            enabled: self.enabled(),
            sends: self.endpoint.is_some(),
            last_sent,
            next: self.report().await,
        }
    }

    /// Sends the report now when it may go. Once the server has it, the counts it carried are
    /// taken off (uses made while it was on its way stay for the next one). A report the server
    /// refuses as malformed is dropped rather than tried forever; a server that is down or busy,
    /// or no network, leaves everything for the next try.
    pub async fn send_now(&self) -> Result<Sent> {
        let Some(endpoint) = self.endpoint.clone() else {
            return Ok(Sent::Skipped("no address"));
        };
        if !self.enabled() {
            return Ok(Sent::Skipped("off"));
        }
        if !self.settings.get_bool(USAGE_NOTICE_SHOWN) {
            return Ok(Sent::Skipped("notice not shown"));
        }
        let report = self.report().await;
        let response = self
            .client
            .post(&endpoint)
            .json(&report)
            .send()
            .await
            .context("The usage report did not reach the server")?;
        let status = response.status();
        let refused = status.is_client_error() && status != reqwest::StatusCode::TOO_MANY_REQUESTS;
        if !status.is_success() && !refused {
            bail!("The server did not take the usage report (HTTP {status})");
        }
        if refused {
            let why = response.text().await.unwrap_or_default();
            tracing::warn!(
                "The server refused the usage report (HTTP {status}: {why}); it is dropped"
            );
        }
        {
            let mut stored = self.stored.lock();
            // Erase everything while it was on its way: the counts it carried are gone already.
            if stored.install == report.install {
                for (key, sent) in &report.tools {
                    if let Some(count) = stored.counts.get_mut(key) {
                        count.uses = count.uses.saturating_sub(sent.uses);
                        count.failed = count.failed.saturating_sub(sent.failed).min(count.uses);
                        if count.uses == 0 {
                            stored.counts.remove(key);
                        }
                    }
                }
                stored.last_sent = Some(Utc::now());
            }
        }
        self.save();
        Ok(if refused { Sent::Refused } else { Sent::Sent })
    }

    /// Whether a day has passed since the last report (or the clock went back past it).
    fn due(&self, now: DateTime<Utc>) -> bool {
        match self.stored.lock().last_sent {
            None => true,
            Some(last) => last > now || now - last >= INTERVAL,
        }
    }

    /// Starts counting runs from the event bus and sending the daily report.
    pub fn start(self: &Arc<Self>, inventory: Arc<GpuInventory>) {
        *self.inventory.lock() = Some(inventory);
        let mut tasks = self.tasks.lock();
        if !tasks.is_empty() {
            return;
        }
        let watcher = self.clone();
        tasks.push(tokio::spawn(async move { watcher.watch().await }));
        if self.endpoint.is_none() {
            return;
        }
        let sender = self.clone();
        tasks.push(tokio::spawn(async move {
            tokio::time::sleep(FIRST_DELAY).await;
            loop {
                if sender.due(Utc::now()) {
                    match sender.send_now().await {
                        Ok(Sent::Sent) => tracing::info!("Usage report sent"),
                        // Refused: logged where it happened.
                        Ok(Sent::Refused | Sent::Skipped(_)) => {}
                        // Tried again within the hour.
                        Err(e) => tracing::info!("{e:#}"),
                    }
                }
                tokio::time::sleep(CHECK_EVERY).await;
            }
        }));
    }

    pub fn shutdown(&self) {
        for task in self.tasks.lock().drain(..) {
            task.abort();
        }
    }

    async fn watch(&self) {
        let mut events = events::subscribe();
        loop {
            match events.recv().await {
                Ok(event) => self.observe(&event.topic, &event.payload),
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    }

    /// Counts the runs in one event: flow runs (`{"run": Run}`), conversions (`{"job": Job}`) and
    /// video clips (the whole list).
    fn observe(&self, topic: &str, payload: &Value) {
        match topic {
            topic::FLOWS => {
                if let Some(run) = payload.get("run") {
                    if let Some(tool) = run
                        .get("flow")
                        .and_then(Value::as_str)
                        .and_then(Tool::of_flow)
                    {
                        self.track(run, tool);
                    }
                }
            }
            topic::CONVERT => {
                if let Some(job) = payload.get("job") {
                    self.track(job, Tool::Convert);
                }
            }
            topic::VIDEO => {
                for clip in payload.as_array().into_iter().flatten() {
                    self.track(clip, Tool::VideoClip);
                }
            }
            _ => {}
        }
    }

    /// One run's state: a use the first time it is seen queued or going, a failure when one that
    /// was seen going ends failed. Runs that were over before Nook saw them count for nothing.
    fn track(&self, run: &Value, tool: Tool) {
        let (Some(id), Some(status)) = (
            run.get("id").and_then(Value::as_str),
            run.get("status").and_then(Value::as_str),
        ) else {
            return;
        };
        let going = matches!(status, "QUEUED" | "RUNNING" | "WAITING" | "CONVERTING");
        let (uses, failed) = {
            let mut live = self.live.lock();
            if going {
                if live.contains_key(id) {
                    return;
                }
                // A run that never ended (deleted while it went) is not kept forever.
                if live.len() >= 512 {
                    live.clear();
                }
                live.insert(id.to_string(), tool);
                (1, 0)
            } else {
                match live.remove(id) {
                    Some(_) if status == "FAILED" => (0, 1),
                    _ => return,
                }
            }
        };
        self.add(tool, uses, failed);
    }

    fn save(&self) {
        let json = match serde_json::to_vec_pretty(&*self.stored.lock()) {
            Ok(json) => json,
            Err(e) => return tracing::warn!("Could not write the usage counts: {e}"),
        };
        if let Err(e) = write_atomic(&self.file, &json) {
            tracing::warn!("Could not write the usage counts: {e:#}");
        }
    }
}

/// Where reports go: the environment's address (`off` or blank for nowhere), else the stamped
/// host's ping address, else nowhere.
pub fn choose_endpoint(stamped: Option<&str>, env: Option<&str>) -> Option<String> {
    if let Some(env) = env {
        let env = env.trim();
        return (!env.is_empty() && !env.eq_ignore_ascii_case("off")).then(|| env.to_string());
    }
    let base = stamped?.trim().trim_end_matches('/');
    (!base.is_empty()).then(|| format!("{base}/v1/ping"))
}

/// usage.json as it was left; a missing or unreadable one starts over with a new install number.
/// Names that are not tools (from another version) are dropped.
fn load(file: &Path) -> Stored {
    let mut stored: Stored = std::fs::read(file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(strip_bom(&bytes)).ok())
        .unwrap_or_default();
    if uuid::Uuid::parse_str(&stored.install).is_err() {
        stored.install = new_install();
    }
    stored.counts.retain(|key, count| {
        count.uses = count.uses.min(MAX_COUNT);
        count.failed = count.failed.min(count.uses);
        Tool::of_key(key).is_some() && count.uses > 0
    });
    stored
}

fn new_install() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn client() -> reqwest::Client {
    let builder = reqwest::Client::builder()
        .connect_timeout(TIMEOUT)
        .timeout(TIMEOUT)
        .user_agent(format!("Nook/{}", BuildInfo::current().version));
    #[cfg(test)]
    let builder = builder.no_proxy();
    builder.build().unwrap_or_default()
}

fn os_name() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        _ => "other",
    }
}

/// The card with the most memory (a dedicated one before an integrated one).
fn gpu_of(devices: &[GpuDevice]) -> Gpu {
    let Some(card) = devices
        .iter()
        .max_by_key(|d| (!d.integrated, d.total_bytes))
    else {
        return Gpu {
            vendor: "none".to_string(),
            vram_gb: None,
        };
    };
    let vendor = match card.vendor.as_str() {
        v @ ("nvidia" | "amd" | "intel" | "apple") => v,
        _ => "other",
    };
    Gpu {
        vendor: vendor.to_string(),
        vram_gb: (card.total_bytes > 0)
            .then(|| (card.total_bytes as f64 / GIB * 10.0).round() / 10.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn usage_in(dir: &Path, endpoint: Option<String>) -> Arc<Usage> {
        let settings = Arc::new(Settings::load(dir.join("settings.json")).unwrap());
        Usage::open(dir.join("usage.json"), settings, endpoint)
    }

    fn uses(usage: &Usage, tool: Tool) -> Count {
        usage
            .stored
            .lock()
            .counts
            .get(tool.key())
            .copied()
            .unwrap_or_default()
    }

    /// A server that answers one request with `status` and hands back the body it was sent.
    async fn serve_once(status: &'static str) -> (String, JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/ping", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = socket.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().to_string())
                        })
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + length || n == 0 {
                        let reply = format!(
                            "HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        );
                        socket.write_all(reply.as_bytes()).await.unwrap();
                        return text[end + 4..].to_string();
                    }
                }
            }
        });
        (url, task)
    }

    #[test]
    fn counts_and_the_install_number_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let first = usage_in(dir.path(), None);
        first.used(Tool::PdfOpen);
        first.outcome(Tool::PdfSave, &Err::<(), _>("disk full"));
        first.outcome(Tool::PdfSave, &Ok::<_, ()>(()));
        let install = first.stored.lock().install.clone();
        assert!(uuid::Uuid::parse_str(&install).is_ok());

        let again = usage_in(dir.path(), None);
        assert_eq!(again.stored.lock().install, install);
        assert_eq!(uses(&again, Tool::PdfOpen), Count { uses: 1, failed: 0 });
        assert_eq!(uses(&again, Tool::PdfSave), Count { uses: 2, failed: 1 });
    }

    #[test]
    fn names_that_are_not_tools_are_dropped_on_load() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("usage.json"),
            br#"{"install":"not a uuid","counts":{"pdf.open":{"uses":2,"failed":5},"C:\\Users\\x":{"uses":1,"failed":0}}}"#,
        )
        .unwrap();
        let usage = usage_in(dir.path(), None);
        let stored = usage.stored.lock();
        assert!(uuid::Uuid::parse_str(&stored.install).is_ok());
        assert_eq!(stored.counts.len(), 1);
        assert_eq!(stored.counts["pdf.open"], Count { uses: 2, failed: 2 });
    }

    #[test]
    fn turned_off_it_drops_what_waited_and_counts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let usage = usage_in(dir.path(), None);
        usage.used(Tool::Convert);
        usage.set_enabled(false).unwrap();
        usage.used(Tool::Convert);
        assert!(usage.stored.lock().counts.is_empty());
        usage.set_enabled(true).unwrap();
        usage.used(Tool::Convert);
        assert_eq!(uses(&usage, Tool::Convert).uses, 1);
    }

    #[test]
    fn a_run_counts_once_when_it_goes_and_again_when_it_fails() {
        let dir = tempfile::tempdir().unwrap();
        let usage = usage_in(dir.path(), None);
        let run = |id: &str, status: &str| json!({ "run": { "id": id, "flow": TRANSCRIBE, "status": status } });
        usage.observe(topic::FLOWS, &run("a", "QUEUED"));
        usage.observe(topic::FLOWS, &run("a", "RUNNING"));
        usage.observe(topic::FLOWS, &run("a", "FAILED"));
        usage.observe(topic::FLOWS, &run("b", "RUNNING"));
        usage.observe(topic::FLOWS, &run("b", "DONE"));
        // Over before Nook saw it go (an old run in a list): nothing.
        usage.observe(topic::FLOWS, &run("c", "FAILED"));
        assert_eq!(uses(&usage, Tool::Transcribe), Count { uses: 2, failed: 1 });

        usage.observe(
            topic::CONVERT,
            &json!({ "job": { "id": "j", "status": "CONVERTING" } }),
        );
        usage.observe(
            topic::CONVERT,
            &json!({ "job": { "id": "j", "status": "STOPPED" } }),
        );
        assert_eq!(uses(&usage, Tool::Convert), Count { uses: 1, failed: 0 });

        let clips = |status: &str| json!([{ "id": "old", "status": "DONE" }, { "id": "v", "status": status }]);
        usage.observe(topic::VIDEO, &clips("QUEUED"));
        usage.observe(topic::VIDEO, &clips("RUNNING"));
        usage.observe(topic::VIDEO, &clips("FAILED"));
        assert_eq!(uses(&usage, Tool::VideoClip), Count { uses: 1, failed: 1 });

        // A flow this list does not know is not counted under any name.
        usage.observe(
            topic::FLOWS,
            &json!({ "run": { "id": "x", "flow": "something-new", "status": "RUNNING" } }),
        );
        assert_eq!(usage.stored.lock().counts.len(), 3);
    }

    #[test]
    fn the_address_comes_from_the_environment_or_the_build() {
        assert_eq!(choose_endpoint(None, None), None);
        assert_eq!(choose_endpoint(Some(""), None), None);
        assert_eq!(
            choose_endpoint(Some("https://api.usenook.ai/"), None).as_deref(),
            Some("https://api.usenook.ai/v1/ping")
        );
        assert_eq!(
            choose_endpoint(Some("https://api.usenook.ai"), Some("off")),
            None
        );
        assert_eq!(
            choose_endpoint(Some("https://api.usenook.ai"), Some(" ")),
            None
        );
        assert_eq!(
            choose_endpoint(None, Some("http://127.0.0.1:8790/v1/ping")).as_deref(),
            Some("http://127.0.0.1:8790/v1/ping")
        );
    }

    #[test]
    fn the_card_is_named_by_maker_and_memory_only() {
        let card = |name: &str, vendor: &str, gib: u64, integrated: bool| GpuDevice {
            index: 0,
            name: name.to_string(),
            total_bytes: gib * 1024 * 1024 * 1024,
            free_bytes: 0,
            driver_version: None,
            compute_capability: None,
            vendor: vendor.to_string(),
            integrated,
        };
        assert_eq!(
            gpu_of(&[]),
            Gpu {
                vendor: "none".into(),
                vram_gb: None
            }
        );
        let gpu = gpu_of(&[
            card("Intel UHD", "intel", 16, true),
            card("RTX 4060", "nvidia", 8, false),
        ]);
        assert_eq!(
            gpu,
            Gpu {
                vendor: "nvidia".into(),
                vram_gb: Some(8.0)
            }
        );
        assert_eq!(
            gpu_of(&[card("Some card", "vulkan", 4, false)]).vendor,
            "other"
        );
    }

    #[tokio::test]
    async fn a_report_goes_once_the_notice_was_seen_and_takes_its_counts_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let (url, server) = serve_once("204 No Content").await;
        let usage = usage_in(dir.path(), Some(url));
        usage.used(Tool::ScreenRecord);
        usage.outcome(Tool::FinderSearch, &Err::<(), _>(()));

        assert_eq!(
            usage.send_now().await.unwrap(),
            Sent::Skipped("notice not shown")
        );
        usage.notice_seen().unwrap();
        assert_eq!(usage.send_now().await.unwrap(), Sent::Sent);

        let body: Value = serde_json::from_str(&server.await.unwrap()).unwrap();
        let mut keys: Vec<_> = body.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["channel", "install", "os", "tools", "version"]);
        assert_eq!(
            body["tools"]["screen.record"],
            json!({ "uses": 1, "failed": 0 })
        );
        assert_eq!(
            body["tools"]["finder.search"],
            json!({ "uses": 1, "failed": 1 })
        );
        assert_eq!(body["channel"], "stable");
        assert!(usage.stored.lock().counts.is_empty());
        assert!(!usage.due(Utc::now()));
    }

    #[tokio::test]
    async fn a_server_that_is_down_leaves_the_counts_for_the_next_try() {
        let dir = tempfile::tempdir().unwrap();
        let (url, server) = serve_once("503 Service Unavailable").await;
        let usage = usage_in(dir.path(), Some(url));
        usage.notice_seen().unwrap();
        usage.used(Tool::CodeTurn);
        assert!(usage.send_now().await.is_err());
        server.await.unwrap();
        assert_eq!(uses(&usage, Tool::CodeTurn).uses, 1);
        assert!(usage.due(Utc::now()));
    }

    #[tokio::test]
    async fn a_report_the_server_refuses_is_dropped_not_tried_forever() {
        let dir = tempfile::tempdir().unwrap();
        let (url, server) = serve_once("400 Bad Request").await;
        let usage = usage_in(dir.path(), Some(url));
        usage.notice_seen().unwrap();
        usage.used(Tool::CodeApply);
        assert_eq!(usage.send_now().await.unwrap(), Sent::Refused);
        server.await.unwrap();
        assert!(usage.stored.lock().counts.is_empty());
        assert!(!usage.due(Utc::now()));
    }

    /// Live: a report to a real server, e.g. the admin panel's `node tools/dev.js 8791`, with
    /// NOOK_TEST_USAGE_URL=http://127.0.0.1:8791/v1/ping. Prints the install number to look up.
    #[tokio::test]
    #[ignore]
    async fn a_real_server_takes_the_report() {
        let url = std::env::var("NOOK_TEST_USAGE_URL").expect("set NOOK_TEST_USAGE_URL");
        let dir = tempfile::tempdir().unwrap();
        let usage = usage_in(dir.path(), Some(url));
        usage.notice_seen().unwrap();
        usage.outcome(Tool::PdfSave, &Err::<(), _>(()));
        usage.used(Tool::PdfSave);
        usage.used(Tool::Convert);
        assert_eq!(usage.send_now().await.unwrap(), Sent::Sent);
        println!("install {}", usage.stored.lock().install);
    }

    #[tokio::test]
    async fn nothing_goes_without_an_address_or_when_off() {
        let dir = tempfile::tempdir().unwrap();
        let usage = usage_in(dir.path(), None);
        usage.notice_seen().unwrap();
        assert_eq!(usage.send_now().await.unwrap(), Sent::Skipped("no address"));
        let dir = tempfile::tempdir().unwrap();
        let usage = usage_in(dir.path(), Some("http://127.0.0.1:9/v1/ping".into()));
        usage.notice_seen().unwrap();
        usage.set_enabled(false).unwrap();
        assert_eq!(usage.send_now().await.unwrap(), Sent::Skipped("off"));
    }

    #[test]
    fn erase_everything_makes_a_new_install() {
        let dir = tempfile::tempdir().unwrap();
        let usage = usage_in(dir.path(), None);
        usage.used(Tool::VoicePrompt);
        let before = usage.stored.lock().install.clone();
        usage.reset();
        assert_ne!(usage.stored.lock().install, before);
        assert!(usage.stored.lock().counts.is_empty());
    }
}

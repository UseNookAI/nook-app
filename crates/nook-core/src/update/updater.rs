//! Ports `service/VersionUpdateService.kt` (its logic; the dialog and the Settings row are UI).
//!
//! Self-update. The check asks [`UpdateSource`] for the channel's latest signed manifest at start,
//! then every minute on the dev channel or from a feed on this machine, every fifteen on stable;
//! the stable channel offers the update in a dialog, the dev channel takes it by itself once no
//! [`BusyWork`] has anything running, since the installer closes the app. The installer is
//! downloaded, its sha256 and size checked against the manifest, and only then run; the app quits,
//! the installer replaces the files and the new build starts ([`super::install`]).
//!
//! Every change goes out as an [`UpdateStatus`] on [`topic::UPDATE`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::manifest::Release;
use super::source::{self, UpdateSource, DEV, USER_AGENT};
use crate::busy::BusyWork;
use crate::events::{self, topic};

/// The first scheduled check comes this long after start (the Hub asks once it is up anyway).
pub const INITIAL_DELAY: Duration = Duration::from_secs(90);
/// The scheduled check runs this often; [`due_for_check`] keeps stable on a web host to every
/// fifteen minutes.
pub const CHECK_EVERY: Duration = Duration::from_secs(60);
/// A stable channel on a web host is asked this often, in milliseconds.
pub const STABLE_EVERY_MS: i64 = 900_000;
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(60);
const CHUNK: usize = 1 << 16;

/// What the update dialog, the top bar's badge and Settings › General › Updates read.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// False when there is nowhere to ask (a blank base): nothing is ever checked.
    pub enabled: bool,
    /// This build's version.
    pub current_version: String,
    /// "0.5.0 (396a3b4, 2026-09-22)" for the About page.
    pub build_label: String,
    /// `stable` or `dev`.
    pub channel: String,
    pub is_update_available: bool,
    /// The release on offer: version, title, notes and the rest of the manifest.
    pub latest_version_info: Option<Release>,
    /// A check is under way.
    pub checking: bool,
    pub is_downloading: bool,
    /// 0 to 1, while downloading.
    pub download_progress: f32,
    /// Why the last download or install failed ("Update failed: ...").
    pub update_error: Option<String>,
    /// What the last check found wrong, for Settings; None when the check went through or never ran.
    pub last_check_error: Option<String>,
    /// For Settings: a build the dev channel found and holds back while something is running.
    pub waiting_note: Option<String>,
    /// The person chose "Later" for the offer on screen; a new offer shows again.
    pub snoozed: bool,
}

#[derive(Default)]
struct State {
    is_update_available: bool,
    latest_version_info: Option<Release>,
    is_downloading: bool,
    download_progress: f32,
    update_error: Option<String>,
    last_check_error: Option<String>,
    waiting_note: Option<String>,
    snoozed: bool,
}

impl State {
    fn offer(&mut self, release: Release) {
        // an offer that appears where there was none shows the dialog again, whatever "Later" said
        if !self.is_update_available {
            self.snoozed = false;
        }
        self.latest_version_info = Some(release);
        self.is_update_available = true;
    }

    fn clear_offer(&mut self) {
        self.latest_version_info = None;
        self.is_update_available = false;
    }
}

type Launcher = Arc<dyn Fn(&Path) -> Result<()> + Send + Sync>;
type QuitHook = Arc<dyn Fn() + Send + Sync>;

struct Download {
    id: u64,
    cancel: CancellationToken,
}

/// Why a download ended early.
enum Stop {
    Cancelled,
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for Stop {
    fn from(e: anyhow::Error) -> Stop {
        Stop::Failed(e)
    }
}

pub struct Updater {
    source: UpdateSource,
    download_dir: PathBuf,
    busy: RwLock<Vec<(String, Arc<dyn BusyWork>)>>,
    state: Mutex<State>,
    last_check: AtomicI64,
    checks_in_flight: AtomicUsize,
    download: Mutex<Option<Download>>,
    download_ids: AtomicU64,
    launcher: RwLock<Launcher>,
    quit: RwLock<Option<QuitHook>>,
    runtime: Mutex<Option<tokio::runtime::Handle>>,
    client: reqwest::Client,
}

impl Updater {
    /// An updater over a source; installers are downloaded into `download_dir` (the app passes
    /// `Home::temp_dir()\update`; the original used the system temp folder).
    pub fn new(source: UpdateSource, download_dir: impl Into<PathBuf>) -> Arc<Updater> {
        let client = reqwest::Client::builder()
            .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
            .read_timeout(DOWNLOAD_READ_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .unwrap_or_default();
        Arc::new(Updater {
            source,
            download_dir: download_dir.into(),
            busy: RwLock::new(Vec::new()),
            state: Mutex::new(State::default()),
            last_check: AtomicI64::new(0),
            checks_in_flight: AtomicUsize::new(0),
            download: Mutex::new(None),
            download_ids: AtomicU64::new(0),
            launcher: RwLock::new(Arc::new(default_launcher)),
            quit: RwLock::new(None),
            runtime: Mutex::new(tokio::runtime::Handle::try_current().ok()),
            client,
        })
    }

    pub fn source(&self) -> &UpdateSource {
        &self.source
    }

    pub fn current_version(&self) -> String {
        self.source.build().version.clone()
    }

    /// "0.5.0 (396a3b4, 2026-09-22)" for the About page.
    pub fn build_label(&self) -> String {
        self.source.build().label()
    }

    /// Adds a long-running job an automatic (dev channel) install must wait for.
    pub fn register_busy(&self, name: &str, work: Arc<dyn BusyWork>) {
        self.busy.write().push((name.to_string(), work));
    }

    /// What the app does once the installer is started: quit (Tauri's `AppHandle::exit(0)`), so the
    /// update script can run the installer. Without a hook the process exits a second later, as
    /// the original did.
    pub fn set_quit_hook(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self.quit.write() = Some(Arc::new(hook));
    }

    /// Replaces what runs a verified installer ([`super::install::launch`]); tests record it instead.
    pub fn set_launcher(&self, launcher: impl Fn(&Path) -> Result<()> + Send + Sync + 'static) {
        *self.launcher.write() = Arc::new(launcher);
    }

    pub fn status(&self) -> UpdateStatus {
        let s = self.state.lock();
        UpdateStatus {
            enabled: self.source.enabled(),
            current_version: self.current_version(),
            build_label: self.build_label(),
            channel: self.source.channel(),
            is_update_available: s.is_update_available,
            latest_version_info: s.latest_version_info.clone(),
            checking: self.checks_in_flight.load(Ordering::SeqCst) > 0,
            is_downloading: s.is_downloading,
            download_progress: s.download_progress,
            update_error: s.update_error.clone(),
            last_check_error: s.last_check_error.clone(),
            waiting_note: s.waiting_note.clone(),
            snoozed: s.snoozed,
        }
    }

    fn emit(&self) {
        events::emit(topic::UPDATE, self.status());
    }

    fn change(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock());
        self.emit();
    }

    pub fn channel(&self) -> String {
        self.source.channel()
    }

    pub fn set_channel(self: &Arc<Self>, channel: &str) -> Result<()> {
        self.source.set_channel(channel)?;
        // what the other channel offered is not on offer here
        self.change(State::clear_offer);
        self.check_for_updates();
        Ok(())
    }

    /// "Later" in the update dialog.
    pub fn snooze(&self) {
        self.change(|s| s.snoozed = true);
    }

    /// The top bar's update badge opens the dialog again.
    pub fn unsnooze(&self) {
        self.change(|s| s.snoozed = false);
    }

    /// Runs the scheduled checks: the first after [`INITIAL_DELAY`], then every [`CHECK_EVERY`]
    /// when [`due_for_check`] says so. Call once, inside the tokio runtime.
    pub fn start(self: &Arc<Self>) -> JoinHandle<()> {
        let handle = tokio::runtime::Handle::current();
        *self.runtime.lock() = Some(handle.clone());
        let me = Arc::clone(self);
        handle.spawn(async move {
            tokio::time::sleep(INITIAL_DELAY).await;
            loop {
                if me.is_due() {
                    me.check_now().await;
                }
                tokio::time::sleep(CHECK_EVERY).await;
            }
        })
    }

    fn is_due(&self) -> bool {
        let dev = self.source.channel() == DEV;
        due_for_check(
            self.source.local(),
            dev,
            self.last_check.load(Ordering::SeqCst),
            now_ms(),
        )
    }

    /// One scheduled round: checks when due.
    pub fn scheduled_check(self: &Arc<Self>) {
        if self.is_due() {
            self.check_for_updates();
        }
    }

    /// The Hub calls it once it is up, Settings for Check and when the channel changes. Returns at
    /// once; the answer arrives as an [`UpdateStatus`] event.
    pub fn check_for_updates(self: &Arc<Self>) {
        if !self.source.enabled() {
            return;
        }
        let me = Arc::clone(self);
        self.spawn(async move { me.check_now().await });
    }

    /// [`Updater::check_for_updates`], awaited.
    pub async fn check_now(self: &Arc<Self>) {
        if !self.source.enabled() {
            return;
        }
        self.last_check.store(now_ms(), Ordering::SeqCst);
        self.checks_in_flight.fetch_add(1, Ordering::SeqCst);
        self.emit();
        let channel = self.source.channel();
        let outcome = self.source.check_outcome(&channel).await;
        let found = outcome.release;
        let take_now = found.is_some() && channel == DEV && !self.state.lock().is_downloading;
        let busy = if take_now { self.busy_with() } else { None };
        {
            let mut s = self.state.lock();
            s.last_check_error = outcome.error.clone();
            match &found {
                Some(release) => s.offer(release.clone()),
                // a check that went through and found nothing takes the old offer away; one that
                // could not be made (no network, an unreadable manifest) leaves it as it was
                None if outcome.error.is_none() && !s.is_downloading => s.clear_offer(),
                None => {}
            }
            // and an offer kept from another channel is never acted on
            if s.latest_version_info
                .as_ref()
                .is_some_and(|r| !r.channel.eq_ignore_ascii_case(&channel))
            {
                s.clear_offer();
            }
            let note = match (&busy, &found) {
                (Some(busy), Some(release)) => Some(wait_note(release, busy)),
                _ => None,
            };
            if let Some(n) = &note {
                if note != s.waiting_note {
                    tracing::info!("Dev channel: {n}");
                }
            }
            s.waiting_note = note;
        }
        self.checks_in_flight.fetch_sub(1, Ordering::SeqCst);
        self.emit();
        if take_now && busy.is_none() {
            if let Some(release) = &found {
                tracing::info!("Dev channel: taking {} now", release.title());
            }
            self.start_download_and_install(true);
        }
    }

    /// Everything running that quitting would cut short, in words, each once: the window asks
    /// before it closes while there is any. One that cannot say is skipped.
    pub fn busy_all(&self) -> Vec<String> {
        let works = self.busy.read().clone();
        let mut all: Vec<String> = Vec::new();
        for (name, work) in works.iter() {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work.busy_with())) {
                Ok(Some(why)) if !all.contains(&why) => all.push(why),
                Ok(_) => {}
                Err(_) => tracing::warn!("Could not ask {name} whether it is busy"),
            }
        }
        all
    }

    /// The first thing running that installing would cut short, or None; one that cannot say is
    /// skipped.
    pub fn busy_with(&self) -> Option<String> {
        let works = self.busy.read().clone();
        works.iter().find_map(|(name, work)| {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work.busy_with())) {
                Ok(answer) => answer,
                Err(_) => {
                    tracing::warn!("Could not ask {name} whether it is busy");
                    None
                }
            }
        })
    }

    /// Downloads the offered release, verifies it and runs the installer. `automatic` is the dev
    /// channel's own install, which gives way to anything that started meanwhile. Returns the
    /// download's task, or None when there is nothing to download or one is running.
    pub fn start_download_and_install(self: &Arc<Self>, automatic: bool) -> Option<JoinHandle<()>> {
        let release = {
            let mut s = self.state.lock();
            let release = s.latest_version_info.clone()?;
            if s.is_downloading {
                return None;
            }
            s.is_downloading = true;
            s.download_progress = 0.0;
            s.update_error = None;
            release
        };
        self.emit();
        let id = self.download_ids.fetch_add(1, Ordering::SeqCst) + 1;
        let cancel = CancellationToken::new();
        *self.download.lock() = Some(Download {
            id,
            cancel: cancel.clone(),
        });
        let me = Arc::clone(self);
        let task = self.spawn(async move {
            me.download_and_install(id, release, automatic, cancel)
                .await
        });
        if task.is_none() {
            *self.download.lock() = None;
            self.change(|s| s.is_downloading = false);
        }
        task
    }

    /// Stops the download; the file is deleted where the cancellation is caught.
    pub fn cancel_download(&self) {
        if let Some(d) = self.download.lock().as_ref() {
            d.cancel.cancel();
        }
        self.change(|s| s.is_downloading = false);
    }

    /// True while `id` is the download in progress (not cancelled and replaced by a newer one).
    fn is_current(&self, id: u64) -> bool {
        self.download.lock().as_ref().is_some_and(|d| d.id == id)
    }

    async fn download_and_install(
        self: Arc<Self>,
        id: u64,
        release: Release,
        automatic: bool,
        cancel: CancellationToken,
    ) {
        let file_name = Path::new(&release.file)
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| "nook-update.exe".into());
        let temp = self.download_dir.join(file_name);
        let result = self.fetch_and_verify(id, &release, &temp, &cancel).await;
        // A download cancelled and already replaced by a new one touches neither the state nor the
        // file, which the new one is writing.
        let current = self.is_current(id);
        match result {
            Ok(()) if cancel.is_cancelled() || !current => {
                if current {
                    let _ = tokio::fs::remove_file(&temp).await;
                    self.change(|s| {
                        s.is_downloading = false;
                        s.download_progress = 0.0;
                    });
                }
            }
            Ok(()) => {
                let busy = if automatic { self.busy_with() } else { None };
                if let Some(busy) = busy {
                    let _ = tokio::fs::remove_file(&temp).await;
                    let note = wait_note(&release, &busy);
                    tracing::info!("Dev channel: {note}");
                    self.change(|s| {
                        s.is_downloading = false;
                        s.waiting_note = Some(note);
                    });
                } else {
                    tracing::info!(
                        "Update {} downloaded and verified; running the installer",
                        release.title()
                    );
                    self.launch_installer_and_exit(&temp);
                }
            }
            Err(Stop::Cancelled) => {
                if current {
                    let _ = tokio::fs::remove_file(&temp).await;
                    self.change(|s| {
                        s.is_downloading = false;
                        s.download_progress = 0.0;
                    });
                }
            }
            Err(Stop::Failed(e)) => {
                tracing::warn!("Update {} not installed: {e:#}", release.title());
                if current {
                    let _ = tokio::fs::remove_file(&temp).await;
                    self.change(|s| {
                        s.is_downloading = false;
                        s.update_error = Some(format!("Update failed: {e:#}"));
                    });
                }
            }
        }
        let mut download = self.download.lock();
        if download.as_ref().is_some_and(|d| d.id == id) {
            *download = None;
        }
    }

    async fn fetch_and_verify(
        &self,
        id: u64,
        release: &Release,
        temp: &Path,
        cancel: &CancellationToken,
    ) -> Result<(), Stop> {
        tokio::fs::create_dir_all(&self.download_dir)
            .await
            .map_err(|e| anyhow!("could not create {}: {e}", self.download_dir.display()))?;
        if tokio::fs::try_exists(temp).await.unwrap_or(false) {
            let _ = tokio::fs::remove_file(temp).await;
        }
        let mut last_emitted = 0.0f32;
        let download = self.download_file(&release.url, temp, cancel, |progress| {
            if !self.is_current(id) {
                return;
            }
            // one event per thousandth is smooth enough for the bar and cheap for the UI
            let emit = progress >= 1.0 || progress - last_emitted >= 0.001;
            self.state.lock().download_progress = progress;
            if emit {
                last_emitted = progress;
                self.emit();
            }
        });
        tokio::select! {
            r = download => r?,
            _ = cancel.cancelled() => return Err(Stop::Cancelled),
        }
        // exactly what the manifest named, or it is deleted and nothing runs
        let (file, r) = (temp.to_path_buf(), release.clone());
        tokio::task::spawn_blocking(move || source::verify_download(&file, &r))
            .await
            .map_err(|e| anyhow!("{e}"))??;
        Ok(())
    }

    async fn download_file(
        &self,
        url: &str,
        destination: &Path,
        cancel: &CancellationToken,
        mut on_progress: impl FnMut(f32),
    ) -> Result<(), Stop> {
        let parsed = url::Url::parse(url).map_err(|e| anyhow!("bad URL {url}: {e}"))?;
        let mut output = tokio::fs::File::create(destination)
            .await
            .map_err(|e| anyhow!("could not write {}: {e}", destination.display()))?;
        let mut total: u64 = 0;
        if parsed.scheme().eq_ignore_ascii_case("file") {
            // a feed on this machine names its installer by file: URL
            let path = parsed
                .to_file_path()
                .map_err(|_| anyhow!("bad URL {url}"))?;
            let mut input = tokio::fs::File::open(&path)
                .await
                .map_err(|e| anyhow!("{}: {e}", path.display()))?;
            let length = input.metadata().await.map(|m| m.len()).unwrap_or(0);
            let mut buf = vec![0u8; CHUNK];
            loop {
                if cancel.is_cancelled() {
                    return Err(Stop::Cancelled);
                }
                let n = tokio::io::AsyncReadExt::read(&mut input, &mut buf)
                    .await
                    .map_err(anyhow::Error::from)?;
                if n == 0 {
                    break;
                }
                output
                    .write_all(&buf[..n])
                    .await
                    .map_err(anyhow::Error::from)?;
                total += n as u64;
                if length > 0 {
                    on_progress(total as f32 / length as f32);
                }
            }
        } else {
            let mut response = self
                .client
                .get(parsed)
                .send()
                .await
                .map_err(anyhow::Error::from)?;
            if response.status() != reqwest::StatusCode::OK {
                return Err(Stop::Failed(anyhow!(
                    "Server returned HTTP {}",
                    response.status().as_u16()
                )));
            }
            let length = response.content_length().unwrap_or(0);
            loop {
                if cancel.is_cancelled() {
                    return Err(Stop::Cancelled);
                }
                let Some(chunk) = response.chunk().await.map_err(anyhow::Error::from)? else {
                    break;
                };
                output
                    .write_all(&chunk)
                    .await
                    .map_err(anyhow::Error::from)?;
                total += chunk.len() as u64;
                if length > 0 {
                    on_progress(total as f32 / length as f32);
                }
            }
        }
        output.flush().await.map_err(anyhow::Error::from)?;
        Ok(())
    }

    /// Starts the installer (through the update script) and quits the app.
    fn launch_installer_and_exit(&self, installer: &Path) {
        let launcher = self.launcher.read().clone();
        match launcher(installer) {
            Ok(()) => {
                let quit = self.quit.read().clone();
                match quit {
                    Some(quit) => quit(),
                    None => {
                        // Give the script a moment to actually start before the process ends.
                        std::thread::spawn(|| {
                            std::thread::sleep(Duration::from_secs(1));
                            std::process::exit(0);
                        });
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Update: the installer did not start: {e:#}");
                self.change(|s| {
                    s.update_error = Some(format!("Failed to launch installer: {e:#}"));
                    s.is_downloading = false;
                });
            }
        }
    }

    fn spawn<F>(&self, fut: F) -> Option<JoinHandle<()>>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let handle = tokio::runtime::Handle::try_current()
            .ok()
            .or_else(|| self.runtime.lock().clone());
        match handle {
            Some(h) => Some(h.spawn(fut)),
            None => {
                tracing::warn!("Update: no async runtime to run on");
                None
            }
        }
    }
}

/// A feed on this machine is read every minute, and so is the dev channel, whose builds come with
/// each commit; stable asks a web host every fifteen.
pub fn due_for_check(local: bool, dev: bool, last_check_ms: i64, now_ms: i64) -> bool {
    local || dev || now_ms - last_check_ms >= STABLE_EVERY_MS
}

/// Settings' line for a dev build held back while something runs.
pub fn wait_note(release: &Release, busy: &str) -> String {
    format!(
        "Nook {} installs once nothing is running; for now {busy}.",
        release.title()
    )
}

#[cfg(not(test))]
fn default_launcher(installer: &Path) -> Result<()> {
    super::install::launch(installer)
}

/// A test that forgets its fake launcher must never run a real installer.
#[cfg(test)]
fn default_launcher(_installer: &Path) -> Result<()> {
    anyhow::bail!("tests never run an installer")
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

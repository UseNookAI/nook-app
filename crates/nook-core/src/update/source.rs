//! Ports `update/UpdateSource.java`: where updates come from.
//!
//! `<base>/<channel>/latest.json` with its signature beside it. The base is [`DEFAULT_BASE_URL`]
//! (blank unless the build was stamped with one, so a build from source never checks), overridden by the
//! `NOOK_RS_UPDATE_URL` environment variable for sandboxes and QA; either is a web address, a
//! `file:` URL or a plain folder. The channel is the `UPDATE_CHANNEL` setting, `stable` unless the
//! person chooses `dev`. Nothing here runs anything: [`UpdateSource::check`] says whether a newer
//! build is published, [`verify_download`] says whether a downloaded installer is the one the
//! manifest named. The installer's own Authenticode signature is checked by Windows when it runs;
//! a signer check here comes with the certificate.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ed25519_dalek::VerifyingKey;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use regex::Regex;
use sha2::{Digest, Sha256};

use super::manifest::{self, Release};
use crate::build_info::{compare_versions, BuildInfo};
use crate::settings::{Settings, UPDATE_CHANNEL};

pub const STABLE: &str = "stable";
pub const DEV: &str = "dev";
/// Connect and read timeout for the manifest and its signature.
pub const TIMEOUT: Duration = Duration::from_secs(10);
pub const MANIFEST_MAX_BYTES: usize = 64 * 1024;
const SIGNATURE_MAX_BYTES: usize = 4096;
/// Environment variable that points the app at a feed (a folder or a web address).
pub const UPDATE_URL_ENV: &str = "NOOK_RS_UPDATE_URL";
/// The configured base (the original's `nook.update.baseUrl`). A blank base means the app never
/// checks: blank unless the build was stamped with `NOOK_RS_UPDATE_BASE` (tools/publish.ps1 bakes
/// in the feed or host it publishes to, so the builds it makes follow it).
pub const DEFAULT_BASE_URL: &str = match option_env!("NOOK_RS_UPDATE_BASE") {
    Some(base) => base,
    None => "",
};
/// The User-Agent every update request carries.
pub(crate) const USER_AGENT: &str = "Nook";
/// The folder under a channel with this platform's manifest: none for Windows (the channel's own
/// `latest.json`), `macos-arm64` for Apple silicon Macs (`nook-release manifest --platform`).
pub const PLATFORM_DIR: Option<&str> = if cfg!(target_os = "macos") {
    Some("macos-arm64")
} else {
    None
};

/// Where the update channel is kept: the `UPDATE_CHANNEL` setting. A trait so the updater can be
/// built over the app's [`Settings`] however the app holds them, and over a fake in tests.
pub trait ChannelStore: Send + Sync {
    fn get(&self) -> Option<String>;
    fn set(&self, channel: &str) -> Result<()>;
}

impl ChannelStore for Settings {
    fn get(&self) -> Option<String> {
        Settings::get(self, UPDATE_CHANNEL)
    }
    fn set(&self, channel: &str) -> Result<()> {
        Settings::set(self, UPDATE_CHANNEL, channel)
    }
}

pub struct UpdateSource {
    base_url: String,
    keys: Vec<VerifyingKey>,
    build: BuildInfo,
    channel_store: Option<Arc<dyn ChannelStore>>,
    last_error: Mutex<Option<String>>,
    client: reqwest::Client,
}

/// What one check found: the newer release, if any, and what went wrong, if anything.
#[derive(Clone, Debug, Default)]
pub struct CheckOutcome {
    pub release: Option<Release>,
    pub error: Option<String>,
}

impl UpdateSource {
    /// The app's source: the base from the environment or [`DEFAULT_BASE_URL`], the bundled
    /// release keys, this build.
    pub fn from_env(channel_store: Option<Arc<dyn ChannelStore>>) -> UpdateSource {
        let keys = manifest::bundled_keys().unwrap_or_else(|e| {
            // A bad line in the bundled key file is a broken build; with no keys nothing verifies,
            // so nothing is ever offered, which is the safe way to be broken.
            tracing::error!("release-keys.txt is unreadable: {e:#}");
            Vec::new()
        });
        let env = std::env::var(UPDATE_URL_ENV).ok();
        UpdateSource::new(
            &choose_base(Some(DEFAULT_BASE_URL), env.as_deref()),
            keys,
            BuildInfo::current(),
            channel_store,
        )
    }

    pub fn new(
        base_url: &str,
        keys: Vec<VerifyingKey>,
        build: BuildInfo,
        channel_store: Option<Arc<dyn ChannelStore>>,
    ) -> UpdateSource {
        let client = reqwest::Client::builder()
            .connect_timeout(TIMEOUT)
            .read_timeout(TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .unwrap_or_default();
        UpdateSource {
            base_url: base_url.trim().trim_end_matches('/').to_string(),
            keys,
            build,
            channel_store,
            last_error: Mutex::new(None),
            client,
        }
    }

    pub fn build(&self) -> &BuildInfo {
        &self.build
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// True when there is somewhere to ask; a blank base means the app never checks.
    pub fn enabled(&self) -> bool {
        !self.base_url.is_empty()
    }

    /// True when the base is a folder on this machine, where a check is only a file read.
    pub fn local(&self) -> bool {
        self.base_url
            .get(..5)
            .is_some_and(|s| s.eq_ignore_ascii_case("file:"))
    }

    /// What went wrong at the last check, for the log and the Settings page; None when nothing did.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().clone()
    }

    pub fn channel(&self) -> String {
        self.channel_store
            .as_ref()
            .and_then(|s| s.get())
            .map(|v| v.trim().to_lowercase())
            .filter(|v| v == DEV || v == STABLE)
            .unwrap_or_else(|| STABLE.to_string())
    }

    pub fn set_channel(&self, channel: &str) -> Result<()> {
        let c = if channel.eq_ignore_ascii_case(DEV) {
            DEV
        } else {
            STABLE
        };
        match &self.channel_store {
            Some(store) => store.set(c),
            None => Ok(()),
        }
    }

    /// The manifest's URL for a channel: the channel's own `latest.json` for the Windows installer,
    /// where every Nook before the Mac's reads it, and a folder of the platform's under it for a
    /// Mac ([`PLATFORM_DIR`]).
    pub fn manifest_url(&self, channel: &str) -> String {
        match PLATFORM_DIR {
            Some(platform) => format!("{}/{}/{platform}/latest.json", self.base_url, channel),
            None => format!("{}/{}/latest.json", self.base_url, channel),
        }
    }

    /// Asks the channel for its latest build. None when there is none newer, when nothing is
    /// configured, or when anything about the answer is wrong (unreachable, unsigned, malformed):
    /// the reason is in [`UpdateSource::last_error`] and the log, and no update is offered.
    pub async fn check(&self, channel: &str) -> Option<Release> {
        self.check_outcome(channel).await.release
    }

    /// [`UpdateSource::check`] with its error alongside, so a caller does not have to read
    /// `last_error` back (another check may have run in between).
    pub async fn check_outcome(&self, channel: &str) -> CheckOutcome {
        *self.last_error.lock() = None;
        if !self.enabled() {
            return CheckOutcome::default();
        }
        let outcome = self.ask(channel).await;
        *self.last_error.lock() = outcome.error.clone();
        outcome
    }

    async fn ask(&self, channel: &str) -> CheckOutcome {
        let url = self.manifest_url(channel);
        let failed = |error: String| CheckOutcome {
            release: None,
            error: Some(error),
        };
        let fetched = async {
            let manifest = fetch(&self.client, &url, MANIFEST_MAX_BYTES).await?;
            let signature = fetch(&self.client, &format!("{url}.sig"), SIGNATURE_MAX_BYTES).await?;
            anyhow::Ok((
                manifest,
                String::from_utf8_lossy(&signature).trim().to_string(),
            ))
        };
        let (manifest, signature) = match fetched.await {
            Ok(v) => v,
            Err(e) => {
                let error = format!("could not read {url}: {e:#}");
                tracing::info!("Update check: {error}");
                return failed(error);
            }
        };
        if !manifest::verify(&manifest, &signature, &self.keys) {
            let error = format!("the manifest at {url} is not signed by Nook; ignored");
            tracing::warn!("Update check: {error}");
            return failed(error);
        }
        let Some(r) = manifest::parse(&manifest) else {
            let error = format!("the manifest at {url} is not in the expected form; ignored");
            tracing::warn!("Update check: {error}");
            return failed(error);
        };
        if !r.channel.eq_ignore_ascii_case(channel) {
            let error = format!(
                "the manifest at {url} is for the {} channel; ignored",
                r.channel
            );
            tracing::warn!("Update check: {error}");
            return failed(error);
        }
        if !is_newer(&r, &self.build, channel) {
            tracing::debug!(
                "Update check: {channel} {} is not newer than this build {}",
                r.title(),
                self.build.label()
            );
            return CheckOutcome::default();
        }
        tracing::info!(
            "Update available on {channel}: {} (this build {})",
            r.title(),
            self.build.label()
        );
        CheckOutcome {
            release: Some(r),
            error: None,
        }
    }
}

/// The environment wins over the configured base; a blank one leaves the configured base.
pub fn choose_base(configured: Option<&str>, env: Option<&str>) -> String {
    if let Some(env) = env.filter(|e| !e.trim().is_empty()) {
        return to_url(env.trim());
    }
    configured.map(|c| to_url(c.trim())).unwrap_or_default()
}

static SCHEME: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^[a-z][a-z0-9+.-]+:").expect("scheme regex"));

/// A base is a URL (a download host), or a folder on this machine (a test feed), which becomes
/// its `file:` URL. A scheme is two letters or more, so `C:\...` is a path.
pub fn to_url(base: &str) -> String {
    if base.is_empty() || SCHEME.is_match(base) {
        return base.to_string();
    }
    let path = Path::new(base);
    let Ok(abs) = std::path::absolute(path) else {
        return base.to_string();
    };
    // As Java's Path.toUri: an existing folder's URL ends in a slash.
    let url = if abs.is_dir() {
        url::Url::from_directory_path(&abs)
    } else {
        url::Url::from_file_path(&abs)
    };
    url.map(|u| u.to_string())
        .unwrap_or_else(|_| base.to_string())
}

/// Newer means a higher version; on the dev channel also the same version from a different
/// commit published after this build was made, since every push to main is a dev build.
pub fn is_newer(r: &Release, build: &BuildInfo, channel: &str) -> bool {
    let by_version = compare_versions(&r.version, &build.version);
    if by_version.is_ne() {
        return by_version.is_gt();
    }
    if !channel.eq_ignore_ascii_case(DEV) {
        return false;
    }
    if r.commit.is_empty() || r.commit.eq_ignore_ascii_case(&build.commit) {
        return false;
    }
    matches!((r.published, build.time), (Some(published), Some(built)) if published > built)
}

/// The downloaded installer must be exactly what the manifest named, or it is deleted and refused.
/// Blocking: it reads the whole file.
pub fn verify_download(file: &Path, r: &Release) -> Result<()> {
    let size = std::fs::metadata(file)
        .with_context(|| format!("{}", file.display()))?
        .len();
    if size != r.size {
        let _ = std::fs::remove_file(file);
        bail!(
            "the download is {size} bytes, the manifest says {}; refused",
            r.size
        );
    }
    let digest = sha256(file)?;
    if !digest.eq_ignore_ascii_case(&r.sha256) {
        let _ = std::fs::remove_file(file);
        bail!(
            "the download's sha256 {}… is not the manifest's {}…; refused",
            &digest[..12],
            r.sha256.get(..12).unwrap_or(&r.sha256)
        );
    }
    Ok(())
}

/// A file's sha256 as lowercase hex. Blocking.
pub fn sha256(file: &Path) -> Result<String> {
    let mut f = std::fs::File::open(file).with_context(|| format!("{}", file.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Reads a web address or a `file:` URL, refusing anything over `max_bytes`.
async fn fetch(client: &reqwest::Client, url: &str, max_bytes: usize) -> Result<Vec<u8>> {
    let parsed = url::Url::parse(url).map_err(|_| anyhow::anyhow!("bad URL {url}"))?;
    if parsed.scheme().eq_ignore_ascii_case("file") {
        let path = parsed
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("bad URL {url}"))?;
        let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            let file = std::fs::File::open(&path).with_context(|| format!("{}", path.display()))?;
            let mut bytes = Vec::new();
            file.take(max_bytes as u64 + 1).read_to_end(&mut bytes)?;
            Ok(bytes)
        })
        .await??;
        if bytes.len() > max_bytes {
            bail!("more than {max_bytes} bytes");
        }
        return Ok(bytes);
    }
    let mut response = client.get(parsed).send().await?;
    if !response.status().is_success() {
        bail!("HTTP {}", response.status().as_u16());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > max_bytes {
            bail!("more than {max_bytes} bytes");
        }
    }
    Ok(bytes)
}

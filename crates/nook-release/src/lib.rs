//! Nook releases: the signing key, the signed `latest.json` manifest, and its check. The Rust
//! port of the original's `tools/release.py` (and the parts of `tools/ed25519.py` it used).
//!
//! The app (`nook_core::update`) fetches `<base>/<channel>/latest.json` and `latest.json.sig`,
//! verifies the signature against the public keys in `resources/release-keys.txt`, compares the
//! version (and on the dev channel the build) with its own, downloads the installer, checks its
//! sha256 and size, and only then runs it. The private key never ships: it lives with whoever
//! publishes, outside every repository.
//!
//! The manifest is written to `DIR/<channel>/latest.json` with `latest.json.sig` beside it: one
//! line, the base64url Ed25519 signature over "nook-release-v1" + the exact bytes of latest.json.
//! The bytes are the ones release.py writes for the same inputs (two-space JSON, non-ASCII escaped,
//! a final newline), so either tool can re-sign or check the other's work. The key file is
//! release.py's too: `{"algorithm", "purpose", "seed" (hex), "public" (base64url), "created"}`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{SecondsFormat, Utc};
use ed25519_dalek::SigningKey;
use nook_core::update::manifest;
use once_cell_regex::regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CHANNELS: [&str; 2] = ["stable", "dev"];
/// The key file's `purpose`: the signature domain.
pub const PURPOSE: &str = "nook-release-v1";

/// A signing key file, as release.py writes it.
#[derive(Debug, Serialize, Deserialize)]
pub struct KeyFile {
    pub algorithm: String,
    pub purpose: String,
    /// The 32-byte Ed25519 seed, hex.
    pub seed: String,
    /// The public half, base64url, as release-keys.txt has it.
    pub public: String,
    pub created: String,
}

/// Makes a new key at `path` (refusing to overwrite one) and returns its public half.
pub fn keygen(path: &Path) -> Result<String> {
    if path.exists() {
        bail!("refusing to overwrite {}", path.display());
    }
    let seed: [u8; 32] = rand::random();
    let key = SigningKey::from_bytes(&seed);
    let public = manifest::encode_public_key(&key.verifying_key());
    let file = KeyFile {
        algorithm: "Ed25519".into(),
        purpose: PURPOSE.into(),
        seed: hex::encode(seed),
        public: public.clone(),
        created: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false),
    };
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&file)?;
    // create_new: never replace a key that appeared since the check above
    use std::io::Write;
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("refusing to overwrite {}", path.display()))?;
    out.write_all(json.as_bytes())?;
    Ok(public)
}

/// Reads a key file's seed.
pub fn load_key(path: &Path) -> Result<SigningKey> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let file: KeyFile = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a key file", path.display()))?;
    let seed = hex::decode(file.seed.trim())
        .ok()
        .and_then(|s| <[u8; 32]>::try_from(s).ok());
    let Some(seed) = seed else {
        bail!("the key file's seed is not 32 bytes");
    };
    Ok(SigningKey::from_bytes(&seed))
}

/// A key's public half, as release-keys.txt has it.
pub fn public_line(key: &SigningKey) -> String {
    manifest::encode_public_key(&key.verifying_key())
}

/// One manifest's fields, in release.py's order (the order they are written in).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub channel: String,
    pub version: String,
    pub file: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub commit: String,
    pub notes: String,
    pub published: String,
}

/// What `manifest` is asked for.
#[derive(Clone, Debug, Default)]
pub struct ManifestRequest {
    pub channel: String,
    /// The installer, to measure; or `sha256` and `size` for one already published.
    pub installer: Option<PathBuf>,
    pub sha256: Option<String>,
    pub size: Option<u64>,
    /// Where the installer will be downloaded from.
    pub url: Option<String>,
    /// Or the download host's base, for the original's layout:
    /// `<base>/builds/<version>-<commit>/<file>`.
    pub base: Option<String>,
    pub version: Option<String>,
    pub commit: Option<String>,
    pub notes: Option<String>,
    pub published: Option<String>,
}

/// What the build stamped beside its installer (`build.json`).
#[derive(Default, Deserialize)]
struct BuildJson {
    version: Option<String>,
    commit: Option<String>,
}

/// Works out a manifest: measures the installer (or takes the measurements given), and fills the
/// version and commit from the arguments, else `build.json` beside the installer, else the
/// installer's name.
pub fn build_manifest(req: &ManifestRequest) -> Result<Manifest> {
    if !CHANNELS.contains(&req.channel.as_str()) {
        bail!("--channel is stable or dev");
    }
    let mut build = BuildJson::default();
    let (name, sha256, size) = if let Some(installer) = &req.installer {
        if !installer.is_file() {
            bail!("no installer at {}", installer.display());
        }
        let beside = std::path::absolute(installer)?.with_file_name("build.json");
        if beside.is_file() {
            build = serde_json::from_str(&std::fs::read_to_string(&beside)?)
                .with_context(|| format!("{} is not JSON", beside.display()))?;
        }
        let name = installer
            .file_name()
            .context("the installer has no file name")?
            .to_string_lossy()
            .to_string();
        (
            name,
            sha256_of(installer)?,
            std::fs::metadata(installer)?.len(),
        )
    } else {
        // an installer that is already where --url says, described by what was measured there
        let (Some(sha256), Some(size)) = (&req.sha256, req.size) else {
            bail!("pass --installer, or --sha256 and --size for an installer already published");
        };
        if !regex!(r"^[0-9a-f]{64}$").is_match(sha256) {
            bail!("--sha256 is not 64 lowercase hex digits");
        }
        let Some(url) = &req.url else {
            bail!("--url is needed for an installer already published");
        };
        (file_name_of(url), sha256.clone(), size)
    };
    let version = req
        .version
        .clone()
        .filter(|v| !v.is_empty())
        .or(build.version.filter(|v| !v.is_empty()))
        .or_else(|| version_from_name(&name));
    let Some(version) = version else {
        bail!("no version: pass --version or put build.json beside the installer");
    };
    let commit: String = req
        .commit
        .clone()
        .filter(|c| !c.is_empty())
        .or(build.commit)
        .unwrap_or_default()
        .chars()
        .take(7)
        .collect();
    let url = match (&req.url, &req.base) {
        (Some(url), _) => url.clone(),
        (None, Some(base)) => layout_url(base, &version, &commit, &name),
        (None, None) => bail!("pass --url, or --base for the host's layout"),
    };
    Ok(Manifest {
        channel: req.channel.clone(),
        version,
        file: name,
        url,
        sha256,
        size,
        commit,
        notes: req.notes.clone().unwrap_or_default(),
        published: req
            .published
            .clone()
            .unwrap_or_else(|| Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()),
    })
}

/// The download host's layout (the original's publish.ps1): each build under its own path, which
/// never changes, `<base>/builds/<version>-<commit>/<file>`.
pub fn layout_url(base: &str, version: &str, commit: &str, file: &str) -> String {
    const SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'%')
        .add(b'/')
        .add(b'<')
        .add(b'>')
        .add(b'?')
        .add(b'`')
        .add(b'{')
        .add(b'}');
    let build = if commit.is_empty() {
        version.to_string()
    } else {
        format!("{version}-{commit}")
    };
    format!(
        "{}/builds/{build}/{}",
        base.trim_end_matches('/'),
        percent_encoding::utf8_percent_encode(file, SEGMENT)
    )
}

/// The last segment of a URL, percent-decoded: the installer's file name.
fn file_name_of(url: &str) -> String {
    let last = url.trim_end_matches('/').rsplit('/').next().unwrap_or(url);
    percent_encoding::percent_decode_str(last)
        .decode_utf8_lossy()
        .to_string()
}

/// The version in an installer's name: Tauri's `Nook_0.5.1_x64-setup.exe` (or the earlier
/// `Nook RS_0.5.1_x64-setup.exe`), `Nook-RS-0.5.1-setup.exe`, the published `Nook-0.5.1.exe`, or
/// a Mac update's `Nook-0.6.0-macos-arm64.app.tar.gz`.
pub fn version_from_name(name: &str) -> Option<String> {
    let tauri = regex!(r"^Nook(?:[ -]RS)?[-_](\d[^_]*?)(?:[-_]x64)?[-_]setup\.exe$");
    let original = regex!(r"^Nook-(.+)\.exe$");
    let mac = regex!(r"^Nook[-_](\d[^_]*?)[-_]macos[-_][a-z0-9]+\.app\.tar\.gz$");
    tauri
        .captures(name)
        .or_else(|| original.captures(name))
        .or_else(|| mac.captures(name))
        .map(|c| c[1].to_string())
}

/// The manifest's bytes as release.py writes them: `json.dumps(manifest, indent=2) + "\n"`, which
/// escapes everything outside printable ASCII.
pub fn manifest_bytes(m: &Manifest) -> Vec<u8> {
    let pretty = serde_json::to_string_pretty(m).expect("a manifest serializes");
    let mut out = String::with_capacity(pretty.len() + 1);
    for c in pretty.chars() {
        if (c as u32) < 0x7f {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out.push('\n');
    out.into_bytes()
}

/// Writes `DIR/<channel>/latest.json` and its `latest.json.sig`, checks the pair verifies against
/// the key's public half, and returns the manifest's path.
pub fn write_signed(key: &SigningKey, m: &Manifest, out: &Path) -> Result<PathBuf> {
    write_signed_in(key, m, &out.join(&m.channel))
}

/// [`write_signed`] for a platform whose builds have a manifest of their own under the channel's
/// (`DIR/<channel>/<platform>/latest.json`, e.g. `macos-arm64`): the Windows installer's stays the
/// channel's own `latest.json`, where every Nook before the Mac's reads it.
pub fn write_signed_for(
    key: &SigningKey,
    m: &Manifest,
    out: &Path,
    platform: Option<&str>,
) -> Result<PathBuf> {
    match platform.filter(|p| !p.is_empty()) {
        Some(p) => {
            if !regex!(r"^[a-z0-9][a-z0-9-]*$").is_match(p) {
                bail!("--platform is a folder name such as macos-arm64");
            }
            write_signed_in(key, m, &out.join(&m.channel).join(p))
        }
        None => write_signed(key, m, out),
    }
}

fn write_signed_in(key: &SigningKey, m: &Manifest, dir: &Path) -> Result<PathBuf> {
    let body = manifest_bytes(m);
    let signature = manifest::sign(key, &body);
    let dir = dir.to_path_buf();
    std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    let path = dir.join("latest.json");
    std::fs::write(&path, &body).with_context(|| format!("could not write {}", path.display()))?;
    std::fs::write(dir.join("latest.json.sig"), format!("{signature}\n"))?;
    if !manifest::verify(&body, &signature, &[key.verifying_key()]) {
        bail!("the signature just made does not verify");
    }
    Ok(path)
}

/// Checks `manifest` (with its `.sig` beside it) against `keys`: a public key (base64url) or a
/// file of them such as release-keys.txt. The parsed release when the signature holds.
pub fn verify_files(
    keys: &str,
    manifest_path: &Path,
) -> Result<Option<nook_core::update::Release>> {
    let pubs = if Path::new(keys).exists() {
        manifest::read_keys(&std::fs::read_to_string(keys)?)?
    } else {
        vec![manifest::public_key(keys)?]
    };
    let body = std::fs::read(manifest_path)
        .with_context(|| format!("could not read {}", manifest_path.display()))?;
    let sig_path = PathBuf::from(format!("{}.sig", manifest_path.display()));
    let signature = std::fs::read_to_string(&sig_path)
        .with_context(|| format!("could not read {}", sig_path.display()))?;
    if !manifest::verify(&body, signature.trim(), &pubs) {
        return Ok(None);
    }
    manifest::parse(&body)
        .map(Some)
        .context("signed, but not in the form the app expects")
}

pub fn sha256_of(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f =
        std::fs::File::open(path).with_context(|| format!("could not read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// `regex!` compiles a pattern once.
mod once_cell_regex {
    macro_rules! regex {
        ($re:literal $(,)?) => {{
            static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
            RE.get_or_init(|| regex::Regex::new($re).expect("a valid pattern"))
        }};
    }
    pub(crate) use regex;
}

#[cfg(test)]
mod tests;

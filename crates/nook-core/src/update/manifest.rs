//! Ports `update/ReleaseManifest.java`: the signed release manifest (docs/plan/distribution.md of
//! the original).
//!
//! `<base>/<channel>/latest.json` and `latest.json.sig` beside it, one line of base64url holding
//! the Ed25519 signature over [`DOMAIN`] and the exact bytes of the manifest. The app carries the
//! public keys in `resources/release-keys.txt` ([`crate::resources::RELEASE_KEYS`]); the private
//! key is with whoever publishes (`nook-release` today, the Rust port of tools/release.py). A
//! manifest that does not verify is not a manifest: nothing is offered, nothing is downloaded.

use anyhow::{anyhow, bail, Result};
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::DecodePaddingMode;
use base64::Engine;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize, Serializer};

/// What the signature covers before the manifest's bytes, so a signature over anything else
/// (another protocol with the same key) can never pass for a release.
pub const DOMAIN: &[u8] = b"nook-release-v1";

/// base64url as Java's URL decoder reads it: padding optional; written without padding.
const B64URL: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// One published build.
///
/// - `channel`: `stable` or `dev`
/// - `version`: the version the installer carries
/// - `file`: the installer's file name
/// - `url`: where the installer is downloaded from
/// - `sha256`: the installer's digest, checked after the download
/// - `size`: the installer's size in bytes, checked after the download
/// - `commit`: the short commit the build came from, empty when unknown; tells dev builds of one
///   version apart
/// - `notes`: what changed, for the update dialog
/// - `published`: when it was published; the dev channel compares this with the build's own time
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub channel: String,
    pub version: String,
    pub file: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub commit: String,
    pub notes: String,
    pub published: Option<DateTime<Utc>>,
}

impl Release {
    /// "0.5.1 (abc1234)", or the version alone when the commit is unknown.
    pub fn title(&self) -> String {
        if self.commit.is_empty() {
            self.version.clone()
        } else {
            format!("{} ({})", self.version, self.commit)
        }
    }
}

/// The record's fields plus `title`, which the update dialog shows (Java's `title()`).
impl Serialize for Release {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct View<'a> {
            channel: &'a str,
            version: &'a str,
            file: &'a str,
            url: &'a str,
            sha256: &'a str,
            size: u64,
            commit: &'a str,
            notes: &'a str,
            published: Option<DateTime<Utc>>,
            title: String,
        }
        View {
            channel: &self.channel,
            version: &self.version,
            file: &self.file,
            url: &self.url,
            sha256: &self.sha256,
            size: self.size,
            commit: &self.commit,
            notes: &self.notes,
            published: self.published,
            title: self.title(),
        }
        .serialize(serializer)
    }
}

/// True when one of the keys signed exactly these bytes.
pub fn verify(manifest: &[u8], signature_base64url: &str, keys: &[VerifyingKey]) -> bool {
    let Ok(raw) = B64URL.decode(signature_base64url.trim()) else {
        return false;
    };
    let Ok(signature) = Signature::from_slice(&raw) else {
        return false;
    };
    let message = message(manifest);
    keys.iter()
        .any(|key| key.verify(&message, &signature).is_ok())
}

/// The fields of a manifest; None when it is not one (missing fields, a bad number, not JSON).
pub fn parse(manifest: &[u8]) -> Option<Release> {
    let n: serde_json::Value = serde_json::from_slice(manifest).ok()?;
    let channel = text(&n, "channel");
    let version = text(&n, "version");
    let file = text(&n, "file");
    let url = text(&n, "url");
    let sha256 = text(&n, "sha256").to_lowercase();
    let size = number(&n, "size");
    if channel.is_empty()
        || version.is_empty()
        || file.is_empty()
        || url.is_empty()
        || sha256.chars().count() != 64
        || size <= 0
    {
        return None;
    }
    let published_text = text(&n, "published");
    let published = if published_text.is_empty() {
        None
    } else {
        Some(
            DateTime::parse_from_rfc3339(&published_text)
                .ok()?
                .with_timezone(&Utc),
        )
    };
    Some(Release {
        channel,
        version,
        file,
        url,
        sha256,
        size: size as u64,
        commit: text(&n, "commit"),
        notes: text(&n, "notes"),
        published,
    })
}

/// A field as text the way Jackson's `path(name).asText("")` reads it: strings as they are,
/// numbers and booleans spelled out, anything else (missing, null, an object) empty.
fn text(n: &serde_json::Value, name: &str) -> String {
    match n.get(name) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(x)) => x.to_string(),
        Some(serde_json::Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// A field as a whole number the way Jackson's `asLong(-1)` reads it; -1 when it is not one.
fn number(n: &serde_json::Value, name: &str) -> i64 {
    match n.get(name) {
        Some(serde_json::Value::Number(x)) => x
            .as_i64()
            .or_else(|| x.as_f64().map(|f| f as i64))
            .unwrap_or(-1),
        Some(serde_json::Value::String(s)) => s.trim().parse().unwrap_or(-1),
        _ => -1,
    }
}

/// Signs a manifest; the publishing tool and the tests use it, the app never holds a private key.
pub fn sign(key: &SigningKey, manifest: &[u8]) -> String {
    B64URL.encode(key.sign(&message(manifest)).to_bytes())
}

/// The keys built into the app (`resources/release-keys.txt`).
pub fn bundled_keys() -> Result<Vec<VerifyingKey>> {
    read_keys(crate::resources::RELEASE_KEYS)
}

/// Reads a key file: one base64url Ed25519 public key per line, `#` starts a comment, and so does
/// anything after the key on its line.
pub fn read_keys(text: &str) -> Result<Vec<VerifyingKey>> {
    let mut keys = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some(key) = line.split_whitespace().next() else {
            continue;
        };
        keys.push(public_key(key).map_err(|e| anyhow!("bad release key line: {line}: {e}"))?);
    }
    Ok(keys)
}

/// A raw 32-byte Ed25519 public key, base64url as the key file carries it.
pub fn public_key(base64url: &str) -> Result<VerifyingKey> {
    let raw = B64URL
        .decode(base64url.trim())
        .map_err(|e| anyhow!("not base64url: {e}"))?;
    let Ok(bytes) = <[u8; 32]>::try_from(raw.as_slice()) else {
        bail!(
            "an Ed25519 public key is 32 bytes, this one is {}",
            raw.len()
        );
    };
    VerifyingKey::from_bytes(&bytes).map_err(|e| anyhow!("not an Ed25519 public key: {e}"))
}

/// A public key as the key file carries it: its 32 raw bytes in base64url without padding.
pub fn encode_public_key(key: &VerifyingKey) -> String {
    B64URL.encode(key.as_bytes())
}

/// base64url without padding, as the signature and key files carry it.
pub fn b64url(raw: &[u8]) -> String {
    B64URL.encode(raw)
}

fn message(manifest: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(DOMAIN.len() + manifest.len());
    message.extend_from_slice(DOMAIN);
    message.extend_from_slice(manifest);
    message
}

//! Ports `gateway/GatewayInfo.java`.
//!
//! The local gateway's identity: a bearer token generated per app start and the port it ended up
//! on, written to `<home>\gateway.json` so local tools (OpenClaw, tests) can find it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};

/// What `gateway.json` holds, in the original's field order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayFile {
    pub base_url: String,
    pub port: u16,
    pub token: String,
    pub openai_base_url: String,
}

pub struct GatewayInfo {
    file: PathBuf,
    token: String,
    port: u16,
}

impl GatewayInfo {
    /// A fresh identity for a gateway listening on `port`, published at `file`.
    pub fn new(file: PathBuf, port: u16) -> GatewayInfo {
        let mut b = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut b);
        GatewayInfo {
            file,
            token: hex::encode(b),
            port,
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// Whether a presented `Authorization` (with or without `Bearer `) or `X-Api-Key` value is
    /// this start's token. Compared in constant time.
    pub fn matches(&self, presented: Option<&str>) -> bool {
        let Some(presented) = presented else {
            return false;
        };
        let t = match presented.strip_prefix("Bearer ") {
            Some(rest) => rest.trim(),
            None => presented.trim(),
        };
        constant_time_eq(t.as_bytes(), self.token.as_bytes())
    }

    /// What `gateway.json` says.
    pub fn contents(&self) -> GatewayFile {
        GatewayFile {
            base_url: self.base_url(),
            port: self.port,
            token: self.token.clone(),
            openai_base_url: format!("{}/v1", self.base_url()),
        }
    }

    /// Writes `gateway.json`, through a temporary file so a reader never sees half of it. A
    /// failure is logged, not fatal: the gateway still answers whoever knows the token.
    pub fn write_file(&self) {
        match self.try_write() {
            Ok(()) => tracing::info!(
                "Local gateway listening on {} (token in {})",
                self.base_url(),
                self.file.display()
            ),
            Err(e) => tracing::warn!("Could not write gateway.json: {e:#}"),
        }
    }

    fn try_write(&self) -> Result<()> {
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Could not create {}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(&self.contents())?;
        let tmp = self.file.with_extension("json.tmp");
        std::fs::write(&tmp, json).with_context(|| format!("Could not write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.file).with_context(|| {
            let _ = std::fs::remove_file(&tmp);
            format!("Could not replace {}", self.file.display())
        })
    }

    /// Removes `gateway.json` when the gateway stops, unless it now belongs to another start
    /// (another token), so a tool never reads a port and token that no longer answer.
    pub fn remove_file(&self) {
        let ours = std::fs::read(&self.file)
            .ok()
            .and_then(|b| serde_json::from_slice::<GatewayFile>(&b).ok())
            .is_some_and(|f| f.token == self.token);
        if ours {
            if let Err(e) = std::fs::remove_file(&self.file) {
                tracing::warn!("Could not remove gateway.json: {e}");
            }
        }
    }
}

/// `MessageDigest.isEqual`: equal lengths and bytes, without stopping at the first difference.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_is_new_each_start_and_read_with_or_without_bearer() {
        let dir = tempfile::tempdir().unwrap();
        let info = GatewayInfo::new(dir.path().join("gateway.json"), 41434);
        assert_eq!(info.token().len(), 64);
        assert!(info.token().chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(
            info.token(),
            GatewayInfo::new(dir.path().join("gateway.json"), 41434).token()
        );
        let t = info.token().to_string();
        assert!(info.matches(Some(&format!("Bearer {t}"))));
        assert!(info.matches(Some(&format!("Bearer  {t} "))));
        assert!(info.matches(Some(&format!(" {t}"))), "an X-Api-Key value");
        assert!(
            !info.matches(Some(&format!("bearer {t}"))),
            "the prefix is case-sensitive"
        );
        assert!(!info.matches(Some("Bearer nope")));
        assert!(!info.matches(Some(&t[..63])));
        assert!(!info.matches(None));
        assert_eq!(info.base_url(), "http://127.0.0.1:41434");
    }

    #[test]
    fn gateway_json_is_written_and_removed_only_while_it_is_ours() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("home").join("gateway.json");
        let info = GatewayInfo::new(file.clone(), 41500);
        info.write_file();
        let written: GatewayFile =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            written,
            GatewayFile {
                base_url: "http://127.0.0.1:41500".into(),
                port: 41500,
                token: info.token().to_string(),
                openai_base_url: "http://127.0.0.1:41500/v1".into(),
            }
        );
        let raw = std::fs::read_to_string(&file).unwrap();
        assert!(
            raw.find("baseUrl").unwrap() < raw.find("openaiBaseUrl").unwrap(),
            "{raw}"
        );
        assert!(!file.with_extension("json.tmp").exists());

        let newer = GatewayInfo::new(file.clone(), 41501);
        newer.write_file();
        info.remove_file();
        assert!(file.exists(), "another start's file stays");
        newer.remove_file();
        assert!(!file.exists());
        newer.remove_file(); // already gone: nothing to do
    }
}

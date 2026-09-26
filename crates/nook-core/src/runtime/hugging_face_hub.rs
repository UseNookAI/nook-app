//! Ports `runtime/HuggingFaceHub.java`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

use super::downloader::{Downloader, Outcome, RUNTIME_USER_AGENT};
use super::model_catalog::{long, text};
use super::model_registry::{now_iso, sidecar_of, write_json};
use super::Progress;
use crate::home::Home;

/// The Hub.
pub const API: &str = "https://huggingface.co";
/// Working memory a loaded model needs beyond its weights: context, compute buffers, driver.
pub const OVERHEAD_BYTES: u64 = 700 << 20;

/// A repository as the search lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Repo {
    pub id: String,
    pub author: String,
    pub name: String,
    pub downloads: i64,
    pub likes: i64,
    pub last_modified: Option<DateTime<Utc>>,
    pub gated: bool,
    pub pipeline_tag: Option<String>,
    pub tags: Vec<String>,
}

impl Repo {
    pub fn looks_like_embedding(&self) -> bool {
        let name = self.name.to_lowercase();
        matches!(
            self.pipeline_tag.as_deref(),
            Some("feature-extraction" | "sentence-similarity")
        ) || name.contains("embed")
            || name.contains("bge")
    }
}

/// One GGUF file in a repository; shards of a split model share a base name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubFile {
    pub path: String,
    pub bytes: u64,
    pub sha256: Option<String>,
    pub quant: String,
    pub shard_index: u32,
    pub shard_count: u32,
    pub shard_base: String,
}

impl HubFile {
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// One downloadable choice: a quantisation, possibly in several shards. `key` is the file name
/// without shard suffix, unique within a repository even when two model files share a
/// quantisation label.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variant {
    pub label: String,
    pub key: String,
    pub files: Vec<HubFile>,
    pub total_bytes: u64,
}

impl Variant {
    pub fn multi_part(&self) -> bool {
        self.files.len() > 1
    }

    pub fn size_gb(&self) -> f64 {
        self.total_bytes as f64 / 1e9
    }
}

/// Whether a model runs comfortably on the largest GPU. Serialized as the Java constant name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Fit {
    Fits,
    Tight,
    Offload,
    NoGpu,
}

/// The open model universe: search Hugging Face for GGUF repositories, list their quantisations
/// with sizes and a fit-on-this-GPU estimate, and download one into the models folder with resume
/// and sha256 from the Hub's LFS metadata. A downloaded GGUF becomes an installed model through
/// the same sidecar the registry writes for catalog downloads; its task (chat or embed) comes
/// from the GGUF header on the first scan.
pub struct HuggingFaceHub {
    client: reqwest::Client,
    api: String,
    home: Home,
    shared_dir: Option<PathBuf>,
    downloader: Arc<Downloader>,
}

impl HuggingFaceHub {
    pub fn new(home: Home, downloader: Arc<Downloader>) -> HuggingFaceHub {
        let shared = home.shared_models_dir();
        HuggingFaceHub::with_api(API, home, downloader, shared)
    }

    /// A hub against another server (tests) with an explicit read-only models folder.
    pub fn with_api(
        api: &str,
        home: Home,
        downloader: Arc<Downloader>,
        shared_dir: Option<PathBuf>,
    ) -> HuggingFaceHub {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .connect_timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("Hub client could not be configured ({e}); using the defaults");
                reqwest::Client::new()
            });
        HuggingFaceHub {
            client,
            api: api.trim_end_matches('/').to_string(),
            home,
            shared_dir,
            downloader,
        }
    }

    // ------------------------------------------------------------------ search

    /// GGUF repositories matching a query, most downloaded first.
    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<Repo>> {
        let q = query.trim();
        let mut url = format!(
            "{}/api/models?filter=gguf&sort=downloads&direction=-1&limit={}",
            self.api,
            limit.clamp(1, 50)
        );
        if !q.is_empty() {
            url.push_str("&search=");
            url.extend(url::form_urlencoded::byte_serialize(q.as_bytes()));
        }
        Ok(repos_from(&self.get(&url).await?))
    }

    /// The GGUF variants of a repository, grouped by quantisation, smallest first.
    pub async fn variants(&self, repo_id: &str) -> Result<Vec<Variant>> {
        let tree = self
            .get(&format!(
                "{}/api/models/{repo_id}/tree/main?recursive=true",
                self.api
            ))
            .await?;
        Ok(variants_from(&tree))
    }

    // ------------------------------------------------------------------ download

    /// Where a repository's files live: `models\hub\<author>-<name>\`.
    pub fn folder_for(&self, repo_id: &str) -> PathBuf {
        self.home
            .models_dir()
            .join("hub")
            .join(folder_name(repo_id))
    }

    /// True when every file of the variant is already present, in this app's folder or in the
    /// installed Nook's.
    pub fn is_installed(&self, repo_id: &str, v: &Variant) -> bool {
        let all_in = |folder: &Path| v.files.iter().all(|f| folder.join(f.file_name()).is_file());
        all_in(&self.folder_for(repo_id))
            || self
                .shared_dir
                .as_ref()
                .is_some_and(|d| all_in(&d.join("hub").join(folder_name(repo_id))))
    }

    /// Downloads every shard of a variant with resume and verification, then writes sidecars so
    /// the registry lists the model once (shards after the first are marked as components).
    /// `progress` gets `(done, total)` over all shards.
    ///
    /// Returns true when installed, false when cancelled.
    pub async fn download(
        &self,
        repo: &Repo,
        v: &Variant,
        progress: Option<Progress>,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        let folder = self.folder_for(&repo.id);
        let total = v.total_bytes;
        let mut before = 0u64;
        for (i, f) in v.files.iter().enumerate() {
            let target = folder.join(f.file_name());
            let url = format!("{}/{}/resolve/main/{}", self.api, repo.id, f.path);
            let offset = before;
            let per_file: Option<Progress> = progress.clone().map(|p| {
                let p: Progress = Arc::new(move |done, _| p(offset + done, total));
                p
            });
            let outcome = self
                .downloader
                .download(
                    &url,
                    &target,
                    f.sha256.as_deref(),
                    f.bytes,
                    per_file.as_ref(),
                    cancel,
                )
                .await?;
            if outcome == Outcome::Cancelled {
                return Ok(false);
            }
            before += tokio::fs::metadata(&target)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            write_sidecar(&target, repo, v, f, i == 0)?;
        }
        tracing::info!("Hub model {} ({}) installed", repo.id, v.label);
        Ok(true)
    }

    // ------------------------------------------------------------------ http

    async fn get(&self, url: &str) -> Result<Value> {
        let r = self
            .client
            .get(url)
            .timeout(Duration::from_secs(30))
            .header(reqwest::header::USER_AGENT, RUNTIME_USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| anyhow!("Could not reach Hugging Face: {e}"))?;
        let status = r.status().as_u16();
        if status == 401 || status == 403 {
            bail!("Hugging Face refused the request ({status}); the repository may be gated.");
        }
        if status == 404 {
            bail!("Not found on Hugging Face.");
        }
        if status / 100 != 2 {
            bail!("Hugging Face answered {status}");
        }
        let body = r
            .text()
            .await
            .map_err(|e| anyhow!("Could not read Hugging Face's answer: {e}"))?;
        Ok(serde_json::from_str(&body)?)
    }
}

fn folder_name(repo_id: &str) -> String {
    static UNSAFE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"[^A-Za-z0-9._-]").expect("folder pattern"));
    UNSAFE
        .replace_all(&repo_id.replace('/', "-"), "_")
        .into_owned()
}

/// Reads the search answer (`/api/models`) into repositories.
pub fn repos_from(arr: &Value) -> Vec<Repo> {
    let mut out = Vec::new();
    for n in arr.as_array().into_iter().flatten() {
        let id = text(n.get("id"))
            .or_else(|| text(n.get("modelId")))
            .unwrap_or_default();
        if id.trim().is_empty() {
            continue;
        }
        let (author, name) = match id.find('/') {
            Some(slash) if slash > 0 => (id[..slash].to_string(), id[slash + 1..].to_string()),
            _ => (String::new(), id.clone()),
        };
        let gated = match n.get("gated") {
            Some(Value::Bool(b)) => *b,
            other => text(other).unwrap_or_else(|| "false".into()) != "false",
        };
        out.push(Repo {
            author,
            name,
            downloads: long(n.get("downloads")).unwrap_or(0),
            likes: long(n.get("likes")).unwrap_or(0),
            last_modified: text(n.get("lastModified"))
                .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|d| d.with_timezone(&Utc)),
            gated,
            pipeline_tag: text(n.get("pipeline_tag")),
            tags: n
                .get("tags")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|t| text(Some(t)).unwrap_or_default())
                .collect(),
            id,
        });
    }
    out
}

static SHARD: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)-(\d{5})-of-(\d{5})\.gguf$").expect("shard pattern"));
/// One quantisation token, matched whole. The original's pattern ends in a lookahead, which the
/// regex crate lacks; [`quant_of`] does the lookahead's work by hand.
static QUANT_TOKEN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^(?:(?:I?Q\d(?:_[A-Z0-9]+)*)|F16|F32|BF16|FP16|MXFP4|TQ\d_\d)$")
        .expect("quant pattern")
});

/// Groups a repository tree listing into variants; split models need every shard present.
pub fn variants_from(tree: &Value) -> Vec<Variant> {
    let mut files = Vec::new();
    for n in tree.as_array().into_iter().flatten() {
        let path = text(n.get("path")).unwrap_or_default();
        if !path.to_lowercase().ends_with(".gguf")
            || text(n.get("type")).as_deref().unwrap_or("file") != "file"
        {
            continue;
        }
        let lfs = n.get("lfs");
        let bytes = long(n.get("size"))
            .or_else(|| long(lfs.and_then(|l| l.get("size"))))
            .unwrap_or(0)
            .max(0) as u64;
        let sha = text(lfs.and_then(|l| l.get("oid")));
        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
        if is_side_file(&name) {
            continue;
        }
        let (mut index, mut count, mut base) = (0u32, 1u32, name.clone());
        if let Some(m) = SHARD.captures(&name) {
            if let (Ok(i), Ok(c)) = (m[1].parse(), m[2].parse()) {
                index = i;
                count = c;
                base = format!(
                    "{}.gguf",
                    &name[..m.get(0).map_or(name.len(), |g| g.start())]
                );
            }
        }
        files.push(HubFile {
            path,
            bytes,
            sha256: sha,
            quant: quant_of(&base),
            shard_index: index,
            shard_count: count,
            shard_base: base,
        });
    }
    // Grouped by base name in the order first seen.
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<HubFile>> = BTreeMap::new();
    for f in files {
        if !groups.contains_key(&f.shard_base) {
            order.push(f.shard_base.clone());
        }
        groups.entry(f.shard_base.clone()).or_default().push(f);
    }
    let mut out = Vec::new();
    for key in order {
        let Some(mut shards) = groups.remove(&key) else {
            continue;
        };
        shards.sort_by_key(|f| f.shard_index);
        let expected = shards[0].shard_count as usize;
        if shards.len() != expected {
            continue; // an incomplete split upload; nothing to run
        }
        let total = shards.iter().map(|f| f.bytes).sum();
        let label = shards[0].quant.clone();
        out.push(Variant {
            label: if label.is_empty() { key.clone() } else { label },
            key,
            files: shards,
            total_bytes: total,
        });
    }
    out.sort_by_key(|v| v.total_bytes);
    out
}

/// Files that sit beside a model but are not one: vision projectors and encoders, multi-token
/// prediction heads, importance matrices, adapters, drafts. Offered as a variant, a vision encoder
/// (`DeepSeek-V4-Flash-Vision-Encoder.gguf`, 932 MB) was downloaded as the model and made the Code
/// worker on 2026-09-26. "Vision" alone is not one: `Llama-3.2-11B-Vision-Instruct` is the model.
/// A name cannot tell everything (a support file for another engine looks like a model); the
/// registry reads the header of what was downloaded.
pub fn is_side_file(name: &str) -> bool {
    let n = name.to_lowercase();
    let tokens: Vec<&str> = n
        .trim_end_matches(".gguf")
        .split(|c: char| !c.is_ascii_alphanumeric())
        .collect();
    n.contains("mmproj")
        || n.contains("projector")
        || n.contains("encoder")
        || tokens.contains(&"mtp")
        || n.contains("imatrix")
        || n.contains("-lora")
        || n.starts_with("lora")
        || n.contains("draft")
}

/// "Q4_K_M" from "Qwen3-8B-Q4_K_M.gguf"; empty when the name carries no quantisation. The last
/// token that starts the name or follows a `-`, `_` or `.` and ends the name or comes before one.
pub fn quant_of(file_name: &str) -> String {
    let cut = file_name.len().saturating_sub(5);
    let stem = match file_name.get(cut..) {
        Some(ext) if ext.eq_ignore_ascii_case(".gguf") => &file_name[..cut],
        _ => file_name,
    };
    let is_sep = |c: char| c == '-' || c == '_' || c == '.';
    // Where a token may end: before a separator, or at the end.
    let ends: Vec<usize> = stem
        .char_indices()
        .filter(|(_, c)| is_sep(*c))
        .map(|(i, _)| i)
        .chain(std::iter::once(stem.len()))
        .collect();
    let longest_from = |start: usize| -> Option<usize> {
        ends.iter()
            .rev()
            .copied()
            .filter(|&e| e > start)
            .find(|&e| QUANT_TOKEN.is_match(&stem[start..e]))
    };
    let mut last = String::new();
    let mut pos = 0usize;
    'search: while pos <= stem.len() {
        for (s, c) in stem[pos..]
            .char_indices()
            .map(|(i, c)| (i + pos, Some(c)))
            .chain(std::iter::once((stem.len(), None)))
        {
            let mut starts = Vec::with_capacity(2);
            if s == 0 {
                starts.push(0);
            }
            if let Some(c) = c.filter(|c| is_sep(*c)) {
                starts.push(s + c.len_utf8());
            }
            for start in starts {
                if let Some(end) = longest_from(start) {
                    last = stem[start..end].to_uppercase();
                    pos = end;
                    continue 'search;
                }
            }
        }
        break;
    }
    last
}

/// Whether a model of this size runs comfortably on the largest GPU.
pub fn fit(bytes: u64, gpu_total_bytes: u64) -> Fit {
    if gpu_total_bytes == 0 {
        return Fit::NoGpu;
    }
    let need = (bytes as f64 * 1.05) as u64 + OVERHEAD_BYTES;
    if need as f64 <= gpu_total_bytes as f64 * 0.92 {
        Fit::Fits
    } else if need as f64 <= gpu_total_bytes as f64 * 1.15 {
        Fit::Tight
    } else {
        Fit::Offload
    }
}

static GGUF_SUFFIX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)[-_]?gguf$").expect("gguf suffix pattern"));

fn quant_named(name: &str, v: &Variant) -> bool {
    !v.label.is_empty() && name.to_lowercase().contains(&v.label.to_lowercase())
}

/// A stable local id: the repo name plus quantisation, lower case; the quant is not repeated
/// when the name has it.
pub fn model_id(repo: &Repo, v: &Variant) -> String {
    static NON_ID: Lazy<Regex> = Lazy::new(|| Regex::new(r"[^a-z0-9.]+").expect("id pattern"));
    static NON_ALNUM: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"[^a-z0-9]+").expect("quant id pattern"));
    let name = GGUF_SUFFIX.replace(&repo.name, "");
    let named = quant_named(&name, v);
    let base = NON_ID
        .replace_all(&name.to_lowercase(), "-")
        .trim_matches('-')
        .to_string();
    let q = NON_ALNUM
        .replace_all(&v.label.to_lowercase(), "")
        .into_owned();
    if q.is_empty() || named {
        base
    } else {
        format!("{base}-{q}")
    }
}

pub fn display_name(repo: &Repo, v: &Variant) -> String {
    let name = GGUF_SUFFIX.replace(&repo.name, "");
    if v.label.is_empty() || quant_named(&name, v) {
        name.into_owned()
    } else {
        format!("{name} {}", v.label)
    }
}

/// "36 MB" or "5.0 GB".
pub fn size(bytes: u64) -> String {
    if bytes < 1_000_000_000 {
        format!("{} MB", (bytes as f64 / 1e6).round() as u64)
    } else {
        format!("{:.1} GB", bytes as f64 / 1e9)
    }
}

fn write_sidecar(file: &Path, repo: &Repo, v: &Variant, f: &HubFile, primary: bool) -> Result<()> {
    let mut n = Map::new();
    n.insert("id".into(), json!(model_id(repo, v)));
    n.insert(
        "role".into(),
        json!(if primary { "model" } else { "component" }),
    );
    n.insert("displayName".into(), json!(display_name(repo, v)));
    n.insert("family".into(), json!(repo.name.to_lowercase()));
    n.insert(
        "task".into(),
        json!(if repo.looks_like_embedding() {
            "embed"
        } else {
            "chat"
        }),
    );
    n.insert("source".into(), json!("huggingface"));
    n.insert("repo".into(), json!(repo.id));
    n.insert("quant".into(), json!(v.label));
    if let Some(sha) = &f.sha256 {
        n.insert("sha256".into(), json!(sha));
    }
    n.insert("bytes".into(), json!(std::fs::metadata(file)?.len()));
    n.insert("downloadedAt".into(), json!(now_iso()));
    write_json(&sidecar_of(file), &Value::Object(n))
}

/// Parsing the Hub's answers: quantisation names, shard grouping, fit and local ids.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::downloader::tests::serve;
    use crate::runtime::model_catalog::ModelCatalog;
    use crate::runtime::model_registry::ModelRegistry;
    use axum::extract::{Path as AxPath, RawQuery};
    use axum::routing::get;
    use axum::{Json, Router};
    use parking_lot::Mutex;
    use sha2::Digest;

    fn repo(id: &str, pipeline: Option<&str>) -> Repo {
        let (author, name) = id.split_once('/').unwrap();
        Repo {
            id: id.into(),
            author: author.into(),
            name: name.into(),
            downloads: 1,
            likes: 1,
            last_modified: None,
            gated: false,
            pipeline_tag: pipeline.map(str::to_string),
            tags: vec![],
        }
    }

    fn variant(label: &str, key: &str, total: u64) -> Variant {
        Variant {
            label: label.into(),
            key: key.into(),
            files: vec![],
            total_bytes: total,
        }
    }

    #[test]
    fn quantisation_comes_from_the_file_name() {
        assert_eq!(quant_of("Qwen3-8B-Q4_K_M.gguf"), "Q4_K_M");
        assert_eq!(quant_of("Llama-3.3-70B-Instruct-IQ4_XS.gguf"), "IQ4_XS");
        assert_eq!(quant_of("nomic-embed-text-v1.5.Q8_0.gguf"), "Q8_0");
        assert_eq!(quant_of("model-f16.gguf"), "F16");
        assert_eq!(quant_of("gemma-3-4b-it-BF16.gguf"), "BF16");
        assert_eq!(quant_of("ggml-model.gguf"), "");
        // The lookahead's cases: the last token wins, and a token stops before a non-separator.
        assert_eq!(quant_of("gpt-oss-20b-MXFP4.gguf"), "MXFP4");
        assert_eq!(quant_of("Q8_0-model-q4_k_m.gguf"), "Q4_K_M");
        assert_eq!(
            quant_of("x-Q4_K+y.gguf"),
            "Q4",
            "the longest prefix that ends before a separator"
        );
        assert_eq!(quant_of("model-TQ1_0.gguf"), "TQ1_0");
        assert_eq!(
            quant_of("Q4_0"),
            "Q4_0",
            "a name without the extension works too"
        );
    }

    #[test]
    fn shards_group_into_one_variant_and_incomplete_splits_are_dropped() {
        let tree: Value = serde_json::from_str(
            r#"[
              {"type":"file","path":"README.md","size":10},
              {"type":"file","path":"Big-Q4_K_M-00001-of-00002.gguf","size":100,"lfs":{"oid":"aa","size":100}},
              {"type":"file","path":"Big-Q4_K_M-00002-of-00002.gguf","size":50,"lfs":{"oid":"bb","size":50}},
              {"type":"file","path":"Big-Q8_0-00001-of-00003.gguf","size":100,"lfs":{"oid":"cc","size":100}},
              {"type":"file","path":"small/Big-Q2_K.gguf","size":30,"lfs":{"oid":"dd","size":30}},
              {"type":"file","path":"mmproj-F16.gguf","size":900,"lfs":{"oid":"ee","size":900}},
              {"type":"file","path":"imatrix_unsloth.gguf","size":5,"lfs":{"oid":"ff","size":5}}
            ]"#,
        )
        .unwrap();
        let v = variants_from(&tree);
        assert_eq!(
            v.len(),
            2,
            "the incomplete split, the projector and the importance matrix are left out"
        );
        assert_eq!(v[0].label, "Q2_K");
        assert_eq!(v[0].key, "Big-Q2_K.gguf");
        assert_eq!(v[0].total_bytes, 30);
        assert!(!v[0].multi_part());
        assert_eq!(v[1].label, "Q4_K_M");
        assert_eq!(v[1].total_bytes, 150);
        assert!(v[1].multi_part());
        assert_eq!(v[1].files[0].shard_index, 1);
        assert_eq!(v[1].files[0].file_name(), "Big-Q4_K_M-00001-of-00002.gguf");
        assert_eq!(v[1].files[0].sha256.as_deref(), Some("aa"));
    }

    /// The files beside a model are not offered as one (antirez/deepseek-v4-gguf, 2026-09-26),
    /// while a vision-language model whose name says "Vision" still is.
    #[test]
    fn projectors_encoders_and_prediction_heads_are_not_variants() {
        for side in [
            "DeepSeek-V4-Flash-Vision-Encoder.gguf",
            "DeepSeek-V4-Flash-MTP-Q4K-Q8_0-F32.gguf",
            "mtp-Qwen3.8-Flash-Next-shared-Q4_K_M.gguf",
            "mmproj-F16.gguf",
            "Qwen2.5-VL-7B-Instruct-mmproj-f16.gguf",
            "gemma-3-4b-vision-projector-f16.gguf",
            "umt5-xxl-encoder-Q5_K_M.gguf",
            "Qwen3-0.6B-draft-Q8_0.gguf",
            "imatrix_unsloth.gguf",
            "Llama-3-8B-lora-f16.gguf",
            "LoRA-adapter.gguf",
        ] {
            assert!(is_side_file(side), "{side}");
        }
        for model in [
            "Llama-3.2-11B-Vision-Instruct-Q4_K_M.gguf",
            "Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf",
            "DeepSeek-V4-Flash-Q2_K-00001-of-00004.gguf",
            "Qwen3-8B-Q4_K_M.gguf",
            "gpt-oss-20b-MXFP4.gguf",
            "SMTP-Helper-7B-Q4_K_M.gguf",
        ] {
            assert!(!is_side_file(model), "{model}");
        }

        let tree: Value = serde_json::from_str(
            r#"[
              {"type":"file","path":"DeepSeek-V4-Flash-Vision-Encoder.gguf","size":932857760},
              {"type":"file","path":"DeepSeek-V4-Flash-MTP-Q4K-Q8_0-F32.gguf","size":3800000000},
              {"type":"file","path":"DeepSeek-V4-Flash-Q2_K.gguf","size":87000000000},
              {"type":"file","path":"vision/Llama-3.2-11B-Vision-Instruct-Q4_K_M.gguf","size":6000000000},
              {"type":"file","path":"vision/Llama-3.2-11B-Vision-Instruct-mmproj-f16.gguf","size":1900000000}
            ]"#,
        )
        .unwrap();
        let keys: Vec<String> = variants_from(&tree).into_iter().map(|v| v.key).collect();
        assert_eq!(
            keys,
            [
                "Llama-3.2-11B-Vision-Instruct-Q4_K_M.gguf",
                "DeepSeek-V4-Flash-Q2_K.gguf"
            ]
        );
    }

    #[test]
    fn fit_is_judged_against_the_card() {
        let eight_gb = 8u64 << 30;
        assert_eq!(fit(5 << 30, eight_gb), Fit::Fits);
        assert_eq!(fit(7_500 << 20, eight_gb), Fit::Tight);
        assert_eq!(fit(12 << 30, eight_gb), Fit::Offload);
        assert_eq!(fit(1 << 30, 0), Fit::NoGpu);
        assert_eq!(serde_json::to_string(&Fit::NoGpu).unwrap(), "\"NO_GPU\"");
    }

    #[test]
    fn ids_and_names_are_stable() {
        let r = repo("bartowski/Qwen_Qwen3-14B-GGUF", Some("text-generation"));
        let v = variant("Q4_K_M", "x.gguf", 0);
        assert_eq!(
            model_id(&r, &v),
            "qwen-qwen3-14b-q4km",
            "the GGUF suffix drops out of the id"
        );
        assert_eq!(display_name(&r, &v), "Qwen_Qwen3-14B Q4_K_M");
        assert!(!r.looks_like_embedding());
        assert!(repo("x/bge-m3-GGUF", None).looks_like_embedding());
        // A repository named after its quantisation does not get it twice.
        let q8 = repo("ggml-org/bge-small-en-v1.5-Q8_0-GGUF", None);
        let v8 = variant("Q8_0", "bge-small-en-v1.5-q8_0.gguf", 36_685_152);
        assert_eq!(model_id(&q8, &v8), "bge-small-en-v1.5-q8-0");
        assert_eq!(display_name(&q8, &v8), "bge-small-en-v1.5-Q8_0");
        assert_eq!(size(36_685_152), "37 MB");
        assert_eq!(size(5_027_783_488), "5.0 GB");
    }

    #[tokio::test]
    async fn searches_lists_variants_and_downloads_into_the_registry() {
        // A real (tiny) GGUF: the registry reads a language model's header on its first scan.
        let fixtures = tempfile::tempdir().unwrap();
        let model = std::fs::read(
            crate::runtime::gguf_metadata::testing::write_language_model(
                fixtures.path(),
                "test.gguf",
                "bert",
                &[],
                0,
            ),
        )
        .unwrap();
        let sha = hex::encode(sha2::Sha256::digest(&model));
        let queries = Arc::new(Mutex::new(Vec::<String>::new()));
        let q = queries.clone();
        let tree = json!([
            {"type":"file","path":"Tiny-Embed-Q8_0.gguf","size":model.len(),"lfs":{"oid":sha,"size":model.len()}},
            {"type":"directory","path":"sub"}
        ]);
        let m = model.clone();
        let router = Router::new()
            .route(
                "/api/models",
                get(move |RawQuery(query): RawQuery| {
                    let q = q.clone();
                    async move {
                        q.lock().push(query.unwrap_or_default());
                        Json(json!([
                            {"id":"someone/Tiny-Embed-GGUF","downloads":1234,"likes":5,"lastModified":"2026-09-01T10:00:00.000Z",
                             "gated":false,"pipeline_tag":"feature-extraction","tags":["gguf","embeddings"]},
                            {"modelId":"other/Gated-GGUF","gated":"manual"},
                            {"id":""}
                        ]))
                    }
                }),
            )
            .route("/api/models/{author}/{name}/tree/main", get(move || { let t = tree.clone(); async move { Json(t) } }))
            .route(
                "/{author}/{name}/resolve/main/{file}",
                get(move |AxPath((_, _, file)): AxPath<(String, String, String)>| {
                    let m = m.clone();
                    async move { if file == "Tiny-Embed-Q8_0.gguf" { Ok(m) } else { Err(axum::http::StatusCode::NOT_FOUND) } }
                }),
            );
        let base = serve(router).await;
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        home.ensure_layout().unwrap();
        let downloader = Arc::new(Downloader::new());
        let hub = HuggingFaceHub::with_api(&base, home.clone(), downloader.clone(), None);

        let repos = hub.search("  tiny embed ", 500).await.unwrap();
        assert_eq!(
            queries.lock()[0],
            "filter=gguf&sort=downloads&direction=-1&limit=50&search=tiny+embed"
        );
        assert_eq!(repos.len(), 2, "an entry without an id is skipped");
        let r = &repos[0];
        assert_eq!(
            (r.author.as_str(), r.name.as_str()),
            ("someone", "Tiny-Embed-GGUF")
        );
        assert_eq!(r.downloads, 1234);
        assert!(r.last_modified.is_some());
        assert!(r.looks_like_embedding());
        assert!(repos[1].gated, "\"manual\" gating counts as gated");
        assert_eq!(repos[1].id, "other/Gated-GGUF");

        let variants = hub.variants(&r.id).await.unwrap();
        assert_eq!(variants.len(), 1);
        let v = &variants[0];
        assert_eq!(v.label, "Q8_0");
        assert!(!hub.is_installed(&r.id, v));
        assert!(hub
            .download(r, v, None, &CancellationToken::new())
            .await
            .unwrap());
        assert!(hub.is_installed(&r.id, v));
        let file = hub.folder_for(&r.id).join("Tiny-Embed-Q8_0.gguf");
        assert_eq!(
            file,
            dir.path()
                .join("models")
                .join("hub")
                .join("someone-Tiny-Embed-GGUF")
                .join("Tiny-Embed-Q8_0.gguf")
        );
        let sidecar: Value =
            serde_json::from_slice(&std::fs::read(sidecar_of(&file)).unwrap()).unwrap();
        assert_eq!(sidecar["id"], "tiny-embed-q80");
        assert_eq!(sidecar["task"], "embed");
        assert_eq!(sidecar["source"], "huggingface");
        assert_eq!(sidecar["repo"], "someone/Tiny-Embed-GGUF");
        assert_eq!(sidecar["sha256"], sha.as_str());

        let registry = ModelRegistry::with_shared_dir(
            home,
            Arc::new(ModelCatalog::bundled().unwrap()),
            downloader,
            None,
        );
        let listed = registry
            .find("tiny-embed-q80")
            .expect("the hub download is an installed model");
        assert!(listed.is_embedding());
        assert_eq!(listed.display_name, "Tiny-Embed Q8_0");

        let missing = HuggingFaceHub::with_api(
            &format!("{base}/nothing-here"),
            Home::at(dir.path()),
            Arc::new(Downloader::new()),
            None,
        );
        assert_eq!(
            missing.search("x", 5).await.unwrap_err().to_string(),
            "Not found on Hugging Face."
        );
    }

    #[test]
    fn a_variant_in_the_installed_nooks_folder_counts_as_installed() {
        let dir = tempfile::tempdir().unwrap();
        let shared = tempfile::tempdir().unwrap();
        let hub = HuggingFaceHub::with_api(
            API,
            Home::at(dir.path()),
            Arc::new(Downloader::new()),
            Some(shared.path().to_path_buf()),
        );
        let v = Variant {
            label: "Q4_K_M".into(),
            key: "M-Q4_K_M.gguf".into(),
            files: vec![HubFile {
                path: "M-Q4_K_M.gguf".into(),
                bytes: 1,
                sha256: None,
                quant: "Q4_K_M".into(),
                shard_index: 0,
                shard_count: 1,
                shard_base: "M-Q4_K_M.gguf".into(),
            }],
            total_bytes: 1,
        };
        assert!(!hub.is_installed("a/M-GGUF", &v));
        let folder = shared.path().join("hub").join("a-M-GGUF");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("M-Q4_K_M.gguf"), b"x").unwrap();
        assert!(hub.is_installed("a/M-GGUF", &v));
    }
}

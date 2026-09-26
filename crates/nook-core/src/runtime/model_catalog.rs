//! Ports `runtime/ModelCatalog.java`.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::engine_component::EngineComponent;

/// One file of a catalog model. `bytes` is 0 when the catalog does not say (the Java record
/// used -1); `format` defaults to `gguf`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub file: String,
    pub url: String,
    pub sha256: Option<String>,
    pub bytes: u64,
    pub format: String,
}

impl Artifact {
    pub fn is_gguf(&self) -> bool {
        self.format.eq_ignore_ascii_case("gguf")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub id: String,
    pub display_name: String,
    pub family: String,
    pub task: String,
    pub params_b: f64,
    pub description: String,
    pub capabilities: Vec<String>,
    pub artifacts: Vec<Artifact>,
    pub defaults: BTreeMap<String, String>,
    pub backends: BTreeMap<String, String>,
    pub license: String,
    pub min_vram_gb: i64,
}

impl CatalogModel {
    /// The model file itself; the other artifacts are its components (VAE, text encoder).
    pub fn primary_artifact(&self) -> Option<&Artifact> {
        self.artifacts.first()
    }

    pub fn total_bytes(&self) -> u64 {
        self.artifacts.iter().map(|a| a.bytes).sum()
    }

    pub fn size_gb(&self) -> f64 {
        self.total_bytes() as f64 / 1e9
    }

    pub fn is_chat(&self) -> bool {
        self.task == "chat"
    }
    pub fn is_embedding(&self) -> bool {
        self.task == "embed"
    }
    pub fn is_speech(&self) -> bool {
        self.task == "speech"
    }
    pub fn is_image(&self) -> bool {
        self.task == "image"
    }
    pub fn is_video(&self) -> bool {
        self.task == "video"
    }

    pub fn default_ctx(&self) -> u32 {
        self.defaults
            .get("nCtx")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(8192)
    }

    pub fn default_temperature(&self) -> f64 {
        self.defaults
            .get("temperature")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0.7)
    }

    pub fn component(&self) -> EngineComponent {
        EngineComponent::for_task(&self.task)
    }
}

/// The curated model catalog, bundled with the app (`runtime/catalog.json`). Tasks: chat, embed,
/// speech, image, video.
#[derive(Clone, Debug)]
pub struct ModelCatalog {
    models: Vec<CatalogModel>,
    default_chat_model: Option<String>,
    default_worker_model: Option<String>,
    default_speech_model: Option<String>,
    default_image_model: Option<String>,
    default_video_model: Option<String>,
}

impl ModelCatalog {
    /// The catalog built into the app.
    pub fn bundled() -> Result<ModelCatalog> {
        ModelCatalog::from_json(crate::resources::CATALOG_JSON)
            .context("Cannot read runtime/catalog.json")
    }

    /// A catalog from JSON in the format of `runtime/catalog.json`, read as leniently as Jackson
    /// did: missing fields take their defaults, numbers may be strings and the other way round.
    pub fn from_json(json: &str) -> Result<ModelCatalog> {
        let root: Value = serde_json::from_str(json)?;
        let mut models: Vec<CatalogModel> = Vec::new();
        for m in root
            .get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let id = text(m.get("id")).unwrap_or_default();
            let artifacts = m
                .get("artifacts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|a| Artifact {
                    file: text(a.get("file")).unwrap_or_default(),
                    url: text(a.get("url")).unwrap_or_default(),
                    sha256: text(a.get("sha256")),
                    bytes: long(a.get("bytes")).filter(|b| *b > 0).unwrap_or(0) as u64,
                    format: text(a.get("format")).unwrap_or_else(|| "gguf".to_string()),
                })
                .collect();
            let model = CatalogModel {
                display_name: text(m.get("displayName")).unwrap_or_else(|| id.clone()),
                family: text(m.get("family")).unwrap_or_default(),
                task: text(m.get("task")).unwrap_or_else(|| "chat".to_string()),
                params_b: double(m.get("paramsB")).unwrap_or(0.0),
                description: text(m.get("description")).unwrap_or_default(),
                capabilities: m
                    .get("capabilities")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|c| text(Some(c)).unwrap_or_default())
                    .collect(),
                artifacts,
                defaults: string_map(m.get("defaults")),
                backends: string_map(m.get("backends")),
                license: text(m.get("license")).unwrap_or_default(),
                min_vram_gb: long(m.get("minVramGb")).unwrap_or(0),
                id,
            };
            // A later entry with the same id replaces the earlier one in place (a LinkedHashMap put).
            match models.iter_mut().find(|x| x.id == model.id) {
                Some(existing) => *existing = model,
                None => models.push(model),
            }
        }
        Ok(ModelCatalog {
            models,
            default_chat_model: text(root.get("defaultChatModel")),
            default_worker_model: text(root.get("defaultWorkerModel")),
            default_speech_model: text(root.get("defaultSpeechModel")),
            default_image_model: text(root.get("defaultImageModel")),
            default_video_model: text(root.get("defaultVideoModel")),
        })
    }

    pub fn all(&self) -> &[CatalogModel] {
        &self.models
    }

    pub fn find(&self, id: &str) -> Option<&CatalogModel> {
        self.models.iter().find(|m| m.id == id)
    }

    /// The catalog model one of whose artifacts has this file name (ignoring case).
    pub fn find_by_file(&self, file_name: &str) -> Option<&CatalogModel> {
        self.models.iter().find(|m| {
            m.artifacts
                .iter()
                .any(|a| a.file.eq_ignore_ascii_case(file_name))
        })
    }

    pub fn default_chat_model(&self) -> Option<&str> {
        self.default_chat_model.as_deref()
    }

    /// The model Nook Code works with by default, when installed.
    pub fn default_worker_model(&self) -> Option<&str> {
        self.default_worker_model.as_deref()
    }

    /// Models fit to be Nook Code's worker (capability "worker"), the default first.
    pub fn worker_models(&self) -> Vec<&CatalogModel> {
        let mut out: Vec<&CatalogModel> = self
            .models
            .iter()
            .filter(|m| m.capabilities.iter().any(|c| c == "worker"))
            .collect();
        let default = self.default_worker_model.as_deref();
        out.sort_by_key(|m| Some(m.id.as_str()) != default);
        out
    }

    pub fn default_speech_model(&self) -> Option<&str> {
        self.default_speech_model.as_deref()
    }

    pub fn default_image_model(&self) -> Option<&str> {
        self.default_image_model.as_deref()
    }

    pub fn default_video_model(&self) -> Option<&str> {
        self.default_video_model.as_deref()
    }

    /// Chat models that fit the given budget, largest first.
    pub fn recommend_chat(&self, vram_bytes: u64) -> Vec<&CatalogModel> {
        let gb = (vram_bytes >> 30) as i64;
        let mut out: Vec<&CatalogModel> = self
            .models
            .iter()
            .filter(|m| m.is_chat() && m.min_vram_gb <= gb.max(0))
            .collect();
        out.sort_by(|a, b| b.params_b.total_cmp(&a.params_b));
        out
    }
}

// Jackson-style lenient readers shared with the registry and the Hub.

/// A value as text (`asText`): strings as they are, numbers and booleans spelled out; None when
/// missing or null. Objects and arrays read as "" as Jackson did.
pub(crate) fn text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(_) | Value::Object(_) => Some(String::new()),
    }
}

/// A value as a long (`asLong`): numbers truncated, numeric strings parsed.
pub(crate) fn long(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(*b as i64),
        _ => None,
    }
}

/// A value as a double (`asDouble`).
pub(crate) fn double(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// An object's fields as text.
fn string_map(v: Option<&Value>) -> BTreeMap<String, String> {
    v.and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), text(Some(v)).unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_catalog_is_well_formed() {
        let catalog = ModelCatalog::bundled().unwrap();
        let all = catalog.all();
        assert!(!all.is_empty());
        for m in all {
            assert!(!m.id.trim().is_empty());
            assert!(!m.artifacts.is_empty(), "{} has artifacts", m.id);
            for a in &m.artifacts {
                assert!(a.url.starts_with("https://"), "{}", a.file);
                let sha = a
                    .sha256
                    .as_deref()
                    .unwrap_or_else(|| panic!("{} has a sha256", a.file));
                assert_eq!(sha.len(), 64, "{} sha256 is hex", a.file);
                assert!(a.bytes > 0);
            }
        }
        let chat = catalog.default_chat_model().expect("a default chat model");
        assert!(catalog.find(chat).is_some());
    }

    #[test]
    fn the_default_video_model_carries_every_file_it_names() {
        let catalog = ModelCatalog::bundled().unwrap();
        let wan = catalog
            .find(catalog.default_video_model().unwrap())
            .unwrap();
        assert!(wan.is_video());
        assert_eq!(wan.component(), EngineComponent::Sd);
        // The text encoder and VAE the engine is pointed at are artifacts downloaded beside the model.
        let files: Vec<&str> = wan.artifacts.iter().map(|a| a.file.as_str()).collect();
        assert!(files.contains(&wan.defaults["t5xxl"].as_str()));
        assert!(files.contains(&wan.defaults["vae"].as_str()));
    }

    #[test]
    fn recommends_only_models_that_fit() {
        let catalog = ModelCatalog::bundled().unwrap();
        let eight_gb = catalog.recommend_chat(8 << 30);
        assert!(eight_gb.iter().any(|m| m.id == "qwen3-8b-q4km"));
        assert!(eight_gb.iter().all(|m| !m.is_embedding()));

        let four_gb = catalog.recommend_chat(4 << 30);
        assert!(four_gb.iter().all(|m| m.min_vram_gb <= 4));
        assert!(four_gb.iter().any(|m| m.id == "qwen3-4b-q4km"));
    }

    #[test]
    fn finds_by_artifact_file_name() {
        let catalog = ModelCatalog::bundled().unwrap();
        assert_eq!(
            catalog
                .find_by_file("Qwen3-8B-Q4_K_M.gguf")
                .map(|m| m.id.as_str()),
            Some("qwen3-8b-q4km")
        );
        assert_eq!(
            catalog
                .find_by_file("qwen3-8b-q4_k_m.GGUF")
                .map(|m| m.id.as_str()),
            Some("qwen3-8b-q4km")
        );
        assert!(catalog.find_by_file("nope.gguf").is_none());
    }

    #[test]
    fn workers_come_default_first_and_defaults_read_as_text() {
        let catalog = ModelCatalog::bundled().unwrap();
        let workers = catalog.worker_models();
        assert!(!workers.is_empty());
        assert_eq!(Some(workers[0].id.as_str()), catalog.default_worker_model());
        let qwen = catalog.find("qwen3-8b-q4km").unwrap();
        assert_eq!(qwen.default_ctx(), 8192);
        assert_eq!(qwen.default_temperature(), 0.7);
        assert_eq!(qwen.defaults["temperature"], "0.7");
        let json = serde_json::to_value(qwen).unwrap();
        assert_eq!(json["displayName"], "Qwen3 8B");
        assert_eq!(json["minVramGb"], 6);
        assert_eq!(json["paramsB"], 8.2);
    }

    #[test]
    fn missing_fields_take_the_originals_defaults() {
        let c = ModelCatalog::from_json(
            r#"{"models":[{"id":"x","artifacts":[{"file":"x.bin","url":"u"}]}]}"#,
        )
        .unwrap();
        let m = c.find("x").unwrap();
        assert_eq!(m.display_name, "x");
        assert_eq!(m.task, "chat");
        assert_eq!(m.artifacts[0].format, "gguf");
        assert_eq!(m.artifacts[0].bytes, 0);
        assert_eq!(m.artifacts[0].sha256, None);
        assert_eq!(m.default_ctx(), 8192);
        assert_eq!(c.default_chat_model(), None);
    }
}

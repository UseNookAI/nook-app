//! Ports `runtime/SpeedProbe.java`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{json, Value};

use super::inference_client::InferenceClient;
use super::model_catalog::{double, long, text};

/// Below this a Nook Code turn takes longer than a person will wait for.
pub const MIN_WORKER_TPS: f64 = 8.0;
/// Tokens the probe asks the model to write.
pub const ANSWER_TOKENS: u32 = 300;
const PROMPT_SENTENCE: &str =
    "Gradle organises a build as a graph of tasks; each task declares its inputs and outputs so that unchanged work is skipped. ";

/// One measurement (`SpeedProbe.Result`).
///
/// - `model_id`: the catalog or local id
/// - `sha256`: the model file's checksum when known, else its size (`bytes:<n>`): the pin
/// - `driver`: the GPU driver version, or `cpu`; None when the device does not report one
/// - `backend`: cuda, vulkan or cpu
/// - `gpu_layers`: layers that were on the GPU when measured (-1 for automatic)
/// - `prompt_tps`: prompt processing, tokens per second
/// - `generate_tps`: generation, tokens per second
///
/// Serialized with camelCase fields plus `fastEnoughToWork`, which the Runtime page shows.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub model_id: String,
    pub sha256: Option<String>,
    pub driver: Option<String>,
    pub backend: Option<String>,
    pub gpu_layers: i32,
    pub prompt_tps: f64,
    pub generate_tps: f64,
    pub measured_at: DateTime<Utc>,
}

impl ProbeResult {
    pub fn fast_enough_to_work(&self) -> bool {
        self.generate_tps >= MIN_WORKER_TPS
    }
}

impl Serialize for ProbeResult {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Shown<'a> {
            model_id: &'a str,
            sha256: &'a Option<String>,
            driver: &'a Option<String>,
            backend: &'a Option<String>,
            gpu_layers: i32,
            prompt_tps: f64,
            generate_tps: f64,
            measured_at: &'a DateTime<Utc>,
            fast_enough_to_work: bool,
        }
        Shown {
            model_id: &self.model_id,
            sha256: &self.sha256,
            driver: &self.driver,
            backend: &self.backend,
            gpu_layers: self.gpu_layers,
            prompt_tps: self.prompt_tps,
            generate_tps: self.generate_tps,
            measured_at: &self.measured_at,
            fast_enough_to_work: self.fast_enough_to_work(),
        }
        .serialize(s)
    }
}

/// The speed probe: the first time a text model loads on this machine, a fixed 500-token prompt
/// and 300-token answer measure real tokens per second, and the number is kept in
/// `runtime/probe.json` with the model pin and the driver version. It is measured again when
/// either changes. Quality belongs to the model and fit is arithmetic; speed is the one thing only
/// the machine can tell, and it decides whether a model is fast enough to work for Nook Code.
pub struct SpeedProbe {
    file: PathBuf,
    /// In insertion order; a new measurement of a model replaces its entry in place.
    results: Mutex<Vec<ProbeResult>>,
}

impl SpeedProbe {
    /// The probe results kept in `<runtime_dir>\probe.json`.
    pub fn new(runtime_dir: &Path) -> SpeedProbe {
        let probe = SpeedProbe {
            file: runtime_dir.join("probe.json"),
            results: Mutex::new(Vec::new()),
        };
        probe.load();
        probe
    }

    /// The stored result for a model when the pin and the driver still match; otherwise None.
    pub fn current(
        &self,
        model_id: &str,
        sha256: Option<&str>,
        driver: Option<&str>,
    ) -> Option<ProbeResult> {
        let results = self.results.lock();
        let r = results.iter().find(|r| r.model_id == model_id)?;
        if r.sha256.as_deref() != sha256 || r.driver.as_deref() != driver {
            return None;
        }
        Some(r.clone())
    }

    pub fn all(&self) -> Vec<ProbeResult> {
        self.results.lock().clone()
    }

    /// The fixed prompt: about 500 tokens of plain prose the model has to read before answering.
    pub fn prompt() -> String {
        format!(
            "Summarise, in your own words and in detail, the following description of a build system. {}",
            PROMPT_SENTENCE.repeat(18)
        )
    }

    /// Runs the measurement against a ready engine and records it. The engine's own timings are
    /// used, so the numbers do not include the HTTP round trip. Takes about thirty seconds on an
    /// 8 GB card; the caller decides when the engine can spare that.
    pub async fn measure(
        &self,
        client: &InferenceClient,
        model_id: &str,
        sha256: Option<&str>,
        driver: Option<&str>,
        backend: &str,
        gpu_layers: i32,
    ) -> Result<ProbeResult> {
        let body = json!({
            "prompt": SpeedProbe::prompt(),
            "n_predict": ANSWER_TOKENS,
            "temperature": 0.2,
            "cache_prompt": false,
        });
        let r = client.completion(body).await?;
        let t = r.get("timings");
        let prompt = double(t.and_then(|t| t.get("prompt_per_second"))).unwrap_or(0.0);
        let generate = double(t.and_then(|t| t.get("predicted_per_second"))).unwrap_or(0.0);
        if generate <= 0.0 {
            bail!("The engine returned no timings for the probe");
        }
        let result = ProbeResult {
            model_id: model_id.to_string(),
            sha256: sha256.map(str::to_string),
            driver: driver.map(str::to_string),
            backend: Some(backend.to_string()),
            gpu_layers,
            prompt_tps: round1(prompt),
            generate_tps: round1(generate),
            measured_at: Utc::now(),
        };
        {
            let mut results = self.results.lock();
            match results.iter_mut().find(|r| r.model_id == model_id) {
                Some(existing) => *existing = result.clone(),
                None => results.push(result.clone()),
            }
            self.save(&results);
        }
        tracing::info!(
            "Speed probe for {model_id}: prompt {} tok/s, generate {} tok/s (driver {}, {gpu_layers} GPU layers)",
            result.prompt_tps,
            result.generate_tps,
            driver.unwrap_or("null")
        );
        Ok(result)
    }

    pub fn forget(&self, model_id: &str) {
        let mut results = self.results.lock();
        let before = results.len();
        results.retain(|r| r.model_id != model_id);
        if results.len() != before {
            self.save(&results);
        }
    }

    fn load(&self) {
        if !self.file.is_file() {
            return;
        }
        if let Err(e) = self.read() {
            tracing::warn!("Ignoring unreadable {}: {e:#}", self.file.display());
        }
    }

    /// Reads the file; an entry that cannot be read stops the reading, keeping the ones before it
    /// (the original's single try around the loop).
    fn read(&self) -> Result<()> {
        let bytes = std::fs::read(&self.file)?;
        let n: Value = serde_json::from_slice(&bytes)?;
        let mut results = self.results.lock();
        for p in n
            .get("probes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let measured = text(p.get("measured_at")).unwrap_or_default();
            let measured_at = DateTime::parse_from_rfc3339(&measured)
                .with_context(|| format!("bad measured_at '{measured}'"))?
                .with_timezone(&Utc);
            let Some(model_id) = text(p.get("model")) else {
                continue;
            };
            let r = ProbeResult {
                model_id,
                sha256: text(p.get("sha256")),
                driver: text(p.get("driver")),
                backend: text(p.get("backend")),
                gpu_layers: long(p.get("gpu_layers")).unwrap_or(-1) as i32,
                prompt_tps: double(p.get("prompt_tps")).unwrap_or(0.0),
                generate_tps: double(p.get("generate_tps")).unwrap_or(0.0),
                measured_at,
            };
            match results.iter_mut().find(|x| x.model_id == r.model_id) {
                Some(existing) => *existing = r,
                None => results.push(r),
            }
        }
        Ok(())
    }

    fn save(&self, results: &[ProbeResult]) {
        let probes: Vec<Value> = results
            .iter()
            .map(|r| {
                json!({
                    "model": r.model_id,
                    "sha256": r.sha256,
                    "driver": r.driver,
                    "backend": r.backend,
                    "gpu_layers": r.gpu_layers,
                    "prompt_tps": r.prompt_tps,
                    "generate_tps": r.generate_tps,
                    "measured_at": r.measured_at.to_rfc3339_opts(SecondsFormat::AutoSi, true),
                })
            })
            .collect();
        let root = json!({
            "version": 1,
            "prompt_tokens": 500,
            "answer_tokens": ANSWER_TOKENS,
            "probes": probes,
        });
        let write = || -> Result<()> {
            if let Some(parent) = self.file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = self.file.with_file_name("probe.json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&root)?)?;
            std::fs::rename(&tmp, &self.file)?;
            Ok(())
        };
        if let Err(e) = write() {
            tracing::warn!("Could not write {}: {e:#}", self.file.display());
        }
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::downloader::tests::serve;
    use axum::routing::post;
    use axum::{Json, Router};

    /// An engine that answers the probe with fixed timings, like llama-server's /completion does.
    async fn fake_engine(prompt_tps: f64, generate_tps: f64) -> InferenceClient {
        let router = Router::new().route(
            "/completion",
            post(move |Json(body): Json<Value>| async move {
                assert_eq!(body["n_predict"], ANSWER_TOKENS);
                assert_eq!(
                    body["cache_prompt"], false,
                    "the prompt must be processed, not served from the cache"
                );
                Json(json!({"content": "…", "timings": {"prompt_n": 512, "prompt_per_second": prompt_tps,
                    "predicted_n": 300, "predicted_per_second": generate_tps}}))
            }),
        );
        InferenceClient::new(serve(router).await, None)
    }

    #[tokio::test]
    async fn measures_once_per_pin_and_driver_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let probe = SpeedProbe::new(dir.path());
        assert!(probe
            .current("qwen3-8b", Some("abc"), Some("596.36"))
            .is_none());

        let r = probe
            .measure(
                &fake_engine(1766.3, 23.54).await,
                "qwen3-8b",
                Some("abc"),
                Some("596.36"),
                "cuda",
                27,
            )
            .await
            .unwrap();
        assert_eq!(r.generate_tps, 23.5);
        assert_eq!(r.prompt_tps, 1766.3);
        assert!(r.fast_enough_to_work());
        assert!(dir.path().join("probe.json").is_file());

        let again = SpeedProbe::new(dir.path());
        assert_eq!(
            again
                .current("qwen3-8b", Some("abc"), Some("596.36"))
                .unwrap()
                .generate_tps,
            23.5,
            "read back from disk"
        );
        assert!(
            again
                .current("qwen3-8b", Some("abc"), Some("600.00"))
                .is_none(),
            "a new driver measures again"
        );
        assert!(
            again
                .current("qwen3-8b", Some("def"), Some("596.36"))
                .is_none(),
            "a new model file measures again"
        );
        assert_eq!(again.all().len(), 1);

        let json = serde_json::to_value(&again.all()[0]).unwrap();
        assert_eq!(json["modelId"], "qwen3-8b");
        assert_eq!(json["generateTps"], 23.5);
        assert_eq!(json["fastEnoughToWork"], true);
        let file: Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("probe.json")).unwrap()).unwrap();
        assert_eq!(file["probes"][0]["gpu_layers"], 27);
        assert_eq!(file["answer_tokens"], 300);

        again.forget("qwen3-8b");
        assert!(SpeedProbe::new(dir.path()).all().is_empty());
    }

    #[tokio::test]
    async fn under_eight_tokens_per_second_is_too_slow_to_work() {
        let dir = tempfile::tempdir().unwrap();
        let probe = SpeedProbe::new(dir.path());
        let slow = probe
            .measure(
                &fake_engine(200.0, 6.9).await,
                "big-moe",
                Some("sha"),
                Some("596.36"),
                "cuda",
                99,
            )
            .await
            .unwrap();
        assert!(!slow.fast_enough_to_work());
        assert!(
            SpeedProbe::prompt().len() > 1500,
            "about five hundred tokens of prose"
        );

        let none = probe
            .measure(&fake_engine(200.0, 0.0).await, "x", None, None, "cpu", 0)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(none, "The engine returned no timings for the probe");
    }

    #[test]
    fn an_unreadable_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("probe.json"), "not json").unwrap();
        assert!(SpeedProbe::new(dir.path()).all().is_empty());
        std::fs::write(
            dir.path().join("probe.json"),
            r#"{"probes":[{"model":"a","driver":null,"generate_tps":9.5,"measured_at":"2026-09-20T10:00:00Z"},
                {"model":"b","measured_at":"yesterday"},{"model":"c","measured_at":"2026-09-20T10:00:00Z"}]}"#,
        )
        .unwrap();
        let probe = SpeedProbe::new(dir.path());
        let all = probe.all();
        assert_eq!(all.len(), 1, "reading stops at the bad entry");
        assert_eq!(all[0].gpu_layers, -1);
        assert!(
            probe.current("a", None, None).is_some(),
            "null pins match null"
        );
    }
}

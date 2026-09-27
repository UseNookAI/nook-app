//! The finder: a small fixed model that picks the Nooklet for a request typed in a sentence.
//!
//! It is multilingual-e5-small (118 M parameters, 132 MB at 8 bits, MIT), an embedding model:
//! the request and each Nooklet's example requests become vectors, and the Nooklet whose example
//! lies nearest wins. It runs on the processor alone (llama.cpp's CPU build, 18 MB), so it works
//! on any computer, loads in about a second and answers in tens of milliseconds; it is not the
//! person's to change. Until it is downloaded, and should it fail, the words of the request
//! choose instead.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::catalog::{self, Preset, NOOKLETS};
use crate::busy::BusyWork;
use crate::events::{self, topic};
use crate::flow::Install;
use crate::home::Home;
use crate::runtime::downloader::{Downloader, Outcome};
use crate::runtime::{Backend, EngineComponent, RuntimeManager, StagedProgress};

/// The model, pinned to the revision whose file the checksum is of.
pub const MODEL_URL: &str = "https://huggingface.co/TwinSunsLLC/multilingual-e5-small-gguf/resolve/b6cac9615d4ecce28d7f22539b7322d695fc2886/multilingual-e5-small-q8_0.gguf";
pub const MODEL_FILE: &str = "multilingual-e5-small-q8_0.gguf";
pub const MODEL_SHA256: &str = "e011debc1208e31bf7b6aebee2d9fc8bd2ca11694a77ed66ac9d0c9d0a877c93";
pub const MODEL_BYTES: u64 = 132_439_008;
/// What the download line calls it.
pub const WHAT: &str = "the Nooklet finder";

/// Nearer than this, a Nooklet does what was asked; between this and `MAYBE` it may.
/// (Measured on the model: requests for a Nooklet land at 0.85 to 0.99, others at 0.83 to 0.86.)
pub const SURE: f32 = 0.88;
pub const MAYBE: f32 = 0.85;
/// The finder stops after this long unused, and starts again (in a second) when asked.
const IDLE: Duration = Duration::from_secs(10 * 60);

/// A Nooklet found for a request, and what the request sets for it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub id: String,
    pub title: String,
    pub blurb: String,
    pub score: f32,
    /// It may do what was asked.
    pub fits: bool,
    pub preset: Option<Preset>,
}

/// Every Nooklet, the best first; `sure` when the first does what was asked, `matched` when it
/// may; `by`: "model", or "words" while the finder is not in.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    pub hits: Vec<Hit>,
    pub sure: bool,
    pub matched: bool,
    pub by: String,
}

/// Whether the finder is in, the size of what it still needs, and its download while it runs.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinderSetup {
    pub installed: bool,
    pub bytes: u64,
    pub install: Option<Install>,
}

/// What turns texts into vectors: the llama.cpp server here, a stand-in in tests.
#[async_trait::async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    /// Lets go of what it runs (the server), when Nook closes.
    async fn stop(&self) {}
}

/// The examples' vectors, each with the index of its Nooklet.
type Examples = Arc<Vec<(usize, Vec<f32>)>>;

pub struct Finder {
    runtime: Arc<RuntimeManager>,
    home: Home,
    downloader: Arc<Downloader>,
    install: Mutex<Option<Install>>,
    install_cancel: Mutex<Option<CancellationToken>>,
    stopping: CancellationToken,
    /// The examples' vectors, by Nooklet, made once.
    examples: Mutex<Option<Examples>>,
    /// A stand-in for the model (tests); else the model's server, made when first asked.
    given: Option<Arc<dyn Embedder>>,
    embedder: Mutex<Option<Arc<dyn Embedder>>>,
    me: Weak<Finder>,
}

impl Finder {
    pub fn new(runtime: Arc<RuntimeManager>, home: Home) -> Arc<Finder> {
        Self::build(runtime, home, None)
    }

    pub fn with_embedder(
        runtime: Arc<RuntimeManager>,
        home: Home,
        embedder: Arc<dyn Embedder>,
    ) -> Arc<Finder> {
        Self::build(runtime, home, Some(embedder))
    }

    fn build(
        runtime: Arc<RuntimeManager>,
        home: Home,
        embedder: Option<Arc<dyn Embedder>>,
    ) -> Arc<Finder> {
        Arc::new_cyclic(|me| Finder {
            runtime,
            home,
            downloader: Arc::new(Downloader::new()),
            install: Mutex::new(None),
            install_cancel: Mutex::new(None),
            stopping: CancellationToken::new(),
            examples: Mutex::new(None),
            given: embedder,
            embedder: Mutex::new(None),
            me: me.clone(),
        })
    }

    fn model_path(&self) -> PathBuf {
        self.home.runtime_dir().join("finder").join(MODEL_FILE)
    }

    fn engine_in(&self) -> bool {
        self.runtime
            .packages()
            .is_installed(EngineComponent::Llama, Backend::Cpu)
    }

    fn model_in(&self) -> bool {
        std::fs::metadata(self.model_path()).is_ok_and(|m| m.len() == MODEL_BYTES)
    }

    pub fn installed(&self) -> bool {
        self.given.is_some() || (self.engine_in() && self.model_in())
    }

    /// What is still to download: the processor's llama.cpp, the model.
    fn missing_bytes(&self) -> u64 {
        let engine = if self.engine_in() {
            0
        } else {
            self.runtime
                .packages()
                .package_for(EngineComponent::Llama, Backend::Cpu)
                .map_or(0, |p| p.total_bytes())
        };
        engine + if self.model_in() { 0 } else { MODEL_BYTES }
    }

    pub fn setup(&self) -> FinderSetup {
        FinderSetup {
            installed: self.installed(),
            bytes: self.missing_bytes(),
            install: self.install.lock().clone(),
        }
    }

    fn install_changed(&self) {
        events::emit(
            topic::NOOKLETS,
            json!({ "install": self.install.lock().clone() }),
        );
    }

    /// Downloads what the finder still needs, in the background.
    pub fn start_install(&self) -> Result<(), String> {
        if self.stopping.is_cancelled() {
            return Err("Nook is closing.".into());
        }
        if self.installed()
            || self
                .install
                .lock()
                .as_ref()
                .is_some_and(|i| i.error.is_none())
        {
            return Ok(());
        }
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "Downloads need the app's async runtime.".to_string())?;
        let total = self.missing_bytes();
        let cancel = self.stopping.child_token();
        *self.install_cancel.lock() = Some(cancel.clone());
        *self.install.lock() = Some(Install {
            what: WHAT.into(),
            done: 0,
            total,
            error: None,
        });
        self.install_changed();
        let me = self.me.clone();
        handle.spawn(async move {
            let Some(me) = me.upgrade() else { return };
            let outcome = me.download(&cancel).await;
            let done = me.install.lock().as_ref().map_or(0, |i| i.done);
            let failed = |error: String| Install {
                what: WHAT.into(),
                done,
                total,
                error: Some(error),
            };
            *me.install.lock() = match outcome {
                Ok(true) => None,
                Ok(false) => Some(failed("The download was stopped.".into())),
                Err(e) => {
                    tracing::warn!("The Nooklet finder did not install: {e:#}");
                    Some(failed(format!("The download failed: {e:#}")))
                }
            };
            me.install_cancel.lock().take();
            me.install_changed();
        });
        Ok(())
    }

    async fn download(&self, cancel: &CancellationToken) -> Result<bool> {
        let progress_from = |base: u64| -> StagedProgress {
            let me = self.me.clone();
            Arc::new(move |_stage: &str, done, _of| {
                let Some(me) = me.upgrade() else { return };
                if let Some(i) = me.install.lock().as_mut().filter(|i| i.error.is_none()) {
                    i.done = (base + done).min(i.total);
                }
                me.install_changed();
            })
        };
        let mut base = 0;
        if !self.engine_in() {
            let packages = self.runtime.packages();
            let size = packages
                .package_for(EngineComponent::Llama, Backend::Cpu)
                .map_or(0, |p| p.total_bytes());
            if !packages
                .ensure_installed(
                    EngineComponent::Llama,
                    Backend::Cpu,
                    Some(progress_from(0)),
                    cancel,
                )
                .await?
            {
                return Ok(false);
            }
            base = size;
        }
        if !self.model_in() {
            let path = self.model_path();
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let staged = progress_from(base);
            let per_file: crate::runtime::Progress =
                Arc::new(move |done, of| staged("download", done, of));
            let outcome = self
                .downloader
                .download(
                    MODEL_URL,
                    &path,
                    Some(MODEL_SHA256),
                    MODEL_BYTES,
                    Some(&per_file),
                    cancel,
                )
                .await?;
            if outcome == Outcome::Cancelled {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn cancel_install(&self) {
        if let Some(c) = self.install_cancel.lock().as_ref() {
            c.cancel();
        }
    }

    pub fn clear_install_error(&self) {
        let cleared = {
            let mut i = self.install.lock();
            if i.as_ref().is_some_and(|i| i.error.is_some()) {
                *i = None;
                true
            } else {
                false
            }
        };
        if cleared {
            self.install_changed();
        }
    }

    // ------------------------------------------------------------------ finding

    /// The Nooklets for `request`, the best first.
    pub async fn find(&self, request: &str) -> Found {
        let words = catalog::by_words(request);
        let by_model = if self.installed() && !request.trim().is_empty() {
            match self.nearness(request).await {
                Ok(n) => Some(n),
                Err(e) => {
                    tracing::warn!("The Nooklet finder could not answer: {e:#}");
                    None
                }
            }
        } else {
            None
        };
        let (scores, by): (Vec<f32>, &str) = match by_model {
            // The model's nearness, nudged by the words that point to a Nooklet.
            Some(near) => (
                near.iter()
                    .zip(&words)
                    .map(|(n, w)| (n + w * 0.03).min(1.0))
                    .collect(),
                "model",
            ),
            None => (words.clone(), "words"),
        };
        let mut hits: Vec<Hit> = NOOKLETS
            .iter()
            .zip(&scores)
            .map(|(n, &score)| Hit {
                id: n.id.into(),
                title: n.title.into(),
                blurb: n.blurb.into(),
                score,
                fits: false,
                preset: catalog::preset(n.id, request),
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        let best = hits.first().map_or(0.0, |h| h.score);
        let (sure, maybe) = if by == "model" {
            (SURE, MAYBE)
        } else {
            (0.6, 0.3)
        };
        for h in &mut hits {
            h.fits = h.score >= maybe;
        }
        let (sure, matched) = (best >= sure, best >= maybe);
        Found {
            hits,
            sure,
            matched,
            by: by.into(),
        }
    }

    /// How near `request` lies to each Nooklet's nearest example (cosine, 0 to 1).
    async fn nearness(&self, request: &str) -> Result<Vec<f32>> {
        let cached = self.examples.lock().clone();
        let examples = match cached {
            Some(e) => e,
            None => {
                let texts: Vec<String> = NOOKLETS
                    .iter()
                    .flat_map(|n| n.examples.iter().map(|e| query(e)))
                    .collect();
                let owners: Vec<usize> = NOOKLETS
                    .iter()
                    .enumerate()
                    .flat_map(|(i, n)| std::iter::repeat_n(i, n.examples.len()))
                    .collect();
                let vectors = self.embed(&texts).await?;
                let made = Arc::new(owners.into_iter().zip(vectors).collect::<Vec<_>>());
                *self.examples.lock() = Some(made.clone());
                made
            }
        };
        let asked = self
            .embed(&[query(request)])
            .await?
            .pop()
            .ok_or_else(|| anyhow!("The finder gave no answer"))?;
        let mut best = vec![0.0f32; NOOKLETS.len()];
        for (owner, v) in examples.iter() {
            best[*owner] = best[*owner].max(cosine(&asked, v));
        }
        Ok(best)
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let embedder = match &self.given {
            Some(e) => e.clone(),
            None => self
                .embedder
                .lock()
                .get_or_insert_with(|| {
                    LlamaEmbedder::shared(
                        self.runtime.packages().server_executable(Backend::Cpu),
                        self.model_path(),
                    )
                })
                .clone(),
        };
        embedder.embed(texts).await
    }

    pub async fn shutdown(&self) {
        self.stopping.cancel();
        let running = self.embedder.lock().take();
        if let Some(e) = running {
            e.stop().await;
        }
    }
}

/// The model served by llama.cpp's build for the processor, on a free port of this machine
/// only: started when first asked, stopped after `IDLE` unused.
pub struct LlamaEmbedder {
    exe: PathBuf,
    model: PathBuf,
    server: tokio::sync::Mutex<Option<Server>>,
    used: AtomicU64,
    me: Weak<LlamaEmbedder>,
}

struct Server {
    child: tokio::process::Child,
    port: u16,
    client: reqwest::Client,
}

impl LlamaEmbedder {
    pub fn shared(exe: PathBuf, model: PathBuf) -> Arc<dyn Embedder> {
        Arc::new_cyclic(|me| LlamaEmbedder {
            exe,
            model,
            server: tokio::sync::Mutex::new(None),
            used: AtomicU64::new(0),
            me: me.clone(),
        })
    }

    async fn start_server(&self) -> Result<Server> {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0")?;
            l.local_addr()?.port()
        };
        let threads = std::thread::available_parallelism()
            .map(|n| (n.get() / 2).clamp(1, 4))
            .unwrap_or(2);
        let mut cmd = crate::process::command(&self.exe);
        cmd.arg("-m")
            .arg(&self.model)
            .args([
                "--embedding",
                "--pooling",
                "mean",
                "-c",
                "512",
                "-ub",
                "512",
                "-np",
                "1",
            ])
            .arg("-t")
            .arg(threads.to_string())
            .args(["--host", "127.0.0.1", "--port"])
            .arg(port.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = crate::process::spawn_managed(&mut cmd)
            .with_context(|| format!("Could not start the finder ({})", self.exe.display()))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        for _ in 0..300 {
            if let Some(status) = child.try_wait()? {
                bail!("The finder stopped as it started ({status})");
            }
            if client
                .get(format!("http://127.0.0.1:{port}/health"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return Ok(Server {
                    child,
                    port,
                    client,
                });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = child.kill().await;
        bail!("The finder did not start within half a minute")
    }

    /// Stops the server after `IDLE` unused.
    fn stop_when_idle(&self) {
        let turn = self.used.fetch_add(1, Ordering::SeqCst) + 1;
        let me = self.me.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            tokio::time::sleep(IDLE).await;
            let Some(me) = me.upgrade() else { return };
            if me.used.load(Ordering::SeqCst) == turn {
                me.stop().await;
            }
        });
    }
}

#[async_trait::async_trait]
impl Embedder for LlamaEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut server = self.server.lock().await;
        let alive = match server.as_mut() {
            Some(s) => s.child.try_wait().ok().flatten().is_none(),
            None => false,
        };
        if !alive {
            *server = Some(self.start_server().await?);
        }
        let s = server.as_ref().expect("started");
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(32) {
            let reply: Value = s
                .client
                .post(format!("http://127.0.0.1:{}/v1/embeddings", s.port))
                .json(&json!({ "input": chunk }))
                .send()
                .await
                .context("The finder did not answer")?
                .error_for_status()?
                .json()
                .await?;
            let mut data: Vec<(u64, Vec<f32>)> = reply["data"]
                .as_array()
                .ok_or_else(|| anyhow!("The finder's answer has no vectors"))?
                .iter()
                .map(|d| {
                    let v = d["embedding"]
                        .as_array()
                        .map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect())
                        .unwrap_or_default();
                    (d["index"].as_u64().unwrap_or(0), v)
                })
                .collect();
            data.sort_by_key(|(i, _)| *i);
            out.extend(data.into_iter().map(|(_, v)| v));
        }
        drop(server);
        self.stop_when_idle();
        Ok(out)
    }

    async fn stop(&self) {
        if let Some(mut s) = self.server.lock().await.take() {
            let _ = s.child.kill().await;
        }
    }
}

/// e5 reads a request with its prefix.
fn query(text: &str) -> String {
    format!("query: {}", text.trim())
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

impl BusyWork for Finder {
    fn busy_with(&self) -> Option<String> {
        self.install
            .lock()
            .as_ref()
            .is_some_and(|i| i.error.is_none())
            .then(|| "the Nooklet finder is downloading".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::manager::testing::{rig, RigSpec};

    /// Vectors by keyword: each text points along the axis of the first Nooklet word it has; a
    /// flight, nothing a Nooklet does, along one of its own.
    struct Axes;

    #[async_trait::async_trait]
    impl Embedder for Axes {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    let t = t.to_lowercase();
                    let axis = if t.contains("translat") || t.contains("dub") || t.contains("çevir")
                    {
                        0
                    } else if t.contains("edit") || t.contains("typo") || t.contains("fix") {
                        1
                    } else if t.contains("convert") || t.contains(" to ") || t.contains("jpg") {
                        2
                    } else if t.contains("flight") {
                        4
                    } else {
                        3
                    };
                    let mut v = vec![0.05; 5];
                    v[axis] = 1.0;
                    v
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn the_nearest_nooklet_comes_first_with_what_the_request_sets() {
        let rig = rig(RigSpec::default());
        let finder = Finder::with_embedder(rig.manager.clone(), rig.home.clone(), Arc::new(Axes));
        assert!(finder.installed());
        let found = finder.find("translate what I say into German").await;
        assert_eq!(found.by, "model");
        assert!(found.sure && found.matched);
        assert_eq!(found.hits[0].id, "translate");
        assert_eq!(found.hits[0].preset.as_ref().unwrap().label, "into German");
        let found = finder.find("convert this pdf to word").await;
        assert_eq!(found.hits[0].id, "convert");
        assert_eq!(found.hits[0].preset.as_ref().unwrap().value, "docx");
        let found = finder.find("book a flight").await;
        assert!(!found.matched, "{:?}", found.hits[0]);
        assert_eq!(found.hits.len(), NOOKLETS.len());
    }

    /// With the real model: `NOOK_TEST_LLAMA_CPU` names llama.cpp's `llama-server.exe` built for
    /// the processor, `NOOK_TEST_FINDER` the model file:
    /// `cargo test -p nook-core finds_with_the_real_model -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs llama.cpp and the model"]
    async fn finds_with_the_real_model() {
        let env = |n: &str| PathBuf::from(std::env::var(n).unwrap_or_else(|_| panic!("set {n}")));
        let rig = rig(RigSpec::default());
        let embedder = LlamaEmbedder::shared(env("NOOK_TEST_LLAMA_CPU"), env("NOOK_TEST_FINDER"));
        let finder = Finder::with_embedder(rig.manager.clone(), rig.home.clone(), embedder.clone());
        let cases: &[(&str, Option<&str>)] = &[
            ("turn my word file into a pdf", Some("convert")),
            ("convert excel to csv", Some("convert")),
            ("jpg to png", Some("convert")),
            ("make a pdf out of these photos", Some("convert")),
            ("I need this presentation as PDF", Some("convert")),
            ("bu belgeyi pdf yap", Some("convert")),
            ("convertir ce fichier word en pdf", Some("convert")),
            ("fix a typo in my contract pdf", Some("pdf")),
            ("change the date on this scanned form", Some("pdf")),
            ("replace the name in the invoice", Some("pdf")),
            ("pdf'deki ismi değiştir", Some("pdf")),
            ("translate what I say into german", Some("translate")),
            ("dub this video in spanish", Some("translate")),
            ("I want to hear this podcast in French", Some("translate")),
            ("übersetze meine Rede ins Englische", Some("translate")),
            ("transcribe this meeting recording", Some("transcribe")),
            ("turn my voice memo into text", Some("transcribe")),
            (
                "I need notes from yesterday's call recording",
                Some("transcribe"),
            ),
            (
                "write down everything said in this interview",
                Some("transcribe"),
            ),
            ("ses kaydını yazıya dök", Some("transcribe")),
            ("summarize this PDF for me", Some("summarize")),
            ("what are the main points of this report", Some("summarize")),
            (
                "is there anything bad hidden in this contract",
                Some("summarize"),
            ),
            ("bu raporu özetle", Some("summarize")),
            ("fasse diesen Artikel zusammen", Some("summarize")),
            ("read this article aloud", Some("read-aloud")),
            ("turn my novel into an audiobook", Some("read-aloud")),
            (
                "I'd rather listen to this document than read it",
                Some("read-aloud"),
            ),
            ("bu metni sesli oku", Some("read-aloud")),
            ("lies mir diesen Brief vor", Some("read-aloud")),
            ("book a flight to Paris", None),
            ("write me a poem", None),
            ("tell me a story", None),
            ("write an email to my boss", None),
            ("what's the weather", None),
            ("play some music", None),
            ("generate an image of a cat", None),
        ];
        let mut wrong = Vec::new();
        for (q, want) in cases {
            let started = std::time::Instant::now();
            let found = finder.find(q).await;
            let best = &found.hits[0];
            println!(
                "{:>4} ms {:.3} {:10} (then {:.3} {:10}) sure={} matched={} {:?} | {q}",
                started.elapsed().as_millis(),
                best.score,
                best.id,
                found.hits[1].score,
                found.hits[1].id,
                found.sure,
                found.matched,
                best.preset.as_ref().map(|p| &p.label)
            );
            let right = match want {
                Some(id) => best.id == *id && found.matched,
                None => !found.sure,
            };
            if !right {
                wrong.push(*q);
            }
        }
        embedder.stop().await;
        assert!(wrong.is_empty(), "wrong: {wrong:?}");
    }

    #[tokio::test]
    async fn without_the_model_the_words_choose() {
        let rig = rig(RigSpec::default());
        let finder = Finder::new(rig.manager.clone(), rig.home.clone());
        assert!(!finder.installed());
        assert_eq!(
            finder.setup().bytes,
            MODEL_BYTES
                + rig
                    .manager
                    .packages()
                    .package_for(EngineComponent::Llama, Backend::Cpu)
                    .map_or(0, |p| p.total_bytes())
        );
        let found = finder.find("fix a typo in my pdf").await;
        assert_eq!(found.by, "words");
        assert_eq!(found.hits[0].id, "pdf");
        assert!(found.matched);
    }
}

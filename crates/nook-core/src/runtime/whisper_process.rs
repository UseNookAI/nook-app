//! Ports `runtime/WhisperProcess.java`: one `whisper-server` process serving one speech model.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::process::Child;

use super::engine_process::{append_log, free_port, log_stdio, log_tail, terminate};
use super::model_registry::now_iso;

/// How long a transcription may take.
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// One `whisper-server` process serving one speech model on a loopback port. The server takes
/// 16 kHz mono WAV uploads on `/inference` and answers with JSON.
pub struct WhisperProcess {
    model_id: String,
    model_file: PathBuf,
    exe: PathBuf,
    log_file: PathBuf,
    port: u16,
    gpu: bool,
    http: reqwest::Client,
    in_flight: AtomicI64,
    last_used: Mutex<DateTime<Utc>>,
    child: Mutex<Option<Child>>,
    /// One start at a time (the Java method was `synchronized`).
    starting: tokio::sync::Mutex<()>,
}

impl WhisperProcess {
    /// Whether it runs on the graphics card.
    pub fn on_gpu(&self) -> bool {
        self.gpu
    }

    /// A speech engine for `model_file` run by `exe`, logging to `<log_dir>\<model_id>.log`.
    /// `gpu`: false passes `-ng` (no GPU). Picks the port; starts nothing.
    pub fn new(
        model_id: &str,
        model_file: &Path,
        exe: &Path,
        log_dir: &Path,
        gpu: bool,
    ) -> Result<WhisperProcess> {
        let port = free_port()?;
        std::fs::create_dir_all(log_dir)
            .with_context(|| format!("Could not create {}", log_dir.display()))?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("speech client could not be configured ({e}); using the defaults");
                reqwest::Client::new()
            });
        Ok(WhisperProcess {
            model_id: model_id.to_string(),
            model_file: model_file.to_path_buf(),
            exe: exe.to_path_buf(),
            log_file: log_dir.join(format!("{model_id}.log")),
            port,
            gpu,
            http,
            in_flight: AtomicI64::new(0),
            last_used: Mutex::new(Utc::now()),
            child: Mutex::new(None),
            starting: tokio::sync::Mutex::new(()),
        })
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn last_used(&self) -> DateTime<Utc> {
        *self.last_used.lock()
    }
    pub fn in_flight(&self) -> u32 {
        self.in_flight.load(Ordering::SeqCst).max(0) as u32
    }
    pub fn is_alive(&self) -> bool {
        match self.child.lock().as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    fn touch(&self) {
        *self.last_used.lock() = Utc::now();
    }

    /// The command line (the executable first).
    pub fn command(&self) -> Vec<String> {
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        let mut cmd: Vec<String> = vec![
            self.exe.display().to_string(),
            "-m".into(),
            self.model_file.display().to_string(),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            self.port.to_string(),
            "-t".into(),
            (threads / 2).max(2).to_string(),
        ];
        if !self.gpu {
            cmd.push("-ng".into());
        }
        cmd
    }

    /// Launches the server and waits until it answers or fails.
    pub async fn start(&self, timeout: Duration) -> Result<()> {
        let _one = self.starting.lock().await;
        if self.is_alive() {
            return Ok(());
        }
        if !self.exe.exists() {
            bail!("Speech engine binary missing: {}", self.exe.display());
        }
        let cmd = self.command();
        append_log(
            &self.log_file,
            &format!("\n=== {} starting {}\n", now_iso(), cmd.join(" ")),
        );
        let (out, err) = log_stdio(&self.log_file)?;
        let mut c = crate::process::command(&cmd[0]);
        c.args(&cmd[1..]).stdout(out).stderr(err);
        if let Some(dir) = self.exe.parent() {
            c.current_dir(dir);
        }
        let child = crate::process::spawn_managed(&mut c).map_err(|e| {
            anyhow!(
                "Could not start the speech engine {}: {e}",
                self.exe.display()
            )
        })?;
        *self.child.lock() = Some(child);
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let exited = match self.child.lock().as_mut() {
                None => Some(-1),
                Some(c) => match c.try_wait() {
                    Ok(None) => None,
                    Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
                    Err(_) => Some(-1),
                },
            };
            if let Some(code) = exited {
                bail!(
                    "Speech engine exited with code {code}. {}",
                    self.log_tail(20)
                );
            }
            if self.reachable().await {
                self.touch();
                tracing::info!(
                    "Speech engine for {} ready on port {}",
                    self.model_id,
                    self.port
                );
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        self.stop().await;
        bail!(
            "Speech engine did not answer within {} s. {}",
            timeout.as_secs(),
            self.log_tail(20)
        )
    }

    /// Any HTTP answer on `/` counts: the server is up.
    async fn reachable(&self) -> bool {
        self.http
            .get(format!("http://127.0.0.1:{}/", self.port))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_ok()
    }

    /// Transcribes a WAV file (16 kHz mono) and returns the text.
    pub async fn transcribe(&self, wav: &Path, language: Option<&str>) -> Result<String> {
        let reply = self.inference(wav, language, "json").await?;
        Ok(reply
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string())
    }

    /// Runs the server's `/inference` with the given response format (`json` or `verbose_json`,
    /// the latter with timed segments) and returns the parsed reply. `language`: an ISO code, or
    /// None for detection (`auto`).
    pub async fn inference(
        &self,
        wav: &Path,
        language: Option<&str>,
        response_format: &str,
    ) -> Result<Value> {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        self.touch();
        let result = self.post_inference(wav, language, response_format).await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        self.touch();
        result
    }

    async fn post_inference(
        &self,
        wav: &Path,
        language: Option<&str>,
        response_format: &str,
    ) -> Result<Value> {
        let boundary = format!("----Nook{}", uuid::Uuid::new_v4());
        let file_bytes = tokio::fs::read(wav)
            .await
            .with_context(|| format!("Could not read {}", wav.display()))?;
        let name = wav
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let body = multipart_body(
            &boundary,
            &name,
            &file_bytes,
            &[
                ("response_format", response_format),
                ("temperature", "0.0"),
                ("language", language.unwrap_or("auto")),
            ],
        );
        let send = self
            .http
            .post(format!("http://127.0.0.1:{}/inference", self.port))
            .header(
                reqwest::header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send();
        let r = match tokio::time::timeout(INFERENCE_TIMEOUT, send).await {
            Ok(r) => r.map_err(|e| anyhow!("Could not reach the speech engine: {e}"))?,
            Err(_) => bail!("request timed out"),
        };
        let status = r.status().as_u16();
        let text = r.text().await?;
        if status / 100 != 2 {
            bail!("Speech engine returned HTTP {status}: {text}");
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// Stops the process and waits for it to go.
    pub async fn stop(&self) {
        let child = self.child.lock().take();
        if let Some(child) = child {
            terminate(child).await;
            tracing::info!("Speech engine for {} stopped", self.model_id);
        }
    }

    pub fn log_tail(&self, lines: usize) -> String {
        log_tail(&self.log_file, lines)
    }
}

/// A multipart/form-data body with one WAV file part and plain fields, as the original built it.
fn multipart_body(
    boundary: &str,
    file_name: &str,
    file: &[u8],
    fields: &[(&str, &str)],
) -> Vec<u8> {
    let nl = "\r\n";
    let mut body = Vec::with_capacity(file.len() + 512);
    body.extend_from_slice(
        format!(
            "--{boundary}{nl}Content-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"{nl}Content-Type: audio/wav{nl}{nl}"
        )
        .as_bytes(),
    );
    body.extend_from_slice(file);
    body.extend_from_slice(nl.as_bytes());
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}{nl}Content-Disposition: form-data; name=\"{name}\"{nl}{nl}{value}{nl}")
                .as_bytes(),
        );
    }
    body.extend_from_slice(format!("--{boundary}--{nl}").as_bytes());
    body
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::body::Bytes;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::Router;
    use serde_json::json;
    use std::sync::Arc;

    /// A fake whisper-server: `/` answers, `/inference` echoes what it was sent. A WAV whose
    /// bytes are `broken` gets HTTP 500.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn fake_whisper_router(seen: Arc<Mutex<Vec<String>>>) -> Router {
        Router::new().route("/", get(|| async { "whisper" })).route(
            "/inference",
            post(move |headers: HeaderMap, body: Bytes| {
                let seen = seen.clone();
                async move {
                    let ct = headers
                        .get("content-type")
                        .and_then(|h| h.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    let text = String::from_utf8_lossy(&body).into_owned();
                    seen.lock().push(format!("{ct}\n{text}"));
                    if text.contains("broken") {
                        return (StatusCode::INTERNAL_SERVER_ERROR, "no audio".to_string())
                            .into_response();
                    }
                    let verbose = text.contains("verbose_json");
                    let reply = if verbose {
                        json!({"text": " hello there ", "language": "en",
                                "segments": [{"start": 0.0, "end": 1.5, "text": "hello there"}]})
                    } else {
                        json!({"text": " hello there \n"})
                    };
                    (StatusCode::OK, reply.to_string()).into_response()
                }
            }),
        )
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) async fn fake_whisper_server(port: u16, seen: Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap();
        let router = fake_whisper_router(seen);
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
    }

    #[test]
    fn the_command_names_the_model_port_and_threads() {
        let dir = tempfile::tempdir().unwrap();
        let w = WhisperProcess::new(
            "whisper-small",
            Path::new("ggml-small.bin"),
            Path::new("whisper-server.exe"),
            dir.path(),
            false,
        )
        .unwrap();
        let cmd = w.command();
        assert_eq!(cmd[0], "whisper-server.exe");
        assert_eq!(cmd[1..3], ["-m".to_string(), "ggml-small.bin".to_string()]);
        let port = cmd.iter().position(|a| a == "--port").unwrap();
        assert_eq!(cmd[port + 1], w.port().to_string());
        let t = cmd.iter().position(|a| a == "-t").unwrap();
        assert!(cmd[t + 1].parse::<usize>().unwrap() >= 2);
        assert_eq!(cmd.last().map(String::as_str), Some("-ng"), "no GPU");
        let gpu = WhisperProcess::new(
            "w",
            Path::new("m.bin"),
            Path::new("w.exe"),
            dir.path(),
            true,
        )
        .unwrap();
        assert!(!gpu.command().contains(&"-ng".to_string()));
    }

    #[test]
    fn the_upload_is_one_file_and_three_fields() {
        let body = multipart_body(
            "B",
            "a.wav",
            b"RIFF",
            &[("response_format", "json"), ("language", "auto")],
        );
        let text = String::from_utf8(body).unwrap();
        assert!(text.starts_with(
            "--B\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF\r\n"
        ));
        assert!(text.contains("name=\"response_format\"\r\n\r\njson\r\n"));
        assert!(text.ends_with("--B--\r\n"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn transcribes_through_the_server_and_stops() {
        use crate::runtime::engine_process::tests::fake_engine;
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_engine(&dir.path().join("whisper"), "whisper-server.cmd", None);
        let w = WhisperProcess::new(
            "whisper-small",
            &dir.path().join("ggml-small.bin"),
            &exe,
            &dir.path().join("logs"),
            true,
        )
        .unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        fake_whisper_server(w.port(), seen.clone()).await;
        w.start(Duration::from_secs(20)).await.unwrap();
        assert!(w.is_alive());

        let wav = dir.path().join("prompt.wav");
        std::fs::write(&wav, b"RIFFfake").unwrap();
        assert_eq!(w.transcribe(&wav, None).await.unwrap(), "hello there");
        let first = seen.lock()[0].clone();
        assert!(
            first.starts_with("multipart/form-data; boundary=----Nook"),
            "{first}"
        );
        assert!(first.contains("filename=\"prompt.wav\""));
        assert!(first.contains("name=\"language\"\r\n\r\nauto"));
        assert!(first.contains("name=\"temperature\"\r\n\r\n0.0"));

        let detailed = w.inference(&wav, Some("de"), "verbose_json").await.unwrap();
        assert_eq!(detailed["language"], "en");
        assert_eq!(detailed["segments"][0]["end"], 1.5);
        assert!(seen.lock()[1].contains("name=\"language\"\r\n\r\nde"));
        assert_eq!(w.in_flight(), 0);

        std::fs::write(&wav, b"broken").unwrap();
        let err = w.transcribe(&wav, None).await.unwrap_err().to_string();
        assert_eq!(err, "Speech engine returned HTTP 500: no audio");
        assert_eq!(w.in_flight(), 0, "released after a failure too");

        w.stop().await;
        assert!(!w.is_alive());
        assert!(
            std::fs::read_to_string(dir.path().join("logs").join("whisper-small.log"))
                .unwrap()
                .contains("starting")
        );
    }

    #[tokio::test]
    async fn a_missing_binary_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("whisper-server.exe");
        let w =
            WhisperProcess::new("w", &dir.path().join("m.bin"), &exe, dir.path(), false).unwrap();
        let err = w
            .start(Duration::from_secs(1))
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            format!("Speech engine binary missing: {}", exe.display())
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_server_that_exits_fails_with_its_log() {
        use crate::runtime::engine_process::tests::fake_engine;
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_engine(dir.path(), "whisper-server.cmd", Some(2));
        let w =
            WhisperProcess::new("w", &dir.path().join("m.bin"), &exe, dir.path(), false).unwrap();
        let err = w
            .start(Duration::from_secs(20))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("Speech engine exited with code 2. "),
            "{err}"
        );
        assert!(err.contains("loading failed"));
    }
}

//! Ports `runtime/Downloader.java`: resumable, verified HTTP downloads.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use reqwest::header::{CONTENT_LENGTH, RANGE, USER_AGENT};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use super::{report, Progress};

/// Sent with every runtime download and Hub request, as the original did.
pub const RUNTIME_USER_AGENT: &str = "Nook-Runtime/0.3";

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(6 * 3600);

/// How a download ended when it did not fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Cancelled,
    AlreadyPresent,
}

/// Resumable, verified HTTP download. Writes to `<target>.part`, resumes with a Range request
/// when a part exists, verifies the sha256 of the complete file, and renames atomically. A
/// cancelled download keeps its part file so it can resume later.
///
/// One `Downloader` (and so one connection pool) is shared by engine installs, catalog models and
/// Hugging Face downloads.
pub struct Downloader {
    client: reqwest::Client,
}

impl Default for Downloader {
    fn default() -> Self {
        Downloader::new()
    }
}

impl Downloader {
    pub fn new() -> Downloader {
        // No transparent decompression: the bytes on disk must be the bytes the Range offsets and
        // the sha256 refer to (Java's client asked for no encoding either).
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .connect_timeout(Duration::from_secs(20))
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("download client could not be configured ({e}); using the defaults");
                reqwest::Client::new()
            });
        Downloader { client }
    }

    /// Downloads `url` to `target`.
    ///
    /// - `expected_sha256`: lowercase hex, or None to skip verification.
    /// - `expected_bytes`: size when known (used for progress before the server answers), or 0.
    /// - `progress`: `(bytes_done, bytes_total)`, at most every 200 ms and once at the end; the
    ///   total is 0 while unknown.
    /// - `cancel`: stops the transfer and keeps the part file for a later resume.
    pub async fn download(
        &self,
        url: &str,
        target: &Path,
        expected_sha256: Option<&str>,
        expected_bytes: u64,
        progress: Option<&Progress>,
        cancel: &CancellationToken,
    ) -> Result<Outcome> {
        if tokio::fs::metadata(target).await.is_ok() {
            let size = file_size(target).await;
            let matches = match expected_sha256 {
                None => true,
                Some(expected) => expected.eq_ignore_ascii_case(&sha256_file(target).await?),
            };
            if matches {
                report(progress, size, size);
                return Ok(Outcome::AlreadyPresent);
            }
            tracing::warn!(
                "Existing file {} does not match its expected hash; downloading again",
                target.display()
            );
            tokio::fs::remove_file(target)
                .await
                .with_context(|| format!("Could not delete {}", target.display()))?;
        }
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Could not create {}", parent.display()))?;
        }
        let part = part_path(target);
        let mut existing = match tokio::fs::metadata(&part).await {
            Ok(m) => m.len(),
            Err(_) => 0,
        };

        let mut req = self.client.get(url).header(USER_AGENT, RUNTIME_USER_AGENT);
        if existing > 0 {
            req = req.header(RANGE, format!("bytes={existing}-"));
        }
        // As with Java's request timeout, the six hours bound the wait for the answer to start,
        // not the transfer itself.
        let response = tokio::select! {
            r = tokio::time::timeout(RESPONSE_TIMEOUT, req.send()) => match r {
                Ok(r) => r.with_context(|| format!("Could not download {url}"))?,
                Err(_) => bail!("Could not download {url}: request timed out"),
            },
            _ = cancel.cancelled() => return Ok(Outcome::Cancelled),
        };
        let status = response.status();
        let append = if status == StatusCode::PARTIAL_CONTENT {
            true
        } else if status == StatusCode::OK {
            if existing > 0 {
                tracing::info!("Server ignored Range for {url}; restarting download");
                let _ = tokio::fs::remove_file(&part).await;
                existing = 0;
            }
            false
        } else if status == StatusCode::RANGE_NOT_SATISFIABLE && existing > 0 {
            // Part is already complete; fall through to verification.
            drop(response);
            return finish(&part, target, expected_sha256, progress).await;
        } else {
            bail!("HTTP {} downloading {url}", status.as_u16());
        };

        let remaining = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let total = match remaining {
            Some(r) => existing + r,
            None => expected_bytes,
        };
        let mut done = existing;
        report(progress, done, total);

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(&part)
            .await
            .with_context(|| format!("Could not write {}", part.display()))?;
        let mut out = tokio::io::BufWriter::with_capacity(1 << 16, &mut file);
        let mut stream = response.bytes_stream();
        let mut last_report = Instant::now();
        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => None,
                chunk = stream.next() => Some(chunk),
            };
            let Some(chunk) = next else {
                out.flush().await?;
                tracing::info!(
                    "Download cancelled at {done} bytes; keeping {}",
                    part.file_name().unwrap_or_default().to_string_lossy()
                );
                return Ok(Outcome::Cancelled);
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk.map_err(|e| anyhow!("Download of {url} was interrupted: {e}"))?;
            out.write_all(&chunk)
                .await
                .with_context(|| format!("Could not write {}", part.display()))?;
            done += chunk.len() as u64;
            if last_report.elapsed() > Duration::from_millis(200) {
                report(progress, done, total);
                last_report = Instant::now();
            }
        }
        out.flush().await?;
        drop(out);
        file.flush().await?;
        drop(file);
        report(progress, done, if total > 0 { total } else { done });
        finish(&part, target, expected_sha256, progress).await
    }
}

async fn finish(
    part: &Path,
    target: &Path,
    expected_sha256: Option<&str>,
    progress: Option<&Progress>,
) -> Result<Outcome> {
    if let Some(expected) = expected_sha256 {
        let actual = sha256_file(part).await?;
        if !expected.eq_ignore_ascii_case(&actual) {
            let _ = tokio::fs::remove_file(part).await;
            bail!(
                "Checksum mismatch for {}: expected {expected} got {actual}",
                target.file_name().unwrap_or_default().to_string_lossy()
            );
        }
    }
    tokio::fs::rename(part, target)
        .await
        .with_context(|| format!("Could not move {} into place", target.display()))?;
    let size = file_size(target).await;
    report(progress, size, size);
    Ok(Outcome::Completed)
}

/// `<file>.part` beside the file: where a download in progress lives.
pub fn part_path(target: &Path) -> PathBuf {
    with_suffix(target, ".part")
}

/// The path with `suffix` appended to its file name (`a.gguf` + `.json` = `a.gguf.json`).
pub(crate) fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

async fn file_size(path: &Path) -> u64 {
    tokio::fs::metadata(path)
        .await
        .map(|m| m.len())
        .unwrap_or(0)
}

/// The sha256 of a file as lowercase hex, read off the async threads (models are gigabytes).
pub async fn sha256_file(path: &Path) -> Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || sha256_file_blocking(&path))
        .await
        .map_err(|e| anyhow!("hashing was interrupted: {e}"))?
}

/// The sha256 of a file as lowercase hex.
pub fn sha256_file_blocking(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file =
        std::fs::File::open(path).with_context(|| format!("Could not read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("Could not read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{HeaderMap, Response, StatusCode as AxStatus};
    use axum::routing::get;
    use axum::Router;
    use parking_lot::Mutex;
    use std::sync::Arc;

    /// Serves `router` on 127.0.0.1:0 and returns its base URL.
    pub(crate) async fn serve(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        format!("http://{addr}")
    }

    fn payload() -> Vec<u8> {
        (0..200_000u32).map(|i| (i % 251) as u8).collect()
    }

    fn sha(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    /// A file server that honours Range (or not), recording the Range headers it saw.
    fn file_router(
        data: Arc<Vec<u8>>,
        honour_range: bool,
        seen: Arc<Mutex<Vec<String>>>,
    ) -> Router {
        Router::new().route(
            "/file",
            get(move |headers: HeaderMap| {
                let data = data.clone();
                let seen = seen.clone();
                async move {
                    let range = headers
                        .get("range")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    seen.lock().push(range.clone().unwrap_or_default());
                    if let (true, Some(r)) = (honour_range, range) {
                        let from: usize = r
                            .trim_start_matches("bytes=")
                            .trim_end_matches('-')
                            .parse()
                            .unwrap();
                        if from >= data.len() {
                            return Response::builder()
                                .status(AxStatus::RANGE_NOT_SATISFIABLE)
                                .body(Body::empty())
                                .unwrap();
                        }
                        return Response::builder()
                            .status(AxStatus::PARTIAL_CONTENT)
                            .header("content-length", data.len() - from)
                            .body(Body::from(data[from..].to_vec()))
                            .unwrap();
                    }
                    Response::builder()
                        .status(AxStatus::OK)
                        .header("content-length", data.len())
                        .body(Body::from(data.to_vec()))
                        .unwrap()
                }
            }),
        )
    }

    #[tokio::test]
    async fn downloads_verifies_and_reports_progress() {
        let data = Arc::new(payload());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let base = serve(file_router(data.clone(), true, seen.clone())).await;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sub").join("model.gguf");
        let last = Arc::new(Mutex::new((0u64, 0u64)));
        let l = last.clone();
        let progress: Progress = Arc::new(move |d, t| *l.lock() = (d, t));
        let d = Downloader::new();
        let outcome = d
            .download(
                &format!("{base}/file"),
                &target,
                Some(&sha(&data).to_uppercase()),
                0,
                Some(&progress),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(outcome, Outcome::Completed);
        assert_eq!(std::fs::read(&target).unwrap(), *data);
        assert!(!part_path(&target).exists());
        assert_eq!(*last.lock(), (data.len() as u64, data.len() as u64));

        // Present and matching: nothing is fetched again.
        let again = d
            .download(
                &format!("{base}/file"),
                &target,
                Some(&sha(&data)),
                0,
                None,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(again, Outcome::AlreadyPresent);
        assert_eq!(seen.lock().len(), 1);
    }

    #[tokio::test]
    async fn resumes_a_part_with_a_range_request() {
        let data = Arc::new(payload());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let base = serve(file_router(data.clone(), true, seen.clone())).await;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("model.gguf");
        std::fs::write(part_path(&target), &data[..50_000]).unwrap();
        let outcome = Downloader::new()
            .download(
                &format!("{base}/file"),
                &target,
                Some(&sha(&data)),
                0,
                None,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(outcome, Outcome::Completed);
        assert_eq!(seen.lock().as_slice(), ["bytes=50000-"]);
        assert_eq!(std::fs::read(&target).unwrap(), *data);
    }

    #[tokio::test]
    async fn restarts_when_the_server_ignores_range() {
        let data = Arc::new(payload());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let base = serve(file_router(data.clone(), false, seen.clone())).await;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("model.gguf");
        std::fs::write(part_path(&target), b"garbage that is not the start").unwrap();
        let outcome = Downloader::new()
            .download(
                &format!("{base}/file"),
                &target,
                Some(&sha(&data)),
                0,
                None,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(outcome, Outcome::Completed);
        assert_eq!(std::fs::read(&target).unwrap(), *data);
    }

    #[tokio::test]
    async fn a_complete_part_is_verified_on_416() {
        let data = Arc::new(payload());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let base = serve(file_router(data.clone(), true, seen.clone())).await;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("model.gguf");
        std::fs::write(part_path(&target), &*data).unwrap();
        let outcome = Downloader::new()
            .download(
                &format!("{base}/file"),
                &target,
                Some(&sha(&data)),
                0,
                None,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(outcome, Outcome::Completed);
        assert_eq!(std::fs::read(&target).unwrap(), *data);
    }

    #[tokio::test]
    async fn a_checksum_mismatch_fails_and_drops_the_part() {
        let data = Arc::new(payload());
        let base = serve(file_router(
            data.clone(),
            true,
            Arc::new(Mutex::new(Vec::new())),
        ))
        .await;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("model.gguf");
        let err = Downloader::new()
            .download(
                &format!("{base}/file"),
                &target,
                Some(&"0".repeat(64)),
                0,
                None,
                &CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("Checksum mismatch for model.gguf: expected 000"),
            "{err}"
        );
        assert!(!target.exists());
        assert!(!part_path(&target).exists());
    }

    #[tokio::test]
    async fn http_errors_name_the_status() {
        let base = serve(Router::new()).await;
        let dir = tempfile::tempdir().unwrap();
        let url = format!("{base}/missing");
        let err = Downloader::new()
            .download(
                &url,
                &dir.path().join("x"),
                None,
                0,
                None,
                &CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, format!("HTTP 404 downloading {url}"));
    }

    #[tokio::test]
    async fn cancelling_keeps_the_part_for_a_resume() {
        // A server that sends a first chunk and then stalls.
        let router = Router::new().route(
            "/slow",
            get(|| async {
                let stream = futures::stream::unfold(0u32, |n| async move {
                    if n == 0 {
                        Some((
                            Ok::<_, std::io::Error>(bytes::Bytes::from(vec![7u8; 4096])),
                            1,
                        ))
                    } else {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        None
                    }
                });
                Response::builder()
                    .header("content-length", 1_000_000)
                    .body(Body::from_stream(stream))
                    .unwrap()
            }),
        );
        let base = serve(router).await;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("model.gguf");
        let cancel = CancellationToken::new();
        let seen = Arc::new(Mutex::new(0u64));
        let s = seen.clone();
        let c = cancel.clone();
        let progress: Progress = Arc::new(move |d, _| {
            *s.lock() = d;
            if d > 0 {
                c.cancel();
            }
        });
        let d = Downloader::new();
        let url = format!("{base}/slow");
        let task = d.download(&url, &target, None, 0, Some(&progress), &cancel);
        // The first progress report comes before any byte; cancel from the outside after a moment.
        let cancel2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            cancel2.cancel();
        });
        let outcome = tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome, Outcome::Cancelled);
        assert!(!target.exists());
        assert_eq!(std::fs::metadata(part_path(&target)).unwrap().len(), 4096);
    }

    #[test]
    fn suffixes_append_to_the_file_name() {
        assert_eq!(
            with_suffix(Path::new("a/b.gguf"), ".json"),
            Path::new("a/b.gguf.json")
        );
        assert_eq!(part_path(Path::new("x.zip")), Path::new("x.zip.part"));
    }
}

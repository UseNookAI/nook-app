//! Ports `gateway/VideoController.java`.
//!
//! Video generation on the gateway, shaped like OpenAI's Videos API: a clip is a job that is
//! created, polled until it completes, then downloaded. Clips take minutes, so nothing here waits
//! for one. Statuses are OpenAI's (queued, in_progress, completed, failed) plus cancelled.

use std::io::SeekFrom;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::{error, is_json, json_response, read_json, spring_error, text_of};
use crate::video::{Clip, Status, SubmitError, VideoStudio};

/// The video routes.
pub fn router(studio: Arc<VideoStudio>) -> Router {
    Router::new()
        .route("/v1/videos", post(create).get(list))
        .route("/v1/videos/{id}", get(get_one).delete(delete_one))
        .route("/v1/videos/{id}/content", get(content))
        .route("/v1/videos/{id}/cancel", post(cancel))
        .with_state(studio)
}

async fn create(
    State(studio): State<Arc<VideoStudio>>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !is_json(&headers) {
        return spring_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, uri.path());
    }
    let json = match read_json(&body) {
        Ok(json) => json,
        Err(bad) => return bad,
    };
    let Some(prompt) = text_of(&json, "prompt").filter(|p| !p.trim().is_empty()) else {
        return error(
            StatusCode::BAD_REQUEST,
            "The request must include a prompt.".to_string(),
        );
    };
    match studio.submit(&prompt, text_of(&json, "model").as_deref()) {
        Ok(clip) => json_response(StatusCode::OK, &to_json(&clip)),
        Err(e @ SubmitError::EmptyPrompt) => error(StatusCode::BAD_REQUEST, e.to_string()),
        Err(e @ SubmitError::Unavailable(_)) => {
            error(StatusCode::SERVICE_UNAVAILABLE, e.to_string())
        }
    }
}

#[derive(Serialize)]
struct VideoList {
    object: &'static str,
    data: Vec<VideoJson>,
}

async fn list(State(studio): State<Arc<VideoStudio>>) -> Response {
    json_response(
        StatusCode::OK,
        &VideoList {
            object: "list",
            data: studio.clips().iter().map(to_json).collect(),
        },
    )
}

async fn get_one(State(studio): State<Arc<VideoStudio>>, Path(id): Path<String>) -> Response {
    match studio.clip(&id) {
        Some(c) => json_response(StatusCode::OK, &to_json(&c)),
        None => not_found(&id),
    }
}

/// The finished clip's AVI. A `Range: bytes=a-b` request gets that part (206), as Spring served
/// a file resource.
async fn content(
    State(studio): State<Arc<VideoStudio>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(c) = studio.clip(&id) else {
        return not_found(&id);
    };
    let file = match (&c.status, &c.file) {
        (Status::Done, Some(file)) => file.clone(),
        _ => {
            return error(
                StatusCode::CONFLICT,
                format!("Video {id} is {}, not completed.", status(&c)),
            )
        }
    };
    let opened = async {
        let mut f = tokio::fs::File::open(&file).await?;
        let total = f.metadata().await?.len();
        let range = headers
            .get(header::RANGE)
            .and_then(|h| h.to_str().ok())
            .map(|r| byte_range(r, total));
        let (status, start, len) = match range {
            None | Some(Range::Ignored) => (StatusCode::OK, 0, total),
            Some(Range::Part(start, end)) => (StatusCode::PARTIAL_CONTENT, start, end - start + 1),
            Some(Range::Unsatisfiable) => (StatusCode::RANGE_NOT_SATISFIABLE, 0, 0),
        };
        if start > 0 {
            f.seek(SeekFrom::Start(start)).await?;
        }
        std::io::Result::Ok((f.take(len), status, start, len, total))
    };
    let (reader, status, start, len, total) = match opened.await {
        Ok(v) => v,
        Err(e) => {
            return super::server_error(&anyhow::anyhow!("Could not read {}: {e}", file.display()))
        }
    };
    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, "video/x-msvideo")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{id}.avi\""),
        )
        .header(header::ACCEPT_RANGES, "bytes")
        .status(status);
    response = match status {
        StatusCode::PARTIAL_CONTENT => response
            .header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{}/{total}", start + len - 1),
            )
            .header(header::CONTENT_LENGTH, len),
        StatusCode::RANGE_NOT_SATISFIABLE => {
            response.header(header::CONTENT_RANGE, format!("bytes */{total}"))
        }
        _ => response.header(header::CONTENT_LENGTH, total),
    };
    let body = if status == StatusCode::RANGE_NOT_SATISFIABLE {
        Body::empty()
    } else {
        Body::from_stream(tokio_util::io::ReaderStream::new(reader))
    };
    response
        .body(body)
        .unwrap_or_else(|e| super::server_error(&anyhow::anyhow!(e)))
}

/// A `Range` header against a file of `total` bytes.
#[derive(Debug, PartialEq, Eq)]
enum Range {
    /// Inclusive first and last byte.
    Part(u64, u64),
    Unsatisfiable,
    /// Not one byte range this reads (several ranges, other units): the whole file goes.
    Ignored,
}

fn byte_range(header: &str, total: u64) -> Range {
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        return Range::Ignored;
    };
    if spec.contains(',') {
        return Range::Ignored;
    }
    let Some((a, b)) = spec.trim().split_once('-') else {
        return Range::Ignored;
    };
    let (a, b) = (a.trim(), b.trim());
    let parsed = match (a.is_empty(), b.is_empty()) {
        // the last b bytes
        (true, false) => match b.parse::<u64>() {
            Ok(0) => return Range::Unsatisfiable,
            Ok(n) => Some((total.saturating_sub(n), total.saturating_sub(1))),
            Err(_) => None,
        },
        (false, true) => a.parse::<u64>().ok().map(|s| (s, total.saturating_sub(1))),
        (false, false) => match (a.parse::<u64>(), b.parse::<u64>()) {
            (Ok(s), Ok(e)) if s <= e => Some((s, e.min(total.saturating_sub(1)))),
            _ => None,
        },
        (true, true) => None,
    };
    match parsed {
        None => Range::Ignored,
        Some((start, _)) if total == 0 || start >= total => Range::Unsatisfiable,
        Some((start, end)) => Range::Part(start, end),
    }
}

async fn cancel(State(studio): State<Arc<VideoStudio>>, Path(id): Path<String>) -> Response {
    if studio.clip(&id).is_none() {
        return not_found(&id);
    }
    studio.cancel(&id);
    match studio.clip(&id) {
        Some(c) => json_response(StatusCode::OK, &to_json(&c)),
        None => not_found(&id),
    }
}

#[derive(Serialize)]
struct Deleted<'a> {
    id: &'a str,
    object: &'static str,
    deleted: bool,
}

async fn delete_one(State(studio): State<Arc<VideoStudio>>, Path(id): Path<String>) -> Response {
    let Some(c) = studio.clip(&id) else {
        return not_found(&id);
    };
    if !studio.delete(&id) {
        return error(
            StatusCode::CONFLICT,
            format!("Video {id} is still {}; cancel it first.", status(&c)),
        );
    }
    json_response(
        StatusCode::OK,
        &Deleted {
            id: &id,
            object: "video.deleted",
            deleted: true,
        },
    )
}

#[derive(Serialize)]
struct ErrorMessage {
    message: String,
}

/// A clip as the Videos API shows one, in the original's field order.
#[derive(Serialize)]
pub struct VideoJson {
    id: String,
    object: &'static str,
    model: Option<String>,
    status: &'static str,
    progress: i64,
    created_at: i64,
    prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage_done: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage_total: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seconds: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frames: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorMessage>,
}

pub fn to_json(c: &Clip) -> VideoJson {
    let done = c.status == Status::Done;
    let staged = c.stage.is_some() && c.total > 0;
    VideoJson {
        id: c.id.clone(),
        object: "video",
        model: c.model_id.clone(),
        status: status(c),
        progress: (c.progress() * 100.0).round() as i64,
        created_at: c.created_at.timestamp(),
        prompt: c.prompt.clone(),
        stage: c.stage.map(|s| s.to_string().to_lowercase()),
        stage_done: staged.then_some(c.done),
        stage_total: staged.then_some(c.total),
        size: done.then(|| format!("{}x{}", c.width, c.height)),
        seconds: done.then(|| one_decimal(c.seconds())),
        frames: done.then_some(c.frames),
        fps: done.then_some(c.fps),
        seed: done.then_some(c.seed),
        elapsed_ms: done.then_some(c.elapsed_ms),
        file: done.then(|| {
            c.file
                .as_ref()
                .map(|f| f.display().to_string())
                .unwrap_or_default()
        }),
        error: c
            .error
            .as_ref()
            .map(|m| ErrorMessage { message: m.clone() }),
    }
}

/// OpenAI's name for a clip's status.
pub fn status(c: &Clip) -> &'static str {
    match c.status {
        Status::Queued => "queued",
        Status::Running => "in_progress",
        Status::Done => "completed",
        Status::Failed => "failed",
        Status::Cancelled => "cancelled",
    }
}

/// `String.format(Locale.ROOT, "%.1f", x)`: the shortest decimal form of `x` rounded half up to
/// one decimal, as Java's formatter rounds.
fn one_decimal(x: f64) -> String {
    let s = format!("{}", x.max(0.0));
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let int: u64 = int.parse().unwrap_or(0);
    let mut digits = frac.bytes().map(|b| (b - b'0') as u64);
    let first = digits.next().unwrap_or(0);
    let up = digits.next().is_some_and(|d| d >= 5);
    let tenths = int * 10 + first + up as u64;
    format!("{}.{}", tenths / 10, tenths % 10)
}

fn not_found(id: &str) -> Response {
    error(StatusCode::NOT_FOUND, format!("No video {id}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_round_half_up_as_java_formats_them() {
        assert_eq!(one_decimal(33.0 / 16.0), "2.1");
        assert_eq!(one_decimal(36.0 / 16.0), "2.3", "an exact half goes up");
        assert_eq!(one_decimal(2.05), "2.1");
        assert_eq!(one_decimal(0.0), "0.0");
        assert_eq!(one_decimal(121.0 / 24.0), "5.0");
        assert_eq!(one_decimal(9.96), "10.0");
    }

    #[test]
    fn ranges_read_as_a_file_resource_served_them() {
        assert_eq!(byte_range("bytes=0-3", 10), Range::Part(0, 3));
        assert_eq!(byte_range("bytes=4-", 10), Range::Part(4, 9));
        assert_eq!(byte_range("bytes=-3", 10), Range::Part(7, 9));
        assert_eq!(byte_range("bytes=5-100", 10), Range::Part(5, 9));
        assert_eq!(byte_range("bytes=10-", 10), Range::Unsatisfiable);
        assert_eq!(byte_range("bytes=0-1,4-5", 10), Range::Ignored);
        assert_eq!(byte_range("items=0-1", 10), Range::Ignored);
        assert_eq!(byte_range("bytes=3-1", 10), Range::Ignored);
    }
}

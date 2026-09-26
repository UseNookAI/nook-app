//! Ports `gateway/EnvelopeStrip.java`.
//!
//! Keeps the empty `<think></think>` envelope, which Qwen3's template leaves at the head of every
//! reply when thinking is off, out of what the gateway hands an OpenAI-style client (the Codex
//! report's P3 and Fable's QA of 2026-09-22: every default reply through `/v1/chat/completions`
//! began with the pair). A whole reply is rewritten in place; a streamed one is passed event by
//! event through [`EnvelopeFilter`], so real thinking (the switch on) streams through as it
//! always did and nothing else about the events changes.
//!
//! JSON that is rewritten comes out compact with its keys in serde_json's order; bodies and
//! events that need no change pass byte for byte.

use std::borrow::Cow;

use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde_json::Value;

use crate::runtime::thinking::EnvelopeFilter;

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";
const DATA: &str = "data: ";
const DONE: &str = "data: [DONE]";

/// The non-streaming reply with the envelope removed from each choice's content; unparseable
/// bodies pass unchanged.
pub fn whole(body: &[u8]) -> Cow<'_, [u8]> {
    let Ok(mut root) = serde_json::from_slice::<Value>(body) else {
        return Cow::Borrowed(body);
    };
    let Some(choices) = root.get_mut("choices").and_then(Value::as_array_mut) else {
        return Cow::Borrowed(body);
    };
    let mut changed = false;
    for choice in choices.iter_mut() {
        let Some(content) = choice
            .get_mut("message")
            .filter(|m| m.is_object())
            .and_then(|m| m.get_mut("content"))
        else {
            continue;
        };
        let Some(text) = content.as_str() else {
            continue;
        };
        let cleaned = strip_empty_envelope(text);
        if cleaned != text {
            *content = Value::String(cleaned.into_owned());
            changed = true;
        }
    }
    if !changed {
        return Cow::Borrowed(body);
    }
    match serde_json::to_vec(&root) {
        Ok(bytes) => Cow::Owned(bytes),
        Err(_) => Cow::Borrowed(body),
    }
}

/// Only the empty envelope goes; a reply that thought keeps its block, since the client may want
/// it.
pub fn strip_empty_envelope(content: &str) -> Cow<'_, str> {
    let t = content.trim_start();
    let Some(rest) = t.strip_prefix(OPEN) else {
        return Cow::Borrowed(content);
    };
    let Some(after) = rest.trim_start().strip_prefix(CLOSE) else {
        return Cow::Borrowed(content);
    };
    Cow::Borrowed(after.trim_start())
}

/// Copies a server-sent-event stream from the engine to the client, passing each event's
/// `choices[].delta.content` through the envelope filter: an event whose content the filter
/// holds back is not sent; held text that was not an envelope is released with the next content;
/// whatever is still held when the stream ends goes out in a final event cloned from the last one
/// seen.
///
/// Lines end as `BufferedReader.readLine` ended them (`\n`, `\r\n` or `\r`) and each goes out
/// with `\n`, as soon as it is complete.
pub fn stream<S, E>(upstream: S) -> impl Stream<Item = Result<Bytes, E>> + Send
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: Send + 'static,
{
    struct State<S> {
        upstream: std::pin::Pin<Box<S>>,
        lines: LineSplitter,
        strip: EventStrip,
        ended: bool,
    }
    let state = State {
        upstream: Box::pin(upstream),
        lines: LineSplitter::default(),
        strip: EventStrip::default(),
        ended: false,
    };
    futures::stream::unfold(state, |mut st| async move {
        loop {
            if st.ended {
                return None;
            }
            let mut out = Vec::new();
            match st.upstream.next().await {
                Some(Ok(chunk)) => {
                    for line in st.lines.push(&chunk) {
                        st.strip.line(&line, &mut out);
                    }
                }
                Some(Err(e)) => {
                    st.ended = true;
                    return Some((Err(e), st));
                }
                None => {
                    st.ended = true;
                    if let Some(line) = st.lines.finish() {
                        st.strip.line(&line, &mut out);
                    }
                }
            }
            if !out.is_empty() {
                return Some((Ok(Bytes::from(out)), st));
            }
        }
    })
}

/// The per-event work of [`stream`]: one input line in, the lines to send out.
#[derive(Default)]
pub struct EventStrip {
    filter: EnvelopeFilter,
    last_event: Option<String>,
}

impl EventStrip {
    /// Handles one line (without its ending), appending what goes out, each line ending in `\n`.
    pub fn line(&mut self, line: &str, out: &mut Vec<u8>) {
        if !line.starts_with(DATA) || line == DONE {
            if line == DONE {
                let tail = self.filter.flush();
                if !tail.is_empty() {
                    if let Some(extra) = self
                        .last_event
                        .as_deref()
                        .and_then(|e| with_content(e, &tail))
                    {
                        write(out, &format!("{DATA}{extra}"));
                    }
                }
            }
            write(out, line);
            return;
        }
        let json = &line[DATA.len()..];
        let mut rewritten: Option<String> = None;
        if let Ok(mut root) = serde_json::from_str::<Value>(json) {
            let finish_is_null = root
                .pointer("/choices/0/finish_reason")
                .is_some_and(Value::is_null);
            if let Some(delta) = root
                .pointer_mut("/choices/0/delta")
                .and_then(Value::as_object_mut)
            {
                if let Some(content) = delta.get("content").and_then(Value::as_str) {
                    let content = content.to_string();
                    self.last_event = Some(json.to_string());
                    let shown = self.filter.push(&content);
                    if shown.is_empty() && delta.len() == 1 && finish_is_null {
                        return; // held back: the envelope, or text that may still be one
                    }
                    if shown != content {
                        delta.insert("content".into(), Value::String(shown));
                        rewritten = serde_json::to_string(&root).ok();
                    }
                }
            }
        }
        // Not JSON we understand, or nothing to change: it passes through.
        write(
            out,
            &format!("{DATA}{}", rewritten.as_deref().unwrap_or(json)),
        );
    }
}

/// The last event seen with other content, and no finish reason, for what was still held when
/// the stream ended.
fn with_content(event_json: &str, content: &str) -> Option<String> {
    let mut root: Value = serde_json::from_str(event_json).ok()?;
    let choice = root.pointer_mut("/choices/0")?.as_object_mut()?;
    let delta = choice.get_mut("delta")?.as_object_mut()?;
    delta.insert("content".into(), Value::String(content.to_string()));
    choice.insert("finish_reason".into(), Value::Null);
    serde_json::to_string(&root).ok()
}

fn write(out: &mut Vec<u8>, line: &str) {
    out.extend_from_slice(line.as_bytes());
    out.push(b'\n');
}

/// Splits bytes into lines as `BufferedReader.readLine` did: a line ends at `\n`, `\r\n` or a
/// lone `\r`; the text is read as UTF-8, malformed bytes replaced.
#[derive(Default)]
struct LineSplitter {
    buf: Vec<u8>,
}

impl LineSplitter {
    /// The lines completed by this chunk.
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut lines = Vec::new();
        let mut start = 0;
        let mut i = 0;
        while i < self.buf.len() {
            match self.buf[i] {
                b'\n' => {
                    lines.push(String::from_utf8_lossy(&self.buf[start..i]).into_owned());
                    i += 1;
                    start = i;
                }
                b'\r' => {
                    if i + 1 == self.buf.len() {
                        break; // a \n may follow in the next chunk
                    }
                    lines.push(String::from_utf8_lossy(&self.buf[start..i]).into_owned());
                    i += if self.buf[i + 1] == b'\n' { 2 } else { 1 };
                    start = i;
                }
                _ => i += 1,
            }
        }
        self.buf.drain(..start);
        lines
    }

    /// What is left when the stream ends: a line ended by a final `\r`, or the last line when
    /// it had no ending.
    fn finish(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.buf);
        match rest.strip_suffix(b"\r") {
            Some(line) => Some(String::from_utf8_lossy(line).into_owned()),
            None => (!rest.is_empty()).then(|| String::from_utf8_lossy(&rest).into_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(sse: &str, chunk: usize) -> String {
        let chunks: Vec<Result<Bytes, std::io::Error>> = sse
            .as_bytes()
            .chunks(chunk)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        let out =
            futures::executor::block_on(stream(futures::stream::iter(chunks)).collect::<Vec<_>>());
        let bytes: Vec<u8> = out.into_iter().flat_map(|r| r.unwrap().to_vec()).collect();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn a_whole_reply_loses_the_empty_envelope_only() {
        let reply = r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"<think>\n\n</think>\n\nNOOK_QA_OK"},"finish_reason":"stop"}],"usage":{"completion_tokens":6}}"#;
        let out = String::from_utf8(whole(reply.as_bytes()).into_owned()).unwrap();
        assert!(out.contains(r#""content":"NOOK_QA_OK""#), "{out}");
        assert!(
            out.contains(r#""completion_tokens":6"#),
            "everything else stays"
        );

        let thought =
            r#"{"choices":[{"message":{"content":"<think>\nOne word.\n</think>\n\nNOOK_QA_OK"}}]}"#;
        assert!(
            matches!(whole(thought.as_bytes()), Cow::Borrowed(b) if b == thought.as_bytes()),
            "the client asked for thinking: it keeps it"
        );
        let junk = b"not json";
        assert!(matches!(whole(junk), Cow::Borrowed(b) if std::ptr::eq(b, junk.as_slice())));
        assert_eq!(
            strip_empty_envelope("<think>\n\n</think>\n\nNOOK_QA_OK"),
            "NOOK_QA_OK"
        );
        assert_eq!(strip_empty_envelope("plain"), "plain");
        assert_eq!(
            strip_empty_envelope("<think>x</think>y"),
            "<think>x</think>y"
        );
    }

    #[test]
    fn a_streamed_reply_loses_the_envelope_events_and_keeps_the_rest() {
        let sse = [
            r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
            "",
            r#"data: {"choices":[{"index":0,"delta":{"content":"<think>"},"finish_reason":null}]}"#,
            "",
            r#"data: {"choices":[{"index":0,"delta":{"content":"\n\n"},"finish_reason":null}]}"#,
            "",
            r#"data: {"choices":[{"index":0,"delta":{"content":"</think>"},"finish_reason":null}]}"#,
            "",
            r#"data: {"choices":[{"index":0,"delta":{"content":"\n\nNOOK"},"finish_reason":null}]}"#,
            "",
            r#"data: {"choices":[{"index":0,"delta":{"content":"_QA_OK"},"finish_reason":null}]}"#,
            "",
            r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"completion_tokens":6}}"#,
            "",
            "data: [DONE]",
            "",
        ]
        .join("\n");
        for chunk in [1, 7, 4096] {
            let got = run(&sse, chunk);
            assert!(!got.contains("<think>"), "{got}");
            assert!(!got.contains("</think>"), "{got}");
            assert!(
                got.contains(r#""content":"NOOK""#),
                "the first word, without the envelope's blank lines: {got}"
            );
            assert!(got.contains(r#""content":"_QA_OK""#), "{got}");
            assert!(got.contains(r#""finish_reason":"stop""#), "{got}");
            assert!(got.contains(r#""completion_tokens":6"#), "{got}");
            assert!(got.trim().ends_with("data: [DONE]"), "{got}");
            assert!(
                got.contains(r#""role":"assistant""#),
                "the opening event with the role stays: {got}"
            );
            assert!(
                got.contains(r#"data: {"choices":[{"index":0,"delta":{"content":"_QA_OK"},"finish_reason":null}]}"#),
                "an event with nothing to change passes byte for byte: {got}"
            );
        }

        // real thinking streams through untouched
        let thinking = [
            r#"data: {"choices":[{"index":0,"delta":{"content":"<think>"},"finish_reason":null}]}"#,
            "",
            r#"data: {"choices":[{"index":0,"delta":{"content":"\nOne word."},"finish_reason":null}]}"#,
            "",
            r#"data: {"choices":[{"index":0,"delta":{"content":"\n</think>\n\nNOOK_QA_OK"},"finish_reason":null}]}"#,
            "",
            "data: [DONE]",
            "",
        ]
        .join("\n");
        let got2 = run(&thinking, 5);
        assert!(
            got2.contains("<think>") && got2.contains("One word.") && got2.contains("NOOK_QA_OK"),
            "{got2}"
        );

        // a stream that ends inside what might have been an envelope releases what it held
        let cut = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"<think>\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n";
        let got3 = run(cut, 3);
        assert!(got3.contains(r#""content":"<think>""#), "{got3}");
    }

    #[test]
    fn lines_end_as_read_line_ended_them() {
        let mut s = LineSplitter::default();
        assert_eq!(s.push(b"a\r"), Vec::<String>::new(), "a \\n may follow");
        assert_eq!(s.push(b"\nb\rc\n"), vec!["a", "b", "c"]);
        assert_eq!(s.push(b"\n"), vec![""]);
        assert_eq!(s.push(b"tail"), Vec::<String>::new());
        assert_eq!(s.finish().as_deref(), Some("tail"));
        assert_eq!(s.finish(), None);
        let mut cr = LineSplitter::default();
        assert!(cr.push(b"x\r").is_empty());
        assert_eq!(cr.finish().as_deref(), Some("x"));
    }
}

//! Ports `runtime/Thinking.java`.
//!
//! Whether a model thinks before it answers, decided in one place (the Codex report of
//! 2026-09-22, P3: with no policy, Qwen3 spent a 256-token budget thinking about "reply with one
//! word" and never answered).
//!
//! One switch in `workers.json`: `"thinking"` for the desktop chat and the gateway's
//! `/v1/chat/completions` when the client did not say (default off: a local 8B answers a chat
//! turn in a second without it).
//!
//! How it is switched depends on the family: Qwen3's chat template takes `enable_thinking`;
//! gpt-oss cannot stop reasoning but takes `reasoning_effort` (low in place of off); other
//! families do not think unasked and get nothing. A request that already carries
//! `chat_template_kwargs` or `reasoning_effort` is the caller's decision and is left alone. The
//! engine runs with `--reasoning-format none`, so what a model does think arrives inline as a
//! `<think>` block; [`stripped`] removes it for callers that want the answer, and
//! [`ran_out_while_thinking`] names the failure the report saw, so it can be reported or retried
//! rather than read as an empty answer.

use std::collections::BTreeMap;

use serde_json::{json, Value};

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

/// True when the family can be told not to think (`enable_thinking`) or to think less.
pub fn switchable(model_id: &str) -> bool {
    let id = model_id.to_lowercase();
    id.contains("qwen3") || id.starts_with("gpt-oss")
}

fn caller_decided(body: &serde_json::Map<String, Value>) -> bool {
    body.contains_key("chat_template_kwargs") || body.contains_key("reasoning_effort")
}

/// Applies the policy to an OpenAI-style request body for `model_id`, unless the body already
/// says (`chat_template_kwargs` or `reasoning_effort` present). Returns true when the body was
/// changed.
pub fn apply(body: &mut Value, model_id: &str, think: bool) -> bool {
    let Some(obj) = body.as_object_mut() else {
        return false;
    };
    if caller_decided(obj) {
        return false;
    }
    let id = model_id.to_lowercase();
    if id.contains("qwen3") {
        obj.insert(
            "chat_template_kwargs".into(),
            json!({ "enable_thinking": think }),
        );
        return true;
    }
    if id.starts_with("gpt-oss") {
        obj.insert(
            "chat_template_kwargs".into(),
            json!({ "reasoning_effort": if think { "medium" } else { "low" } }),
        );
        return true;
    }
    false
}

/// The same policy for a call constrained by a JSON schema (the reading tools). Qwen3's
/// `enable_thinking=false` makes the template prefill an empty `<think></think>` in the assistant
/// turn, and llama-server's grammar sampler then fails to start ("Unexpected empty grammar stack
/// after accepting piece: <think>", Fable's QA of 2026-09-22 01:57); the soft switch,
/// `/no_think` at the end of the user message, leaves the prefix alone and the model answers in
/// the grammar. gpt-oss takes `reasoning_effort` as before, which changes the system prompt, not
/// the assistant prefix. Returns true when the body was changed.
pub fn apply_to_constrained(body: &mut Value, model_id: &str, think: bool) -> bool {
    let Some(obj) = body.as_object_mut() else {
        return false;
    };
    if caller_decided(obj) {
        return false;
    }
    if model_id.to_lowercase().contains("qwen3") {
        if think {
            return false;
        }
        let Some(msgs) = obj.get_mut("messages").and_then(Value::as_array_mut) else {
            return false;
        };
        for m in msgs.iter_mut().rev() {
            let Some(m) = m.as_object_mut() else { continue };
            if m.get("role").and_then(Value::as_str) != Some("user") {
                continue;
            }
            // Content in parts (text and images): the switch goes in as one more text part.
            if let Some(parts) = m.get_mut("content").and_then(Value::as_array_mut) {
                let last_text = parts
                    .iter()
                    .rev()
                    .find_map(|p| p.get("text").and_then(Value::as_str));
                if last_text.is_some_and(|t| t.ends_with("/no_think")) {
                    return false;
                }
                parts.push(json!({ "type": "text", "text": "/no_think" }));
                return true;
            }
            let c = match m.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Number(n)) => n.to_string(),
                Some(Value::Bool(b)) => b.to_string(),
                _ => String::new(),
            };
            if c.ends_with("/no_think") {
                return false;
            }
            m.insert("content".into(), Value::String(format!("{c} /no_think")));
            return true;
        }
        return false;
    }
    apply(body, model_id, think)
}

/// The desktop chat and gateway default from `workers.json`: `"thinking": "on"`; off otherwise.
pub fn chat_thinks(worker_preferences: &BTreeMap<String, String>) -> bool {
    worker_preferences
        .get("thinking")
        .map(String::as_str)
        .unwrap_or("off")
        .eq_ignore_ascii_case("on")
}

/// The content without its `<think>` blocks (complete ones; an unfinished one is left, see
/// [`ran_out_while_thinking`]), trimmed.
pub fn stripped(content: &str) -> String {
    let mut out = content.to_string();
    while let Some(i) = out.find(OPEN) {
        let Some(j) = out[i..].find(CLOSE).map(|j| j + i) else {
            break;
        };
        out.replace_range(i..j + CLOSE.len(), "");
    }
    out.trim().to_string()
}

/// For a streamed reply: holds back the empty `<think></think>` envelope Qwen3's template leaves
/// when thinking is off, and passes everything else through as it arrives, so the chat bubble
/// opens on the first word of the answer. Real thinking (the switch on) streams as before.
#[derive(Debug, Default)]
pub struct EnvelopeFilter {
    held: String,
    decided: bool,
    emitted: bool,
}

impl EnvelopeFilter {
    pub fn new() -> EnvelopeFilter {
        EnvelopeFilter::default()
    }

    /// The text to show for this delta: possibly nothing yet, possibly more than the delta.
    pub fn push(&mut self, delta: &str) -> String {
        if self.decided {
            return self.lead(delta.to_string());
        }
        self.held.push_str(delta);
        let t = self.held.trim_start();
        if t.is_empty() && self.held.chars().count() < 64 {
            return String::new();
        }
        if OPEN.starts_with(t) {
            return String::new();
        }
        if let Some(rest) = t.strip_prefix(OPEN) {
            let rest = rest.trim_start();
            if rest.is_empty() || CLOSE.starts_with(rest) {
                return String::new();
            }
            if let Some(after) = rest.strip_prefix(CLOSE) {
                let after = after.to_string();
                self.decided = true;
                self.held.clear();
                return self.lead(after);
            }
        }
        self.decided = true;
        let out = std::mem::take(&mut self.held);
        self.lead(out)
    }

    /// Whatever is still held when the stream ends.
    pub fn flush(&mut self) -> String {
        self.decided = true;
        let out = std::mem::take(&mut self.held);
        self.lead(out)
    }

    fn lead(&mut self, s: String) -> String {
        if self.emitted {
            return s;
        }
        let t = s.trim_start();
        if !t.is_empty() {
            self.emitted = true;
        }
        t.to_string()
    }
}

/// True when the reply is a thinking block that never closed and the engine stopped for length:
/// the budget went on thinking and there is no answer in it.
pub fn ran_out_while_thinking(completion: &Value) -> bool {
    let choice = &completion["choices"][0];
    let content = choice["message"]["content"].as_str().unwrap_or("");
    let length = choice["finish_reason"].as_str() == Some("length");
    match content.rfind(OPEN) {
        Some(open) => length && !content[open..].contains(CLOSE),
        None => false,
    }
}

/// One place decides whether a model thinks, per family, and the envelope and the budget failure
/// are handled.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_per_family() {
        let mut qwen = json!({});
        assert!(apply(&mut qwen, "qwen3-8b-q4km", false));
        assert_eq!(qwen["chat_template_kwargs"]["enable_thinking"], false);
        let mut qwen_on = json!({});
        apply(&mut qwen_on, "Qwen3-Coder-30B-A3B", true);
        assert_eq!(qwen_on["chat_template_kwargs"]["enable_thinking"], true);

        let mut oss = json!({});
        assert!(apply(&mut oss, "gpt-oss-20b", false));
        assert_eq!(
            oss["chat_template_kwargs"]["reasoning_effort"], "low",
            "gpt-oss cannot stop, it can think less"
        );
        let mut oss_on = json!({});
        apply(&mut oss_on, "gpt-oss-20b", true);
        assert_eq!(oss_on["chat_template_kwargs"]["reasoning_effort"], "medium");

        let mut llama = json!({});
        assert!(
            !apply(&mut llama, "llama-3.1-8b", false),
            "a family that does not think unasked gets nothing"
        );
        assert!(llama.get("chat_template_kwargs").is_none());

        let mut decided = json!({"chat_template_kwargs": {"enable_thinking": true}});
        assert!(
            !apply(&mut decided, "qwen3-8b-q4km", false),
            "the caller's own kwargs stand"
        );
        assert_eq!(decided["chat_template_kwargs"]["enable_thinking"], true);
        let mut effort = json!({"reasoning_effort": "high"});
        assert!(!apply(&mut effort, "gpt-oss-20b", false));

        assert!(switchable("qwen3-8b-q4km"));
        assert!(switchable("gpt-oss-20b"));
        assert!(!switchable("mistral-7b"));
        assert!(!switchable(""));
    }

    /// A schema-constrained call: the soft switch, since the template's prefilled envelope breaks
    /// the grammar sampler.
    #[test]
    fn a_constrained_call_uses_the_soft_switch() {
        let mut body = json!({"messages": [
            {"role": "system", "content": "extract"},
            {"role": "user", "content": "the text"}
        ]});
        assert!(apply_to_constrained(&mut body, "qwen3-8b-q4km", false));
        assert!(
            body.get("chat_template_kwargs").is_none(),
            "no kwarg: it would prefill <think></think> ahead of the grammar"
        );
        assert_eq!(body["messages"][1]["content"], "the text /no_think");
        assert!(
            !apply_to_constrained(&mut body, "qwen3-8b-q4km", false),
            "already switched"
        );
        assert_eq!(
            body["messages"][1]["content"], "the text /no_think",
            "not twice"
        );

        let mut thinks = json!({"messages": [{"role": "user", "content": "the text"}]});
        assert!(
            !apply_to_constrained(&mut thinks, "qwen3-8b-q4km", true),
            "thinking on: the template's default"
        );
        assert_eq!(thinks["messages"][0]["content"], "the text");

        let mut oss = json!({"messages": [{"role": "user", "content": "the text"}]});
        assert!(apply_to_constrained(&mut oss, "gpt-oss-20b", false));
        assert_eq!(
            oss["chat_template_kwargs"]["reasoning_effort"], "low",
            "a system-prompt switch is safe with a grammar"
        );
    }

    #[test]
    fn the_switches_in_workers_json() {
        let mut prefs = BTreeMap::new();
        assert!(!chat_thinks(&prefs), "off by default");
        prefs.insert("thinking".to_string(), "on".to_string());
        assert!(chat_thinks(&prefs));
        prefs.insert("thinking".to_string(), "off".to_string());
        assert!(!chat_thinks(&prefs));
    }

    #[test]
    fn the_streamed_envelope_never_reaches_the_bubble() {
        let mut f = EnvelopeFilter::new();
        let mut shown = String::new();
        for d in ["<th", "ink>", "\n\n", "</think>", "\n\nNOOK", "_QA_OK"] {
            shown.push_str(&f.push(d));
        }
        shown.push_str(&f.flush());
        assert_eq!(shown, "NOOK_QA_OK");

        let mut g = EnvelopeFilter::new();
        let mut thought = String::new();
        for d in [
            "<think>",
            "\nOkay, one word",
            "\n</think>",
            "\n\nNOOK_QA_OK",
        ] {
            thought.push_str(&g.push(d));
        }
        assert_eq!(
            thought, "<think>\nOkay, one word\n</think>\n\nNOOK_QA_OK",
            "real thinking streams through as before"
        );

        let mut h = EnvelopeFilter::new();
        assert_eq!(h.push("  Plain"), "Plain");
        assert_eq!(h.push(" answer"), " answer");

        let mut cut = EnvelopeFilter::new();
        assert_eq!(cut.push("<think>"), "");
        assert_eq!(
            cut.flush(),
            "<think>",
            "a stream that ends inside the envelope shows what it had"
        );
    }

    #[test]
    fn the_envelope_and_the_budget_failure() {
        assert_eq!(
            stripped("<think>\nthe user wants one word\n</think>\n\nNOOK_QA_OK"),
            "NOOK_QA_OK"
        );
        assert_eq!(
            stripped("<think>\n\n</think>\n\nNOOK_QA_OK"),
            "NOOK_QA_OK",
            "the empty envelope enable_thinking=false leaves"
        );
        assert_eq!(stripped("plain"), "plain");
        assert_eq!(
            stripped("<think>still going"),
            "<think>still going",
            "an unfinished block is not an answer and is not hidden"
        );

        let ran_out: Value = serde_json::from_str(
            r#"{"choices":[{"finish_reason":"length","message":{"content":"<think>\nOkay, the user"}}]}"#,
        )
        .unwrap();
        assert!(ran_out_while_thinking(&ran_out));
        let answered: Value = serde_json::from_str(
            r#"{"choices":[{"finish_reason":"stop","message":{"content":"<think>\nok\n</think>\nNOOK_QA_OK"}}]}"#,
        )
        .unwrap();
        assert!(!ran_out_while_thinking(&answered));
        let cut: Value = serde_json::from_str(
            r#"{"choices":[{"finish_reason":"length","message":{"content":"<think>\nok\n</think>\nA long answer that was cut"}}]}"#,
        )
        .unwrap();
        assert!(
            !ran_out_while_thinking(&cut),
            "cut while answering is a different failure"
        );
        assert!(!ran_out_while_thinking(&Value::Null));
    }
}

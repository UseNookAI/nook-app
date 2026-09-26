//! Ports `flow/Translator.java`: translates the lines of a transcript with a local chat model.
//! Lines go to the model in numbered batches and come back numbered, so each keeps its place (and
//! its subtitle timing); a batch that comes back with a number missing is split in two and asked
//! again, down to one line.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use async_trait::async_trait;
use once_cell::sync::Lazy;
use regex::Regex;
use tokio_util::sync::CancellationToken;

use super::Stopped;

/// A batch stops growing at this many characters or lines: small enough for a 4B model to keep
/// every number.
pub const MAX_BATCH_CHARS: usize = 1200;
pub const MAX_BATCH_LINES: usize = 24;

/// The model's part: what it is given and what it returns.
#[async_trait]
pub trait Chat: Send + Sync {
    /// The reply to `user`, with any thinking removed.
    async fn reply(&self, user: &str) -> Result<String>;
}

/// The instructions for one run: from `source` (a name, or None when unknown) into `target`.
pub fn system(source: Option<&str>, target: &str) -> String {
    let from = source
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("the language they are in");
    format!(
        "You are a professional translator. Translate each numbered line from {from} into {target}.\n\
         Reply with the same numbered lines in the same order, one per line, and nothing else: no notes, no headings.\n\
         Keep the meaning, tone, names and numbers; keep a line short if it is short; never merge, split, add or drop lines.\n\
         If a line is already in {target}, return it as it is."
    )
}

/// The lines as the model sees them: "1. …", "2. …".
pub fn numbered(lines: &[String]) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{}. {}", i + 1, l.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

static NUMBERED: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^\s*(\d{1,4})\s*[.):\-]\s*(.*)$").expect("the numbered-line pattern")
});

/// The numbered lines of a reply, by number; an unnumbered line continues the one before it.
/// None when any of 1..=`count` is missing or empty, or the reply numbers past `count`.
pub fn parse(reply: &str, count: usize) -> Option<BTreeMap<usize, String>> {
    let mut out: BTreeMap<usize, String> = BTreeMap::new();
    let mut current = 0usize;
    for raw in reply.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(m) = NUMBERED.captures(line) {
            current = m[1].parse().ok()?;
            if current < 1 || current > count {
                return None;
            }
            append(&mut out, current, m[2].trim());
        } else if current > 0 {
            append(&mut out, current, line);
        }
    }
    (1..=count)
        .all(|i| out.get(&i).is_some_and(|t| !t.trim().is_empty()))
        .then_some(out)
}

fn append(out: &mut BTreeMap<usize, String>, n: usize, text: &str) {
    match out.get_mut(&n) {
        Some(t) if !t.is_empty() => {
            t.push(' ');
            t.push_str(text);
        }
        Some(t) => t.push_str(text),
        None => {
            out.insert(n, text.to_string());
        }
    }
}

/// Splits the line indexes into batches of at most [`MAX_BATCH_LINES`] lines and about
/// [`MAX_BATCH_CHARS`] characters.
pub fn batches(lines: &[String]) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut chars = 0;
    for (i, line) in lines.iter().enumerate() {
        let len = line.chars().count();
        if !cur.is_empty() && (cur.len() >= MAX_BATCH_LINES || chars + len > MAX_BATCH_CHARS) {
            out.push(std::mem::take(&mut cur));
            chars = 0;
        }
        cur.push(i);
        chars += len;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Every line translated, in order. `progress` hears how many lines are done so far; `cancel`
/// stops the work between batches with [`Stopped`].
pub async fn translate(
    lines: &[String],
    chat: &dyn Chat,
    cancel: &CancellationToken,
    progress: &(dyn Fn(usize) + Send + Sync),
) -> Result<Vec<String>> {
    let mut out = vec![String::new(); lines.len()];
    let mut done = 0;
    for batch in batches(lines) {
        if cancel.is_cancelled() {
            return Err(Stopped.into());
        }
        let texts: Vec<String> = batch.iter().map(|&i| lines[i].clone()).collect();
        let translated = translate_batch(&texts, chat, cancel).await?;
        for (&i, t) in batch.iter().zip(translated) {
            out[i] = t;
        }
        done += batch.len();
        progress(done);
    }
    Ok(out)
}

fn translate_batch<'a>(
    texts: &'a [String],
    chat: &'a dyn Chat,
    cancel: &'a CancellationToken,
) -> Pin<Box<dyn Future<Output = Result<Vec<String>>> + Send + 'a>> {
    Box::pin(async move {
        if cancel.is_cancelled() {
            return Err(Stopped.into());
        }
        let reply = chat.reply(&numbered(texts)).await?;
        if let Some(parsed) = parse(&reply, texts.len()) {
            return Ok(parsed.into_values().map(|t| unquote(&t)).collect());
        }
        if texts.len() == 1 {
            // The model answered the one line without its number, or with extra words: take what
            // it said.
            let t = reply.trim();
            let t = NUMBERED
                .captures(t)
                .map(|m| m[2].trim().to_string())
                .unwrap_or_else(|| t.to_string());
            return Ok(vec![if t.trim().is_empty() {
                texts[0].clone()
            } else {
                unquote(&t)
            }]);
        }
        let half = texts.len() / 2;
        let mut out = translate_batch(&texts[..half], chat, cancel).await?;
        out.extend(translate_batch(&texts[half..], chat, cancel).await?);
        Ok(out)
    })
}

/// A line the model wrapped in quotes, unwrapped.
pub fn unquote(s: &str) -> String {
    let t = s.trim();
    for (open, close) in [('"', '"'), ('«', '»'), ('“', '”')] {
        if t.chars().count() >= 2 && t.starts_with(open) && t.ends_with(close) {
            let inner = &t[open.len_utf8()..t.len() - close.len_utf8()];
            return inner.trim().to_string();
        }
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn lines_are_numbered_and_read_back_by_number() {
        assert_eq!(numbered(&strings(&["a ", " b"])), "1. a\n2. b");
        let parsed = parse("1. Eins\n2) Zwei\nund mehr\n\n3: Drei", 3).unwrap();
        assert_eq!(parsed[&1], "Eins");
        assert_eq!(parsed[&2], "Zwei und mehr", "an unnumbered line continues");
        assert_eq!(parsed[&3], "Drei");
        assert!(
            parse("1. Eins\n3. Drei", 3).is_none(),
            "a number is missing"
        );
        assert!(
            parse("1. Eins\n2. Zwei\n3. Drei", 2).is_none(),
            "past the count"
        );
        assert!(parse("1. Eins\n2. ", 2).is_none(), "an empty line");
        assert!(
            parse("Here you go:\n1. Eins", 1).is_some(),
            "a preamble is ignored"
        );
    }

    #[test]
    fn batches_stop_at_the_line_and_character_limits() {
        let many: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        let b = batches(&many);
        assert_eq!(b.len(), 3);
        assert_eq!(b[0].len(), MAX_BATCH_LINES);
        assert_eq!(b[2], (48..50).collect::<Vec<_>>());
        let long = vec!["x".repeat(700), "y".repeat(700), "z".repeat(10)];
        assert_eq!(batches(&long), vec![vec![0], vec![1, 2]]);
        assert!(batches(&[]).is_empty());
    }

    #[test]
    fn quotes_come_off() {
        assert_eq!(unquote(" \"Hallo\" "), "Hallo");
        assert_eq!(unquote("«Salut»"), "Salut");
        assert_eq!(unquote("\""), "\"");
        assert_eq!(unquote("say \"hi\""), "say \"hi\"");
    }

    /// Answers each batch by upper-casing its lines, except that it drops a line from any batch
    /// longer than `drops_above`; remembers how many lines each request had.
    struct Scripted {
        drops_above: usize,
        asked: Mutex<Vec<usize>>,
    }

    #[async_trait]
    impl Chat for Scripted {
        async fn reply(&self, user: &str) -> Result<String> {
            let lines: Vec<&str> = user.lines().collect();
            self.asked.lock().push(lines.len());
            let mut out: Vec<String> = lines
                .iter()
                .map(|l| {
                    let m = NUMBERED.captures(l).unwrap();
                    format!("{}. \"{}\"", &m[1], m[2].to_uppercase())
                })
                .collect();
            if lines.len() > self.drops_above {
                out.remove(1);
            }
            if lines.len() == 1 {
                // a lone line comes back without its number
                return Ok(out[0].split_once(". ").map(|(_, t)| t.to_string()).unwrap());
            }
            Ok(out.join("\n"))
        }
    }

    #[tokio::test]
    async fn a_malformed_batch_is_split_and_asked_again() {
        let lines = strings(&["one", "two", "three", "four"]);
        let chat = Scripted {
            drops_above: 2,
            asked: Mutex::new(Vec::new()),
        };
        let seen = Mutex::new(Vec::new());
        let out = translate(&lines, &chat, &CancellationToken::new(), &|n| {
            seen.lock().push(n)
        })
        .await
        .unwrap();
        assert_eq!(out, strings(&["ONE", "TWO", "THREE", "FOUR"]));
        assert_eq!(*chat.asked.lock(), vec![4, 2, 2], "four, then two halves");
        assert_eq!(*seen.lock(), vec![4]);

        let single = Scripted {
            drops_above: 1,
            asked: Mutex::new(Vec::new()),
        };
        let out = translate(
            &strings(&["a", "b"]),
            &single,
            &CancellationToken::new(),
            &|_| {},
        )
        .await
        .unwrap();
        assert_eq!(
            out,
            strings(&["A", "B"]),
            "down to one line, taken as it came"
        );
    }

    #[tokio::test]
    async fn a_stopped_run_asks_nothing() {
        let chat = Scripted {
            drops_above: 99,
            asked: Mutex::new(Vec::new()),
        };
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = translate(&strings(&["a"]), &chat, &cancel, &|_| {})
            .await
            .unwrap_err();
        assert!(err.is::<Stopped>());
        assert!(chat.asked.lock().is_empty());
    }
}

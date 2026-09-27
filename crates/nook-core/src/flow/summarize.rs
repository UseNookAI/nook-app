//! Summaries by the chat model, for the Summarize Nooklet (a document) and the Transcribe
//! Nooklet's notes (what was said in a recording). A text too long for the model in one go is cut
//! into parts at paragraph ends; the key points of each part are asked for, then one summary of
//! them all, in the shape the person chose.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::translator::Chat;
use super::Stopped;

/// The most a part may hold, in the model's tokens (estimated): with the instructions and a reply
/// of up to 2048 tokens it fits the 8192 tokens every chat model in the catalog has.
pub const PART_TOKENS: usize = 2800;

/// How long a summary is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Length {
    /// A paragraph and the key points.
    #[default]
    Short,
    /// A section for each main part.
    Detailed,
}

/// What is summarized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Document,
    /// The transcript of a recording: a meeting, a call, a lecture, an interview.
    Talk,
}

/// What the summary is to be.
///
/// - `language`: the name of the language to write in, or None for the text's own
/// - `focus`: what the person wants to know most ("the risks for me as a tenant")
/// - `title`: the file's name, a hint to what the text is
#[derive(Clone, Debug)]
pub struct Brief {
    pub kind: Kind,
    pub length: Length,
    pub language: Option<String>,
    pub focus: Option<String>,
    pub title: String,
}

/// Roughly how many tokens `text` is: a token for about every four letters of a script with
/// spaces, one for each character of Chinese, Japanese, Korean or Thai.
pub fn tokens(text: &str) -> usize {
    let mut dense = 0usize;
    let mut other = 0usize;
    for c in text.chars() {
        let u = c as u32;
        let cjk = (0x2E80..=0x9FFF).contains(&u)
            || (0xAC00..=0xD7AF).contains(&u)
            || (0xF900..=0xFAFF).contains(&u)
            || (0x0E00..=0x0E7F).contains(&u);
        if cjk {
            dense += 1;
        } else {
            other += 1;
        }
    }
    dense + other.div_ceil(4)
}

/// `text` in parts of at most `max_tokens`, cut at paragraph ends, else at sentence ends, else
/// between words.
pub fn parts(text: &str, max_tokens: usize) -> Vec<String> {
    let mut pieces: Vec<String> = Vec::new();
    for paragraph in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        if tokens(paragraph) <= max_tokens {
            pieces.push(paragraph.to_string());
            continue;
        }
        pieces.extend(cut_long(paragraph, max_tokens));
    }
    let mut out: Vec<String> = Vec::new();
    let mut now = String::new();
    for p in pieces {
        if !now.is_empty() && tokens(&now) + tokens(&p) + 1 > max_tokens {
            out.push(std::mem::take(&mut now));
        }
        if !now.is_empty() {
            now.push_str("\n\n");
        }
        now.push_str(&p);
    }
    if !now.trim().is_empty() {
        out.push(now);
    }
    out
}

/// A paragraph too long for a part, at sentence ends or between words.
fn cut_long(paragraph: &str, max_tokens: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut now = String::new();
    let units: Vec<&str> = split_keep(paragraph, &['.', '!', '?', '。', '！', '？']);
    for unit in units {
        let pieces: Vec<String> = if tokens(unit) > max_tokens {
            // A sentence with no end in sight: between words, else by characters.
            let mut words = Vec::new();
            let mut w = String::new();
            for c in unit.chars() {
                w.push(c);
                if (c.is_whitespace() && tokens(&w) > max_tokens / 2) || tokens(&w) >= max_tokens {
                    words.push(std::mem::take(&mut w));
                }
            }
            if !w.is_empty() {
                words.push(w);
            }
            words
        } else {
            vec![unit.to_string()]
        };
        for p in pieces {
            if !now.is_empty() && tokens(&now) + tokens(&p) > max_tokens {
                out.push(std::mem::take(&mut now));
            }
            now.push_str(&p);
        }
    }
    if !now.trim().is_empty() {
        out.push(now);
    }
    out.into_iter().map(|s| s.trim().to_string()).collect()
}

/// `text` split after each of `ends`, the ends kept.
fn split_keep<'a>(text: &'a str, ends: &[char]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if ends.contains(&c) {
            let end = i + c.len_utf8();
            out.push(&text[start..end]);
            start = end;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// What the model is told once, for every question of a summary.
pub fn system(brief: &Brief) -> String {
    let who = match brief.kind {
        Kind::Document => {
            "You write summaries of documents for someone who has no time to read them."
        }
        Kind::Talk => {
            "You write notes from the transcript of a recording (a meeting, a call, a lecture, an \
             interview) for someone who was not there. The transcript was made by a machine, so \
             correct a word that was plainly misheard, and only then."
        }
    };
    let language = match &brief.language {
        Some(name) => format!("Write in {name}."),
        None => "Write in the language the text is in.".into(),
    };
    format!(
        "{who} Stay faithful to the text: say only what it says, and keep names, numbers, dates, \
         amounts and deadlines exactly as written. Use Markdown. {language}"
    )
}

fn focus_line(brief: &Brief) -> String {
    match brief.focus.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
        Some(f) => format!(
            "\nThe reader most wants to know about: {f}. Give that the most room, and say so plainly when the text does not cover it."
        ),
        None => String::new(),
    }
}

fn what(brief: &Brief) -> &'static str {
    match brief.kind {
        Kind::Document => "the document",
        Kind::Talk => "the transcript",
    }
}

/// The question for one part's key points.
pub fn points_prompt(brief: &Brief, index: usize, count: usize, part: &str) -> String {
    let named = if brief.title.is_empty() {
        String::new()
    } else {
        format!(" ({})", brief.title)
    };
    let keep = match brief.kind {
        Kind::Document => "name, number, date, amount, deadline and obligation",
        Kind::Talk => "name, number, date, decision, task and question",
    };
    format!(
        "Here is part {} of {count} of {}{named}. List its key points as Markdown bullets, at most 12, keeping every {keep} it mentions. Write only the bullets.{}\n\n<<<\n{part}\n>>>",
        index + 1,
        what(brief),
        focus_line(brief)
    )
}

/// The question that merges the key points of many parts into fewer.
fn merge_prompt(brief: &Brief, points: &str) -> String {
    format!(
        "Here are key points from several parts of {}, in order. Merge them into one list of Markdown bullets, at most 20, dropping repeats and keeping every name, number, date and amount. Write only the bullets.{}\n\n<<<\n{points}\n>>>",
        what(brief),
        focus_line(brief)
    )
}

/// The shape of the finished summary.
fn shape(brief: &Brief) -> &'static str {
    match (brief.kind, brief.length) {
        (Kind::Document, Length::Short) => {
            "Write the summary in this shape:\n\
             # A title that says what the document is\n\
             One short paragraph: what it is, who it is from or for, and what it says or asks.\n\
             ## Key points\nAt most 7 bullets.\n\
             ## Worth checking\nDeadlines, amounts, obligations, risks or anything unusual, as bullets. Leave this section out when there is none."
        }
        (Kind::Document, Length::Detailed) => {
            "Write the summary in this shape:\n\
             # A title that says what the document is\n\
             One paragraph: what it is, who it is from or for, and what it says or asks.\n\
             Then a ## section for each main part of the document, with its points as bullets or short paragraphs.\n\
             ## Worth checking\nDeadlines, amounts, obligations, risks or anything unusual, as bullets. Leave this section out when there is none."
        }
        (Kind::Talk, Length::Short) => {
            "Write the notes in this shape:\n\
             # A title that says what the recording is about\n\
             One short paragraph: who spoke, if the transcript tells, and what it was about.\n\
             ## Key points\nAt most 7 bullets.\n\
             ## Decisions\n## To do\nEach task as a bullet: who does what, and by when, as far as the recording says.\n\
             ## Open questions\n\
             Leave out a section with nothing in it."
        }
        (Kind::Talk, Length::Detailed) => {
            "Write the notes in this shape:\n\
             # A title that says what the recording is about\n\
             One paragraph: who spoke, if the transcript tells, and what it was about.\n\
             Then a ## section for each topic, in the order they came up, with what was said as bullets.\n\
             ## Decisions\n## To do\nEach task as a bullet: who does what, and by when, as far as the recording says.\n\
             ## Open questions\n\
             Leave out a section with nothing in it."
        }
    }
}

/// The question for the finished summary, of the whole text or of its parts' key points.
pub fn final_prompt(brief: &Brief, body: &str, from_points: bool) -> String {
    let of = if from_points {
        format!("the key points of each part of {}, in order", what(brief))
    } else {
        what(brief).to_string()
    };
    let named = if brief.title.is_empty() {
        String::new()
    } else {
        format!(" (the file is called \"{}\")", brief.title)
    };
    format!(
        "{}{}\n\nHere is {of}{named}:\n\n<<<\n{body}\n>>>",
        shape(brief),
        focus_line(brief)
    )
}

/// The reply without a code fence around it or talk before the Markdown.
pub fn tidy(reply: &str) -> String {
    let mut s = reply.trim();
    if let Some(rest) = s.strip_prefix("```") {
        let rest = rest.trim_start_matches(|c: char| c.is_alphanumeric());
        s = rest.strip_suffix("```").unwrap_or(rest).trim();
    }
    // "Here is the summary:" before the first heading goes.
    if let Some(at) = s.find("\n# ").filter(|_| !s.starts_with('#')) {
        let before = &s[..at];
        if before.lines().count() <= 2 && before.trim_end().ends_with(':') {
            s = s[at..].trim_start();
        }
    }
    s.to_string()
}

/// Summarizes `text` as `brief` asks, one question at a time; `progress` hears (questions
/// asked, questions in all).
pub async fn summarize(
    text: &str,
    brief: &Brief,
    chat: &dyn Chat,
    cancel: &CancellationToken,
    progress: &(dyn Fn(usize, usize) + Send + Sync),
) -> Result<String> {
    let text = text.trim();
    if text.is_empty() {
        anyhow::bail!("There are no words to summarize.");
    }
    let pieces = parts(text, PART_TOKENS);
    if pieces.len() <= 1 {
        progress(0, 1);
        let reply = ask(chat, &final_prompt(brief, text, false), cancel).await?;
        progress(1, 1);
        return Ok(tidy(&reply));
    }
    // Each part, then (when the points are still too long) merges, then the summary.
    let mut total = pieces.len() + 1;
    let mut asked = 0;
    progress(asked, total);
    let mut points = Vec::new();
    for (i, part) in pieces.iter().enumerate() {
        let reply = ask(chat, &points_prompt(brief, i, pieces.len(), part), cancel).await?;
        points.push(tidy(&reply));
        asked += 1;
        progress(asked, total);
    }
    let mut joined = points.join("\n\n");
    while tokens(&joined) > PART_TOKENS * 3 / 2 {
        let groups = parts(&joined, PART_TOKENS);
        if groups.len() <= 1 {
            break;
        }
        total += groups.len();
        let mut merged = Vec::new();
        for g in &groups {
            merged.push(tidy(&ask(chat, &merge_prompt(brief, g), cancel).await?));
            asked += 1;
            progress(asked, total);
        }
        let next = merged.join("\n\n");
        if tokens(&next) >= tokens(&joined) {
            joined = next;
            break;
        }
        joined = next;
    }
    let reply = ask(chat, &final_prompt(brief, &joined, true), cancel).await?;
    progress(total, total);
    Ok(tidy(&reply))
}

async fn ask(chat: &dyn Chat, prompt: &str, cancel: &CancellationToken) -> Result<String> {
    if cancel.is_cancelled() {
        return Err(Stopped.into());
    }
    let reply = tokio::select! {
        r = chat.reply(prompt) => r?,
        _ = cancel.cancelled() => return Err(Stopped.into()),
    };
    if reply.trim().is_empty() {
        anyhow::bail!("The chat model gave an empty answer.");
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use parking_lot::Mutex;

    fn brief(kind: Kind, length: Length) -> Brief {
        Brief {
            kind,
            length,
            language: None,
            focus: None,
            title: "lease.pdf".into(),
        }
    }

    #[test]
    fn tokens_are_counted_by_script() {
        assert_eq!(tokens("abcd efgh"), 3);
        assert_eq!(tokens("日本語"), 3);
        assert_eq!(tokens(""), 0);
    }

    #[test]
    fn a_long_text_is_cut_at_paragraph_ends() {
        let para = "word ".repeat(400); // 500 tokens
        let text = vec![para.trim(); 13].join("\n\n");
        let parts = parts(&text, PART_TOKENS);
        assert_eq!(parts.len(), 3, "5 paragraphs of 500 tokens fit in 2800");
        assert!(parts.iter().all(|p| tokens(p) <= PART_TOKENS));
        assert_eq!(
            parts.iter().map(|p| p.split("\n\n").count()).sum::<usize>(),
            13
        );
        // One paragraph far too long is cut at its sentences.
        let long = "This is a sentence of some length here. ".repeat(600);
        let cut = super::parts(&long, PART_TOKENS);
        assert!(cut.len() >= 2, "{}", cut.len());
        assert!(cut
            .iter()
            .all(|p| tokens(p) <= PART_TOKENS && p.ends_with('.')));
        // Words with no sentence end at all still fit.
        let run_on = "x".repeat(40_000);
        assert!(super::parts(&run_on, 1000)
            .iter()
            .all(|p| tokens(p) <= 1000));
    }

    #[test]
    fn the_prompts_carry_the_language_the_focus_and_the_shape() {
        let mut b = brief(Kind::Document, Length::Short);
        assert!(system(&b).contains("language the text is in"));
        b.language = Some("German".into());
        b.focus = Some("the notice period".into());
        assert!(system(&b).contains("Write in German."));
        let f = final_prompt(&b, "TEXT", false);
        assert!(
            f.contains("## Worth checking")
                && f.contains("the notice period")
                && f.contains("TEXT")
        );
        assert!(f.contains("lease.pdf"));
        let talk = final_prompt(&brief(Kind::Talk, Length::Detailed), "T", true);
        assert!(
            talk.contains("## To do")
                && talk.contains("each topic")
                && talk.contains("key points of each part")
        );
        assert!(points_prompt(&b, 1, 4, "P").contains("part 2 of 4"));
    }

    #[test]
    fn fences_and_preambles_come_off() {
        assert_eq!(tidy("```markdown\n# A\n- b\n```"), "# A\n- b");
        assert_eq!(tidy("Here is the summary:\n# A\ntext"), "# A\ntext");
        assert_eq!(
            tidy("# A\n\nSo: # not a heading"),
            "# A\n\nSo: # not a heading"
        );
    }

    /// Answers each question with its first line, and remembers them.
    struct Echo {
        asked: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Chat for Echo {
        async fn reply(&self, user: &str) -> Result<String> {
            self.asked.lock().push(user.to_string());
            Ok(format!("- {}", user.lines().next().unwrap_or("")))
        }
    }

    #[tokio::test]
    async fn a_short_text_is_one_question_a_long_one_is_asked_in_parts() {
        let chat = Echo {
            asked: Mutex::new(Vec::new()),
        };
        let cancel = CancellationToken::new();
        let seen = Mutex::new(Vec::new());
        let b = brief(Kind::Document, Length::Short);
        summarize("A short lease.", &b, &chat, &cancel, &|d, t| {
            seen.lock().push((d, t))
        })
        .await
        .unwrap();
        assert_eq!(chat.asked.lock().len(), 1);
        assert_eq!(*seen.lock(), vec![(0, 1), (1, 1)]);

        chat.asked.lock().clear();
        let para = "word ".repeat(400);
        let long = vec![para.trim(); 13].join("\n\n");
        summarize(&long, &b, &chat, &cancel, &|_, _| {})
            .await
            .unwrap();
        let asked = chat.asked.lock();
        assert_eq!(asked.len(), 4, "three parts and the summary");
        assert!(asked[0].starts_with("Here is part 1 of 3"));
        assert!(asked[3].contains("key points of each part"));
    }

    #[tokio::test]
    async fn a_stopped_summary_asks_nothing() {
        let chat = Echo {
            asked: Mutex::new(Vec::new()),
        };
        let cancel = CancellationToken::new();
        cancel.cancel();
        let b = brief(Kind::Talk, Length::Short);
        let e = summarize("Hello.", &b, &chat, &cancel, &|_, _| {})
            .await
            .unwrap_err();
        assert!(e.is::<Stopped>());
        assert!(chat.asked.lock().is_empty());
    }
}

//! The words of a document as the Read aloud Nooklet speaks them: Markdown's marks taken off, code
//! and pictures left out, and the text cut into lines of a sentence or a few, each short enough
//! for a voice to speak in one breath. Also which language a text is in, so the right voice reads
//! it.

use once_cell::sync::Lazy;
use regex::Regex;

use super::languages;

/// A line is filled with sentences up to about this many characters.
pub const LINE_CHARS: usize = 240;
/// A sentence longer than this is cut at its commas.
pub const LONGEST: usize = 300;

static IMAGE: Lazy<Regex> = Lazy::new(|| Regex::new(r"!\[[^\]]*\]\([^)]*\)(\{[^}]*\})?").unwrap());
static LINK: Lazy<Regex> = Lazy::new(|| Regex::new(r"\[([^\]]*)\]\([^)]*\)(\{[^}]*\})?").unwrap());
static ATTRS: Lazy<Regex> = Lazy::new(|| Regex::new(r"\{[#.][^}]*\}").unwrap());
static FOOTNOTE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\[\^[^\]]*\]").unwrap());
static TAG: Lazy<Regex> = Lazy::new(|| Regex::new(r"</?[A-Za-z][^>]*>").unwrap());
static URL: Lazy<Regex> = Lazy::new(|| Regex::new(r"<?https?://\S+>?").unwrap());
static MARKS: Lazy<Regex> = Lazy::new(|| Regex::new(r"(\*\*|__|\*|`|~~)").unwrap());
static BULLET: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*(?:[-*+]|\d{1,3}[.)])\s+").unwrap());

/// One block of the document to speak: a heading or a paragraph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub text: String,
    pub heading: bool,
}

/// The document's blocks, the Markdown marks off: headings, paragraphs, list items and table rows
/// (their cells joined with commas); code and pictures are left out.
pub fn blocks(markdown: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut in_code = false;
    let flush = |paragraph: &mut Vec<String>, out: &mut Vec<Block>| {
        let text = paragraph.join(" ");
        let text = clean(&text);
        if speakable(&text) {
            out.push(Block {
                text,
                heading: false,
            });
        }
        paragraph.clear();
    };
    for raw in markdown.lines() {
        let line = raw.trim_end();
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            flush(&mut paragraph, &mut out);
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        if t.is_empty() || t.starts_with("<!--") {
            flush(&mut paragraph, &mut out);
            continue;
        }
        if let Some(h) = t.strip_prefix('#') {
            flush(&mut paragraph, &mut out);
            let text = clean(h.trim_start_matches('#'));
            if speakable(&text) {
                out.push(Block {
                    text,
                    heading: true,
                });
            }
            continue;
        }
        if t.starts_with('|') {
            flush(&mut paragraph, &mut out);
            let cells: Vec<String> = t
                .trim_matches('|')
                .split('|')
                .map(clean)
                .filter(|c| !c.is_empty())
                .collect();
            let rule = cells
                .iter()
                .all(|c| c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')));
            if !rule && !cells.is_empty() {
                out.push(Block {
                    text: cells.join(", "),
                    heading: false,
                });
            }
            continue;
        }
        if t.chars().all(|c| matches!(c, '-' | '*' | '_' | '=' | ' ')) {
            // A rule, or the underline of a heading.
            flush(&mut paragraph, &mut out);
            continue;
        }
        if BULLET.is_match(t) {
            flush(&mut paragraph, &mut out);
            paragraph.push(BULLET.replace(t, "").to_string());
            continue;
        }
        let t = t.trim_start_matches('>').trim();
        paragraph.push(t.trim_end_matches('\\').to_string());
    }
    flush(&mut paragraph, &mut out);
    out
}

/// A piece of text without Markdown's marks, links as their words, pictures and addresses gone.
fn clean(text: &str) -> String {
    let t = IMAGE.replace_all(text, "");
    let t = LINK.replace_all(&t, "$1");
    let t = FOOTNOTE.replace_all(&t, "");
    let t = ATTRS.replace_all(&t, "");
    let t = TAG.replace_all(&t, "");
    let t = URL.replace_all(&t, "");
    let t = MARKS.replace_all(&t, "");
    let t = t.replace("\\", "");
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether a piece of text has anything a voice can say.
fn speakable(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

/// Where sentences end: after . ! ? … followed by a space, or after the full stops of Chinese
/// and Japanese.
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut now = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        now.push(c);
        let next = chars.get(i + 1).copied();
        let end = match c {
            '。' | '！' | '？' => true,
            '.' | '!' | '?' | '…' => next.is_none_or(char::is_whitespace),
            _ => false,
        };
        if end {
            let s = now.trim().to_string();
            if !s.is_empty() {
                out.push(s);
            }
            now.clear();
        }
    }
    let s = now.trim().to_string();
    if !s.is_empty() {
        out.push(s);
    }
    out
}

/// A sentence too long for one breath, at commas, semicolons and dashes, else between words.
fn cut_sentence(s: &str) -> Vec<String> {
    if s.chars().count() <= LONGEST {
        return vec![s.to_string()];
    }
    let mut pieces = Vec::new();
    let mut now = String::new();
    for c in s.chars() {
        now.push(c);
        let pause = matches!(c, ',' | ';' | ':' | '，' | '、' | '；' | '–' | '—');
        let long = now.chars().count();
        if (pause && long >= LINE_CHARS / 2)
            || (c.is_whitespace() && long >= LINE_CHARS)
            || long >= LONGEST
        {
            pieces.push(now.trim().to_string());
            now.clear();
        }
    }
    if !now.trim().is_empty() {
        pieces.push(now.trim().to_string());
    }
    pieces.retain(|p| !p.is_empty());
    pieces
}

/// The lines to speak, in order: each heading alone, each paragraph's sentences joined up to
/// [`LINE_CHARS`].
pub fn lines(markdown: &str) -> Vec<String> {
    let mut out = Vec::new();
    for b in blocks(markdown) {
        if b.heading {
            out.push(b.text);
            continue;
        }
        let mut now = String::new();
        for s in sentences(&b.text).iter().flat_map(|s| cut_sentence(s)) {
            if !now.is_empty() && now.chars().count() + 1 + s.chars().count() > LINE_CHARS {
                out.push(std::mem::take(&mut now));
            }
            if !now.is_empty() {
                now.push(' ');
            }
            now.push_str(&s);
        }
        if speakable(&now) {
            out.push(now);
        }
    }
    out
}

/// The words in a text, for the card ("2,400 words").
pub fn word_count(text: &str) -> u32 {
    let dense = text
        .chars()
        .filter(|c| {
            let u = *c as u32;
            (0x3040..=0x9FFF).contains(&u) || (0xAC00..=0xD7AF).contains(&u)
        })
        .count();
    // A Chinese or Japanese word is about two characters.
    (text.split_whitespace().filter(|w| speakable(w)).count() + dense / 2) as u32
}

/// The language `text` is written in, as one of the flows' codes, when it can be told.
pub fn detect_language(text: &str) -> Option<String> {
    let sample: String = text.chars().take(4000).collect();
    let info = whatlang::detect(&sample)?;
    if info.confidence() < 0.3 {
        return None;
    }
    let code = match info.lang().code() {
        "eng" => "en",
        "spa" => "es",
        "fra" => "fr",
        "deu" => "de",
        "ita" => "it",
        "por" => "pt",
        "nld" => "nl",
        "swe" => "sv",
        "dan" => "da",
        "nob" | "nno" => "no",
        "fin" => "fi",
        "pol" => "pl",
        "ces" => "cs",
        "slk" => "sk",
        "hun" => "hu",
        "ron" => "ro",
        "bul" => "bg",
        "hrv" => "hr",
        "srp" => "sr",
        "slv" => "sl",
        "ukr" => "uk",
        "rus" => "ru",
        "ell" => "el",
        "tur" => "tr",
        "ara" => "ar",
        "heb" => "he",
        "pes" => "fa",
        "urd" => "ur",
        "hin" => "hi",
        "ben" => "bn",
        "tam" => "ta",
        "tel" => "te",
        "mal" => "ml",
        "ind" => "id",
        "vie" => "vi",
        "tha" => "th",
        "cmn" => "zh",
        "jpn" => "ja",
        "kor" => "ko",
        "cat" => "ca",
        "est" => "et",
        "lav" => "lv",
        "lit" => "lt",
        "tgl" => "tl",
        _ => return None,
    };
    languages::by_code(code).map(|l| l.code.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_marks_come_off_and_code_and_pictures_stay_silent() {
        let md = "# The **Lease**\n\nThe tenant pays [rent](https://x.y) of *€900*.\nIt is due monthly.\n\n![plan](plan.png)\n\n```\nlet x = 1;\n```\n\n- First item\n- Second `item`\n\n| Name | Rent |\n|---|---:|\n| Flat 2 | €900 |\n\n---\n\nSee https://example.com for more.[^1]";
        let b = blocks(md);
        let texts: Vec<&str> = b.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "The Lease",
                "The tenant pays rent of €900. It is due monthly.",
                "First item",
                "Second item",
                "Name, Rent",
                "Flat 2, €900",
                "See for more.",
            ]
        );
        assert!(b[0].heading && !b[1].heading);
    }

    #[test]
    fn paragraphs_become_lines_of_whole_sentences() {
        let sentence = "This sentence is exactly fifty characters long ok.";
        let para = [sentence; 10].join(" ");
        let lines = lines(&format!("## Part one\n\n{para}"));
        assert_eq!(lines[0], "Part one");
        assert!(lines[1..]
            .iter()
            .all(|l| l.chars().count() <= LINE_CHARS && l.ends_with('.')));
        assert_eq!(
            lines[1..]
                .iter()
                .map(|l| l.matches("ok.").count())
                .sum::<usize>(),
            10
        );
        // A sentence with no end is cut at its commas.
        let long = vec!["a clause that goes on and on"; 20].join(", ");
        let cut = super::lines(&long);
        assert!(cut.len() > 1 && cut.iter().all(|l| l.chars().count() <= LONGEST));
        // Japanese sentences end at 。 without spaces.
        assert_eq!(
            sentences("今日は晴れです。明日は雨です。"),
            vec!["今日は晴れです。", "明日は雨です。"]
        );
        // "3.5" and "e.g." do not end a sentence unless a space follows.
        assert_eq!(
            sentences("It costs 3.5 euros. Fine"),
            vec!["It costs 3.5 euros.", "Fine"]
        );
    }

    #[test]
    fn the_language_is_told_from_the_text() {
        assert_eq!(
            detect_language("The tenant shall pay the rent on the first day of each month, and the landlord shall keep the building in good repair.").as_deref(),
            Some("en")
        );
        assert_eq!(
            detect_language("Der Mieter zahlt die Miete am ersten Tag jedes Monats, und der Vermieter hält das Gebäude in gutem Zustand.").as_deref(),
            Some("de")
        );
        assert_eq!(
            detect_language(
                "Kiracı kirayı her ayın ilk günü öder ve ev sahibi binayı iyi durumda tutar."
            )
            .as_deref(),
            Some("tr")
        );
        assert_eq!(detect_language("12 34 !!").as_deref(), None);
        assert_eq!(word_count("Hello there, world."), 3);
    }
}

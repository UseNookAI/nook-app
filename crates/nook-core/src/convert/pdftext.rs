//! A PDF's text as Markdown or plain text. Its lines (in the order the PDF draws them, as
//! PDFium reads them out, which is the reading order of most documents, columns included) become
//! paragraphs where they sit close under each other in one size and each line but the last runs
//! to the column's right edge, headings where they are larger than the body text, list items
//! where they start with a bullet or a number, and bold where a short line is; a word broken over
//! two lines is joined, and a page's number at its head or foot is left out.

/// A line of a page: its text, size and weight, and where it is (points, y up).
#[derive(Clone, Debug, PartialEq)]
pub struct TextLine {
    pub text: String,
    pub size: f32,
    pub bold: bool,
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageText {
    pub lines: Vec<TextLine>,
}

/// The size most of the text is in (rounded to half a point), by characters.
fn body_size(pages: &[PageText]) -> f32 {
    let mut counts: Vec<(i32, usize)> = Vec::new();
    for l in pages.iter().flat_map(|p| &p.lines) {
        let k = (l.size * 2.0).round() as i32;
        let n = l.text.chars().filter(|c| !c.is_whitespace()).count();
        match counts.iter_mut().find(|(s, _)| *s == k) {
            Some((_, c)) => *c += n,
            None => counts.push((k, n)),
        }
    }
    counts
        .into_iter()
        .max_by_key(|&(_, n)| n)
        .map_or(11.0, |(k, _)| k as f32 / 2.0)
}

fn is_page_number(t: &str) -> bool {
    let t = t.trim().to_lowercase();
    let t = t
        .strip_prefix("page ")
        .or_else(|| t.strip_prefix("- "))
        .unwrap_or(&t)
        .trim_end_matches(" -")
        .trim();
    let mut parts = t
        .split(|c: char| c == '/' || c.is_whitespace())
        .filter(|p| !p.is_empty() && *p != "of");
    let first = parts.next().unwrap_or("");
    !first.is_empty()
        && first.chars().all(|c| c.is_ascii_digit())
        && first.len() <= 4
        && parts.all(|p| p.chars().all(|c| c.is_ascii_digit()))
}

/// A list item's marker and its text: "- " for a bullet, "1. " for a number.
fn list_item(t: &str) -> Option<(String, &str)> {
    let t = t.trim_start();
    let mut chars = t.chars();
    let first = chars.next()?;
    if "•·◦▪‣●○■□–-*".contains(first) {
        let rest = chars.as_str();
        if rest.starts_with(char::is_whitespace) {
            return Some(("- ".into(), rest.trim_start()));
        }
    }
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if (1..=3).contains(&digits.len()) {
        let rest = &t[digits.len()..];
        if let Some(r) = rest.strip_prefix(['.', ')']) {
            if r.starts_with(char::is_whitespace) {
                return Some((format!("{digits}. "), r.trim_start()));
            }
        }
    }
    None
}

/// Text made safe to read as Markdown: marks that would start emphasis, links, code, headings
/// or lists are escaped.
fn escape(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    for c in t.chars() {
        if "\\*_`[]<>|".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    // A paragraph that begins like a heading, a quote or a numbered item is none of them.
    if out.starts_with('#') || out.starts_with('>') || out.starts_with('+') || out.starts_with('-')
    {
        out.insert(0, '\\');
    }
    let digits = out.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && out[digits..].starts_with(". ") {
        out.insert(digits, '\\');
    }
    out
}

/// Lines joined into a paragraph: a word broken by a hyphen at a line's end is made whole.
fn join(lines: &[&TextLine]) -> String {
    let mut acc = String::new();
    for l in lines {
        let t = l.text.trim();
        if t.is_empty() {
            continue;
        }
        if acc.ends_with('-')
            && acc[..acc.len() - 1].ends_with(char::is_alphabetic)
            && t.starts_with(char::is_lowercase)
        {
            acc.pop();
            acc.push_str(t);
        } else {
            if !acc.is_empty() {
                acc.push(' ');
            }
            acc.push_str(t);
        }
    }
    acc
}

/// The pages as Markdown, or as plain text (`plain`): paragraphs a blank line apart.
pub fn write(pages: &[PageText], plain: bool) -> String {
    let body = body_size(pages);
    // Heading levels by size, the largest first.
    let mut heads: Vec<i32> = pages
        .iter()
        .flat_map(|p| &p.lines)
        .filter(|l| l.size >= body * 1.2 && !l.text.trim().is_empty())
        .map(|l| (l.size * 2.0).round() as i32)
        .collect();
    heads.sort_unstable_by(|a, b| b.cmp(a));
    heads.dedup();
    let level = |size: f32| -> Option<usize> {
        let k = (size * 2.0).round() as i32;
        heads.iter().position(|&h| h == k).map(|i| (i + 1).min(3))
    };

    let mut blocks: Vec<String> = Vec::new();
    for page in pages {
        let mut lines: Vec<&TextLine> = page
            .lines
            .iter()
            .filter(|l| !l.text.trim().is_empty())
            .collect();
        // A page number at the head or the foot.
        if lines.last().is_some_and(|l| is_page_number(&l.text)) {
            lines.pop();
        }
        if lines.first().is_some_and(|l| is_page_number(&l.text)) {
            lines.remove(0);
        }
        // Where a column's lines end: the furthest right of the lines that start where it does.
        let right_edge = |left: f32, size: f32| {
            lines
                .iter()
                .filter(|l| (l.left - left).abs() < size * 2.0)
                .map(|l| l.right)
                .fold(f32::MIN, f32::max)
        };
        let mut para: Vec<&TextLine> = Vec::new();
        let flush = |para: &mut Vec<&TextLine>, blocks: &mut Vec<String>| {
            let Some(first) = para.first() else { return };
            let text = join(para);
            let block = if plain {
                text
            } else if let Some(n) = level(first.size).filter(|_| text.chars().count() <= 200) {
                format!("{} {}", "#".repeat(n), escape(&text))
            } else if let Some((mark, rest)) = list_item(&text) {
                format!("{mark}{}", escape(rest))
            } else if first.bold && para.len() == 1 && text.chars().count() <= 120 {
                format!("**{}**", escape(&text))
            } else {
                escape(&text)
            };
            blocks.push(block);
            para.clear();
        };
        for l in lines.iter().copied() {
            let goes_on = para.last().is_some_and(|prev: &&TextLine| {
                let gap = prev.bottom - l.top;
                (prev.size - l.size).abs() < 0.8
                    && prev.bold == l.bold
                    && gap > -prev.size * 0.5
                    && gap <= prev.size * 0.8
                    && l.left - para[0].left < prev.size * 1.5
                    && prev.right >= right_edge(para[0].left, prev.size) - prev.size * 3.0
                    && list_item(&l.text).is_none()
                    && level(prev.size).is_none()
            });
            if !goes_on {
                flush(&mut para, &mut blocks);
            }
            para.push(l);
        }
        flush(&mut para, &mut blocks);
    }
    let mut out = blocks.join("\n\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, size: f32, top: f32, left: f32) -> TextLine {
        TextLine {
            text: text.into(),
            size,
            bold: false,
            left,
            right: left + text.chars().count() as f32 * size * 0.5,
            top,
            bottom: top - size,
        }
    }

    fn page() -> PageText {
        let mut bold = line("Terms at a glance", 11.0, 560.0, 72.0);
        bold.bold = true;
        PageText {
            lines: vec![
                line("1", 9.0, 780.0, 300.0),
                line("Invoice 2026-0914", 20.0, 740.0, 72.0),
                line(
                    "Payment is due within thirty days of the in-",
                    11.0,
                    700.0,
                    72.0,
                ),
                line(
                    "voice date. Late payments may incur interest",
                    11.0,
                    686.0,
                    72.0,
                ),
                line("at 8% per year.", 11.0, 672.0, 72.0),
                line("Items", 14.0, 640.0, 72.0),
                line("• Website redesign", 11.0, 620.0, 72.0),
                line("• Hosting, 12 months", 11.0, 606.0, 72.0),
                line("2. Support *hours*", 11.0, 592.0, 72.0),
                bold,
                line(
                    "All work remains the property of the supplier",
                    11.0,
                    540.0,
                    72.0,
                ),
                line("until paid in full.", 11.0, 526.0, 72.0),
                line("Page 1 of 3", 9.0, 40.0, 280.0),
            ],
        }
    }

    #[test]
    fn a_page_reads_as_headings_paragraphs_lists_and_bold() {
        let md = write(&[page()], false);
        assert_eq!(
            md,
            "# Invoice 2026-0914\n\n\
             Payment is due within thirty days of the invoice date. Late payments may incur interest at 8% per year.\n\n\
             ## Items\n\n\
             - Website redesign\n\n\
             - Hosting, 12 months\n\n\
             2. Support \\*hours\\*\n\n\
             **Terms at a glance**\n\n\
             All work remains the property of the supplier until paid in full.\n"
        );
        let text = write(&[page()], true);
        assert!(text.starts_with("Invoice 2026-0914\n\nPayment is due"));
        assert!(!text.contains("Page 1 of 3") && !text.contains("\n1\n"));
    }

    #[test]
    fn short_lines_are_lines_of_their_own() {
        // A list whose bullets are drawn, not written, and a table's rows.
        let page = PageText {
            lines: vec![
                line("New customers: 42", 11.0, 700.0, 72.0),
                line("Returning customers: 118", 11.0, 686.0, 72.0),
                line("Region Sales", 11.0, 660.0, 72.0),
                line("North 1,200", 11.0, 646.0, 72.0),
                line(
                    "The totals below are before tax and are rounded to the",
                    11.0,
                    620.0,
                    72.0,
                ),
                line("nearest whole number.", 11.0, 606.0, 72.0),
            ],
        };
        assert_eq!(
            write(&[page], true),
            concat!(
                "New customers: 42

Returning customers: 118

Region Sales

North 1,200

",
                "The totals below are before tax and are rounded to the nearest whole number.
"
            )
        );
    }

    #[test]
    fn page_numbers_are_told_from_text() {
        for n in ["3", "Page 2", "page 4 of 9", "12 / 40", "- 7 -"] {
            assert!(is_page_number(n), "{n}");
        }
        for t in ["2026 results", "Page two", "1.5", "Chapter 3"] {
            assert!(!is_page_number(t), "{t}");
        }
    }
}

//! Which text a selection covers, as lines. Pure geometry on the page's text runs (PDFium's text
//! objects: a whole line, a word, or, from Chrome and Edge, one glyph each), so it is tested
//! without PDFium. Coordinates are PDF points with y going up.

use serde::{Deserialize, Serialize};

/// A rectangle on the page, in points, y up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
    pub top: f32,
}

impl Rect {
    pub fn new(left: f32, bottom: f32, right: f32, top: f32) -> Rect {
        Rect {
            left: left.min(right),
            bottom: bottom.min(top),
            right: left.max(right),
            top: bottom.max(top),
        }
    }

    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.top - self.bottom
    }

    pub fn area(&self) -> f32 {
        self.width().max(0.0) * self.height().max(0.0)
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.left && x <= self.right && y >= self.bottom && y <= self.top
    }

    pub fn intersection(&self, o: &Rect) -> f32 {
        let w = self.right.min(o.right) - self.left.max(o.left);
        let h = self.top.min(o.top) - self.bottom.max(o.bottom);
        if w <= 0.0 || h <= 0.0 {
            0.0
        } else {
            w * h
        }
    }

    pub fn union(&self, o: &Rect) -> Rect {
        Rect {
            left: self.left.min(o.left),
            bottom: self.bottom.min(o.bottom),
            right: self.right.max(o.right),
            top: self.top.max(o.top),
        }
    }

    /// A square of `pad` points around a point: what a click selects.
    pub fn around(x: f32, y: f32, pad: f32) -> Rect {
        Rect::new(x - pad, y - pad, x + pad, y + pad)
    }
}

/// One text object on the page.
///
/// - `index`: its place in the page's objects
/// - `bounds`: what it covers (zero-sized for a bare space)
/// - `origin_x`, `baseline`: where its text starts (the object's matrix `e` and `f`)
/// - `size`: its font size on the page, in points
/// - `style`: its font, size and colour in one string: runs that look alike share it
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub index: usize,
    pub text: String,
    pub bounds: Rect,
    pub origin_x: f32,
    pub baseline: f32,
    pub size: f32,
    pub style: String,
}

impl Run {
    fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Where it starts and ends on its line: its bounds, or its origin for a bare space.
    fn span(&self) -> (f32, f32) {
        if self.bounds.width() > 0.0 {
            (self.bounds.left, self.bounds.right)
        } else {
            (self.origin_x, self.origin_x)
        }
    }
}

/// Runs on one baseline, left to right, with their text joined.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub runs: Vec<Run>,
    pub text: String,
    pub bounds: Rect,
    pub baseline: f32,
    pub size: f32,
}

/// The runs a dragged rectangle covers. A glyph-sized run counts when its middle is inside; a
/// longer one (a word, a line) as soon as the rectangle touches it, since it cannot be split; a
/// bare space when its origin is inside.
pub fn covered(runs: &[Run], area: Rect) -> Vec<Run> {
    runs.iter()
        .filter(|r| {
            let b = r.bounds;
            if b.area() <= 0.0 {
                return area.contains(r.origin_x, r.baseline + r.size * 0.3);
            }
            if b.width() <= r.size * 1.2 {
                area.contains((b.left + b.right) / 2.0, (b.bottom + b.top) / 2.0)
            } else {
                area.intersection(&b) > 0.0
            }
        })
        .cloned()
        .collect()
}

/// The text under a click: the run of one style on its line (a bold name in a sentence, a
/// link), as far as it goes without a wide gap (so a table's next column is not part of it).
/// Spaces between two runs of the style belong to it. None when no text is under the point.
pub fn line_at(runs: &[Run], x: f32, y: f32) -> Option<Line> {
    let hit = runs.iter().find(|r| {
        !r.is_blank() && {
            let pad = r.size * 0.15;
            Rect::new(
                r.bounds.left - pad,
                r.bounds.bottom - pad,
                r.bounds.right + pad,
                r.bounds.top + pad,
            )
            .contains(x, y)
        }
    })?;
    let mut same: Vec<Run> = runs
        .iter()
        .filter(|r| same_baseline(r, hit))
        .cloned()
        .collect();
    same.sort_by(|a, b| a.span().0.total_cmp(&b.span().0));
    let at = same.iter().position(|r| r.index == hit.index)?;
    let gap = hit.size * 1.5;
    let fits = |r: &Run| r.is_blank() || r.style == hit.style;
    let close = |a: &Run, b: &Run| b.span().0 - a.span().1 <= gap;
    let (mut from, mut to) = (at, at);
    while from > 0 && fits(&same[from - 1]) && close(&same[from - 1], &same[from]) {
        from -= 1;
    }
    while to + 1 < same.len() && fits(&same[to + 1]) && close(&same[to], &same[to + 1]) {
        to += 1;
    }
    // Spaces at the ends are not the style's own.
    while from < to && same[from].is_blank() {
        from += 1;
    }
    while to > from && same[to].is_blank() {
        to -= 1;
    }
    lines(same[from..=to].to_vec()).into_iter().next()
}

fn same_baseline(a: &Run, b: &Run) -> bool {
    (a.baseline - b.baseline).abs() <= a.size.max(b.size) * 0.3
}

/// Runs grouped into lines, top of the page first, each left to right. A space is put between
/// two runs with a gap wider than a fifth of the font size and none of their own.
pub fn lines(mut runs: Vec<Run>) -> Vec<Line> {
    runs.sort_by(|a, b| b.baseline.total_cmp(&a.baseline));
    let mut groups: Vec<Vec<Run>> = Vec::new();
    for r in runs {
        match groups.iter_mut().find(|g| same_baseline(&g[0], &r)) {
            Some(g) => g.push(r),
            None => groups.push(vec![r]),
        }
    }
    let mut out: Vec<Line> = groups
        .into_iter()
        .filter(|g| g.iter().any(|r| !r.is_blank()))
        .map(|mut g| {
            g.sort_by(|a, b| a.span().0.total_cmp(&b.span().0));
            let mut text = String::new();
            let mut last_right: Option<f32> = None;
            for r in &g {
                let (left, right) = r.span();
                if let Some(prev) = last_right {
                    let gap = left - prev;
                    if gap > r.size * 0.2
                        && !text.ends_with(char::is_whitespace)
                        && !r.text.starts_with(char::is_whitespace)
                    {
                        text.push(' ');
                    }
                }
                text.push_str(&r.text);
                last_right = Some(last_right.map_or(right, |p: f32| p.max(right)));
            }
            let inked: Vec<&Run> = g.iter().filter(|r| r.bounds.area() > 0.0).collect();
            let bounds = inked.iter().skip(1).fold(
                inked.first().map(|r| r.bounds).unwrap_or_default(),
                |acc, r| acc.union(&r.bounds),
            );
            let size = g.iter().map(|r| r.size).fold(0.0, f32::max);
            let baseline = g
                .iter()
                .find(|r| !r.is_blank())
                .map_or(g[0].baseline, |r| r.baseline);
            Line {
                text: collapse(&text),
                runs: g,
                bounds,
                baseline,
                size,
            }
        })
        .collect();
    out.sort_by(|a, b| b.baseline.total_cmp(&a.baseline));
    out
}

/// The style most of a line's letters are in: the one an edit of the whole line keeps.
pub fn main_style(line: &Line) -> Option<&str> {
    let mut count: Vec<(&str, usize)> = Vec::new();
    for r in line.runs.iter().filter(|r| !r.is_blank()) {
        let n = r.text.chars().filter(|c| !c.is_whitespace()).count();
        match count.iter_mut().find(|(s, _)| *s == r.style) {
            Some((_, c)) => *c += n,
            None => count.push((&r.style, n)),
        }
    }
    count.into_iter().max_by_key(|(_, n)| *n).map(|(s, _)| s)
}

/// Runs of whitespace as one space, and none at either end.
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether a line reads as a figure (an amount, a date, a code), which is kept to its right edge
/// when it changes length, as figures stand in columns.
pub fn is_figure(text: &str) -> bool {
    let t = text.trim();
    !t.is_empty()
        && t.chars().any(|c| c.is_ascii_digit())
        && t.chars().all(|c| {
            c.is_ascii_digit()
                || c.is_whitespace()
                || ".,:;-+/%()'".contains(c)
                || "€$£¥₹₩₽¢".contains(c)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run per glyph, as Chrome writes them: `text` from `x`, 6 points a glyph.
    fn glyphs(first: usize, text: &str, x: f32, baseline: f32, size: f32) -> Vec<Run> {
        text.chars()
            .enumerate()
            .map(|(i, c)| {
                let left = x + i as f32 * 6.0;
                let blank = c == ' ';
                Run {
                    index: first + i,
                    text: c.to_string(),
                    bounds: if blank {
                        Rect::default()
                    } else {
                        Rect::new(left, baseline - 1.0, left + 5.0, baseline + size * 0.7)
                    },
                    origin_x: left,
                    baseline,
                    size,
                    style: format!("regular {size}"),
                }
            })
            .collect()
    }

    fn word(index: usize, text: &str, x: f32, baseline: f32) -> Run {
        Run {
            index,
            text: text.into(),
            bounds: Rect::new(
                x,
                baseline - 2.0,
                x + text.len() as f32 * 6.0,
                baseline + 8.0,
            ),
            origin_x: x,
            baseline,
            size: 10.0,
            style: "regular".into(),
        }
    }

    #[test]
    fn a_dragged_box_takes_the_glyphs_it_covers_as_lines() {
        let mut runs = glyphs(0, "Bill to: Northwind", 50.0, 700.0, 10.0);
        runs.extend(glyphs(100, "Invoice", 50.0, 740.0, 20.0));
        // around "Northwind" only
        let picked = covered(&runs, Rect::new(99.5, 695.0, 160.0, 712.0));
        let ls = lines(picked);
        assert_eq!(ls.len(), 1);
        assert_eq!(ls[0].text, "Northwind");
        assert_eq!(ls[0].runs[0].index, 9);
        // both lines, top first
        let both = lines(covered(&runs, Rect::new(0.0, 690.0, 400.0, 760.0)));
        assert_eq!(
            both.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
            vec!["Invoice", "Bill to: Northwind"]
        );
        assert_eq!(both[0].size, 20.0);
    }

    #[test]
    fn a_click_takes_its_line_up_to_the_next_column() {
        let runs = vec![
            word(0, "Hosting,", 50.0, 600.0),
            word(1, "12", 104.0, 600.0),
            word(2, "months", 122.0, 600.0),
            // the next column, far to the right
            word(3, "€360.00", 400.0, 600.0),
            word(4, "Other", 50.0, 580.0),
        ];
        let line = line_at(&runs, 110.0, 603.0).unwrap();
        assert_eq!(line.text, "Hosting, 12 months");
        assert_eq!(line.runs.len(), 3);
        assert_eq!(line_at(&runs, 420.0, 603.0).unwrap().text, "€360.00");
        assert!(line_at(&runs, 300.0, 603.0).is_none());
    }

    #[test]
    fn a_click_takes_the_bold_name_out_of_its_sentence() {
        let mut runs = glyphs(0, "Bill to: ", 50.0, 700.0, 10.0);
        let mut bold = glyphs(20, "Northwind Ltd", 104.0, 700.0, 10.0);
        bold.iter_mut().for_each(|r| r.style = "bold 10".into());
        runs.extend(bold);
        runs.extend(glyphs(40, ", Bristol", 182.0, 700.0, 10.0));
        let line = line_at(&runs, 130.0, 703.0).unwrap();
        assert_eq!(line.text, "Northwind Ltd");
        assert_eq!(line.runs.first().unwrap().index, 20);
        assert_eq!(main_style(&line), Some("bold 10"));
        let whole = lines(covered(&runs, Rect::new(0.0, 690.0, 400.0, 720.0)));
        assert_eq!(whole[0].text, "Bill to: Northwind Ltd, Bristol");
        assert_eq!(
            main_style(&whole[0]),
            Some("regular 10"),
            "7 + 8 letters against 12 bold"
        );
    }

    #[test]
    fn a_word_the_box_only_touches_counts_but_a_glyph_needs_its_middle() {
        let runs = vec![word(0, "Northwind", 50.0, 600.0)];
        assert_eq!(
            covered(&runs, Rect::new(100.0, 598.0, 103.0, 601.0)).len(),
            1
        );
        let g = glyphs(0, "ab", 50.0, 600.0, 10.0);
        assert_eq!(covered(&g, Rect::new(50.0, 598.0, 51.0, 606.0)).len(), 0);
        assert_eq!(covered(&g, Rect::new(51.0, 598.0, 54.0, 606.0)).len(), 1);
    }

    #[test]
    fn figures_are_told_from_words() {
        assert!(is_figure("€5,200.00"));
        assert!(is_figure("14/09/2026"));
        assert!(is_figure("(12%)"));
        assert!(!is_figure("Total"));
        assert!(!is_figure("12 months"));
        assert!(!is_figure("—"));
    }
}

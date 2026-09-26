//! Text drawn as pixels (a scan, a form printed flat, letters turned into shapes): which pixels
//! are the letters' ink, its colour and the paper's, and where the letters stand.
//!
//! The pixels are a part of the page PDFium drew: BGRA, rows from the top. Boxes are in those
//! pixels and half open, `left..right` and `top..bottom`; the baseline is the edge between the
//! last row the letters stand on and the row below it.

/// A drawn part of a page: BGRA, `width` × `height`, rows from the top.
pub struct Pixels<'a> {
    pub bgra: &'a [u8],
    pub width: usize,
    pub height: usize,
}

impl Pixels<'_> {
    fn rgb(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.width + x) * 4;
        [self.bgra[i + 2], self.bgra[i + 1], self.bgra[i]]
    }

    fn luma(&self, x: usize, y: usize) -> u8 {
        let [r, g, b] = self.rgb(x, y);
        ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000) as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PxBox {
    pub left: usize,
    pub top: usize,
    pub right: usize,
    pub bottom: usize,
}

impl PxBox {
    pub fn width(&self) -> usize {
        self.right.saturating_sub(self.left)
    }

    pub fn height(&self) -> usize {
        self.bottom.saturating_sub(self.top)
    }

    fn holds(&self, x: f32, y: f32) -> bool {
        x >= self.left as f32
            && x < self.right as f32
            && y >= self.top as f32
            && y < self.bottom as f32
    }
}

/// A word read from the pixels, its box in them.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Word {
    fn height(&self) -> f32 {
        (self.bottom - self.top).max(1.0)
    }

    fn middle(&self) -> (f32, f32) {
        (
            (self.left + self.right) / 2.0,
            (self.top + self.bottom) / 2.0,
        )
    }
}

/// Where ink is: `width` × `height`, rows from the top.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub width: usize,
    pub height: usize,
    pub ink: Vec<bool>,
}

impl Mask {
    pub fn new(width: usize, height: usize) -> Mask {
        Mask {
            width,
            height,
            ink: vec![false; width * height],
        }
    }

    pub fn get(&self, x: usize, y: usize) -> bool {
        x < self.width && y < self.height && self.ink[y * self.width + x]
    }

    pub fn set(&mut self, x: usize, y: usize, on: bool) {
        if x < self.width && y < self.height {
            self.ink[y * self.width + x] = on;
        }
    }

    pub fn count(&self) -> usize {
        self.ink.iter().filter(|&&i| i).count()
    }

    /// The box around the ink; None when there is none.
    pub fn bounds(&self) -> Option<PxBox> {
        let (mut left, mut top, mut right, mut bottom) = (usize::MAX, usize::MAX, 0, 0);
        for y in 0..self.height {
            for x in 0..self.width {
                if self.ink[y * self.width + x] {
                    left = left.min(x);
                    right = right.max(x + 1);
                    top = top.min(y);
                    bottom = bottom.max(y + 1);
                }
            }
        }
        (right > 0).then_some(PxBox {
            left,
            top,
            right,
            bottom,
        })
    }

    pub fn crop(&self, b: PxBox) -> Mask {
        let mut out = Mask::new(b.width(), b.height());
        for y in 0..out.height {
            for x in 0..out.width {
                out.ink[y * out.width + x] = self.get(b.left + x, b.top + y);
            }
        }
        out
    }

    /// The baseline: where the ink thins most sharply in the letters' lower half. The bodies of
    /// the letters end there; below it are only tails (g, p, y) or nothing. (Not where most
    /// strokes end: a T's bar ends high.)
    pub fn baseline(&self) -> Option<usize> {
        let rows: Vec<i64> = (0..self.height)
            .map(|y| (0..self.width).filter(|&x| self.get(x, y)).count() as i64)
            .collect();
        let first = rows.iter().position(|&c| c > 0)?;
        let last = rows.iter().rposition(|&c| c > 0)? + 1;
        let from = (first + (last - first) / 2).max(first + 1);
        (from..=last).max_by_key(|&e| (rows[e - 1] - rows.get(e).copied().unwrap_or(0), e))
    }
}

/// Otsu's threshold: the grey level that best splits the histogram into two groups (the dark
/// one is `..=t`).
pub fn otsu(hist: &[u32; 256]) -> u8 {
    let total: f64 = hist.iter().map(|&c| c as f64).sum();
    let sum: f64 = hist
        .iter()
        .enumerate()
        .map(|(i, &c)| i as f64 * c as f64)
        .sum();
    let (mut below, mut below_sum, mut best, mut t) = (0.0f64, 0.0f64, -1.0f64, 0u8);
    for (i, &c) in hist.iter().enumerate() {
        below += c as f64;
        if below == 0.0 {
            continue;
        }
        let above = total - below;
        if above == 0.0 {
            break;
        }
        below_sum += i as f64 * c as f64;
        let between = below * above * (below_sum / below - (sum - below_sum) / above).powi(2);
        if between > best {
            best = between;
            t = i as u8;
        }
    }
    t
}

/// The letters inside a box: their ink, colour and the paper's, and the line they stand on.
#[derive(Clone, Debug, PartialEq)]
pub struct Letters {
    /// Where their ink is.
    pub bounds: PxBox,
    /// The baseline, in the pixels.
    pub baseline: usize,
    /// The ink, cropped to `bounds`.
    pub mask: Mask,
    pub ink: [u8; 3],
    pub paper: [u8; 3],
}

/// The letters in `around` (the box a word reader gave them). The box is looked at a little
/// larger, for the letters' soft edges; rules that cross it (a form's lines) and bits of
/// neighbours that reach into it are left out. None when there is no ink to speak of.
pub fn letters(p: &Pixels, around: PxBox) -> Option<Letters> {
    let pad = (around.height() / 8).max(2);
    let b = PxBox {
        left: around.left.saturating_sub(pad),
        top: around.top.saturating_sub(pad),
        right: (around.right + pad).min(p.width),
        bottom: (around.bottom + pad).min(p.height),
    };
    if b.width() < 2 || b.height() < 2 {
        return None;
    }
    let mut hist = [0u32; 256];
    let (mut darkest, mut lightest) = (255u8, 0u8);
    for y in b.top..b.bottom {
        for x in b.left..b.right {
            let l = p.luma(x, y);
            hist[l as usize] += 1;
            darkest = darkest.min(l);
            lightest = lightest.max(l);
        }
    }
    if lightest.saturating_sub(darkest) < 40 {
        return None;
    }
    let t = otsu(&hist);
    // The paper is what the box's edge mostly is; the ink the other side of the threshold.
    let (mut light, mut dark) = (0usize, 0usize);
    for x in b.left..b.right {
        for y in [b.top, b.bottom - 1] {
            if p.luma(x, y) > t {
                light += 1;
            } else {
                dark += 1;
            }
        }
    }
    for y in b.top..b.bottom {
        for x in [b.left, b.right - 1] {
            if p.luma(x, y) > t {
                light += 1;
            } else {
                dark += 1;
            }
        }
    }
    let dark_ink = light >= dark;
    let is_ink = |l: u8| if dark_ink { l <= t } else { l > t };

    let mut mask = Mask::new(b.width(), b.height());
    for y in 0..mask.height {
        for x in 0..mask.width {
            mask.set(x, y, is_ink(p.luma(b.left + x, b.top + y)));
        }
    }
    drop_rules(&mut mask);
    keep_own(&mut mask, around, b);

    let own = mask.bounds()?;
    let bounds = PxBox {
        left: b.left + own.left,
        top: b.top + own.top,
        right: b.left + own.right,
        bottom: b.top + own.bottom,
    };
    let mask = mask.crop(own);
    let baseline = bounds.top + mask.baseline()?;

    // The ink's colour from its core, the darkest quarter (soft edges and thin strokes are part
    // paper), the paper's from what is clearly not ink.
    let mut inked: Vec<(u8, [u8; 3])> = Vec::new();
    let mut paper: Vec<[u8; 3]> = Vec::new();
    // Clearly paper: within a quarter of the way from the paper side's average to the ink's.
    let mean = |range: std::ops::RangeInclusive<usize>| {
        let (n, s) = range.fold((0u64, 0u64), |(n, s), i| {
            (n + hist[i] as u64, s + i as u64 * hist[i] as u64)
        });
        s as f32 / n.max(1) as f32
    };
    let (dark_mean, light_mean) = (mean(0..=t as usize), mean(t as usize + 1..=255));
    let quarter = (light_mean - dark_mean) / 4.0;
    for y in b.top..b.bottom {
        for x in b.left..b.right {
            let (l, c) = (p.luma(x, y), p.rgb(x, y));
            let at = |bx: usize, by: usize| {
                bx >= bounds.left && by >= bounds.top && mask.get(bx - bounds.left, by - bounds.top)
            };
            if at(x, y) {
                inked.push((l, c));
            } else if (dark_ink && l as f32 >= light_mean - quarter)
                || (!dark_ink && l as f32 <= dark_mean + quarter)
            {
                paper.push(c);
            }
        }
    }
    if inked.is_empty() {
        return None;
    }
    inked.sort_by_key(|&(l, _)| if dark_ink { l } else { 255 - l });
    let core: Vec<[u8; 3]> = inked[..inked.len().div_ceil(4)]
        .iter()
        .map(|&(_, c)| c)
        .collect();
    let paper = if paper.is_empty() {
        if dark_ink {
            [255, 255, 255]
        } else {
            [0, 0, 0]
        }
    } else {
        median(&paper)
    };
    Some(Letters {
        bounds,
        baseline,
        mask,
        ink: median(&core),
        paper,
    })
}

fn median(colours: &[[u8; 3]]) -> [u8; 3] {
    let mut out = [0u8; 3];
    for (ch, o) in out.iter_mut().enumerate() {
        let mut v: Vec<u8> = colours.iter().map(|c| c[ch]).collect();
        v.sort_unstable();
        *o = v[v.len() / 2];
    }
    out
}

/// Clears lines that run right across the box (a form's rules under or through the words): a
/// row of ink nearly as wide as a wide box, or a column as tall as the whole box.
fn drop_rules(mask: &mut Mask) {
    let (w, h) = (mask.width, mask.height);
    if w >= 3 * h {
        let full: Vec<bool> = (0..h)
            .map(|y| (0..w).filter(|&x| mask.get(x, y)).count() * 100 >= w * 85)
            .collect();
        for y in 0..h {
            let half = (0..w).filter(|&x| mask.get(x, y)).count() * 2 >= w;
            let near = full[y] || (y > 0 && full[y - 1]) || (y + 1 < h && full[y + 1]);
            if full[y] || (half && near) {
                for x in 0..w {
                    mask.set(x, y, false);
                }
            }
        }
    }
    for x in 0..w {
        if (0..h).filter(|&y| mask.get(x, y)).count() * 100 >= h * 98 {
            for y in 0..h {
                mask.set(x, y, false);
            }
        }
    }
}

/// Keeps the pieces of ink whose middle is in `around` (the word's own box, in the pixels; the
/// mask covers `b`), so a neighbour's letters that reach into the margin go.
fn keep_own(mask: &mut Mask, around: PxBox, b: PxBox) {
    for piece in pieces(mask) {
        let (mx, my) = piece.middle();
        if !around.holds(b.left as f32 + mx, b.top as f32 + my) {
            for i in piece.pixels {
                mask.ink[i] = false;
            }
        }
    }
}

/// A piece of ink: its pixels (indexes into its mask) and its box, `right` and `bottom` the last
/// column and row it reaches.
struct Piece {
    pixels: Vec<usize>,
    left: usize,
    top: usize,
    right: usize,
    bottom: usize,
}

impl Piece {
    fn middle(&self) -> (f32, f32) {
        (
            (self.left + self.right) as f32 / 2.0 + 0.5,
            (self.top + self.bottom) as f32 / 2.0 + 0.5,
        )
    }
}

/// The mask's pieces of ink, touching pixels (corners too) together.
fn pieces(mask: &Mask) -> Vec<Piece> {
    let (w, h) = (mask.width, mask.height);
    let mut seen = vec![false; w * h];
    let mut stack = Vec::new();
    let mut out = Vec::new();
    for start in 0..w * h {
        if seen[start] || !mask.ink[start] {
            continue;
        }
        let mut piece = Piece {
            pixels: Vec::new(),
            left: usize::MAX,
            top: usize::MAX,
            right: 0,
            bottom: 0,
        };
        seen[start] = true;
        stack.push(start);
        while let Some(i) = stack.pop() {
            piece.pixels.push(i);
            let (x, y) = (i % w, i / w);
            piece.left = piece.left.min(x);
            piece.top = piece.top.min(y);
            piece.right = piece.right.max(x);
            piece.bottom = piece.bottom.max(y);
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    let n = ny as usize * w + nx as usize;
                    if !seen[n] && mask.ink[n] {
                        seen[n] = true;
                        stack.push(n);
                    }
                }
            }
        }
        out.push(piece);
    }
    out
}

/// A letter cut from the page: how much ink each of its pixels holds (0 to 255), the rows of it
/// above the baseline, its ink's colour, and where it stood (the page's pixels: its left edge and
/// its baseline).
#[derive(Clone, Debug, PartialEq)]
pub struct Cut {
    pub width: usize,
    pub height: usize,
    pub alpha: Vec<u8>,
    pub above: usize,
    pub ink: [u8; 3],
    pub at: (usize, usize),
    /// The ink's own height, in pixels (the cut has a pixel of margin around it).
    pub ink_height: usize,
}

/// The letters of one phrase (`letters` read from `p` over `words`), each with its character:
/// a word's pieces of ink, those stacked over each other taken as one (an i and its dot, an
/// accent), are its letters when there are as many as it has characters. A word whose letters
/// touch (or come apart) gives none, as nothing says which piece is which.
pub fn cut_letters(p: &Pixels, letters: &Letters, words: &[Word]) -> Vec<(char, Cut)> {
    let b = letters.bounds;
    let luma = |[r, g, bl]: [u8; 3]| r as f32 * 0.299 + g as f32 * 0.587 + bl as f32 * 0.114;
    let (paper, ink) = (luma(letters.paper), luma(letters.ink));
    if (paper - ink).abs() < 30.0 {
        return Vec::new();
    }
    let all = pieces(&letters.mask);
    let mut out = Vec::new();
    for word in words {
        let (wl, wr) = (word.left - b.left as f32, word.right - b.left as f32);
        let mut mine: Vec<&Piece> = all
            .iter()
            .filter(|pc| (wl..wr).contains(&pc.middle().0))
            .collect();
        mine.sort_by_key(|pc| pc.left);
        // Pieces over each other are one letter.
        let mut groups: Vec<Vec<&Piece>> = Vec::new();
        for pc in mine {
            if let Some(g) = groups.last_mut() {
                let (gl, gr) = (
                    g.iter().map(|q| q.left).min().unwrap_or(0),
                    g.iter().map(|q| q.right).max().unwrap_or(0),
                );
                let overlap = gr.min(pc.right) as i64 - gl.max(pc.left) as i64 + 1;
                let narrower = (gr - gl + 1).min(pc.right - pc.left + 1) as i64;
                if overlap * 10 >= narrower * 3 {
                    g.push(pc);
                    continue;
                }
            }
            groups.push(vec![pc]);
        }
        let chars: Vec<char> = word.text.chars().filter(|c| !c.is_whitespace()).collect();
        if groups.len() != chars.len() {
            continue;
        }
        for (c, g) in chars.into_iter().zip(groups) {
            let (l, t, r, bm) = g.iter().fold((usize::MAX, usize::MAX, 0, 0), |a, q| {
                (
                    a.0.min(q.left),
                    a.1.min(q.top),
                    a.2.max(q.right),
                    a.3.max(q.bottom),
                )
            });
            // A pixel of margin on the page, for the soft edges; only this letter's own pixels
            // and those touching them, so a neighbour's edge does not come along.
            let (x0, y0) = (
                (b.left + l).saturating_sub(1),
                (b.top + t).saturating_sub(1),
            );
            let (x1, y1) = (
                (b.left + r + 1).min(p.width - 1),
                (b.top + bm + 1).min(p.height - 1),
            );
            let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
            let mut own = vec![false; w * h];
            for q in &g {
                for &i in &q.pixels {
                    let (x, y) = (
                        b.left + i % letters.mask.width,
                        b.top + i / letters.mask.width,
                    );
                    for yy in y.saturating_sub(1).max(y0)..=(y + 1).min(y1) {
                        for xx in x.saturating_sub(1).max(x0)..=(x + 1).min(x1) {
                            own[(yy - y0) * w + (xx - x0)] = true;
                        }
                    }
                }
            }
            let mut alpha = vec![0u8; w * h];
            for yy in 0..h {
                for xx in 0..w {
                    if own[yy * w + xx] {
                        let l = luma(p.rgb(x0 + xx, y0 + yy));
                        alpha[yy * w + xx] =
                            (((paper - l) / (paper - ink)).clamp(0.0, 1.0) * 255.0).round() as u8;
                    }
                }
            }
            let top = y0;
            if letters.baseline <= top {
                continue;
            }
            out.push((
                c,
                Cut {
                    width: w,
                    height: h,
                    alpha,
                    above: letters.baseline - top,
                    ink: letters.ink,
                    at: (x0, letters.baseline),
                    ink_height: bm - t + 1,
                },
            ));
        }
    }
    out
}

/// The words at a click: the line holding the word under it (or the nearest on its row, within
/// a letter's height), cut where a gap is wider than the letters are tall, so a form's separate
/// fields stay apart.
pub fn phrase_at(lines: &[Vec<Word>], x: f32, y: f32) -> Option<Vec<Word>> {
    let mut best: Option<(usize, usize, f32)> = None;
    for (li, line) in lines.iter().enumerate() {
        for (wi, w) in line.iter().enumerate() {
            let h = w.height();
            if y < w.top - h * 0.3 || y > w.bottom + h * 0.3 {
                continue;
            }
            let dx = (w.left - x).max(x - w.right).max(0.0);
            if dx > h {
                continue;
            }
            let d = dx + (y - w.middle().1).abs() * 0.5;
            if best.is_none_or(|b| d < b.2) {
                best = Some((li, wi, d));
            }
        }
    }
    let (li, wi, _) = best?;
    let hit = &lines[li][wi];
    phrases(&lines[li]).into_iter().find(|ph| ph.contains(hit))
}

/// A line's words in phrases, left to right: cut where a gap is wider than the letters are tall
/// (a form's separate fields).
pub fn phrases(line: &[Word]) -> Vec<Vec<Word>> {
    let mut words = line.to_vec();
    words.sort_by(|a, b| a.left.total_cmp(&b.left));
    let mut heights: Vec<f32> = words.iter().map(Word::height).collect();
    heights.sort_by(f32::total_cmp);
    let Some(&middle) = heights.get(heights.len() / 2) else {
        return Vec::new();
    };
    let gap = middle * 1.2;
    let mut out: Vec<Vec<Word>> = Vec::new();
    for w in words {
        match out.last_mut() {
            Some(ph) if w.left - ph[ph.len() - 1].right <= gap => ph.push(w),
            _ => out.push(vec![w]),
        }
    }
    out
}

/// The words a dragged box holds (their middles inside it), line by line from the top.
pub fn words_in(lines: &[Vec<Word>], b: PxBox) -> Vec<Vec<Word>> {
    let mut out: Vec<Vec<Word>> = lines
        .iter()
        .map(|line| {
            let mut ws: Vec<Word> = line
                .iter()
                .filter(|w| {
                    let (mx, my) = w.middle();
                    b.holds(mx, my)
                })
                .cloned()
                .collect();
            ws.sort_by(|a, b| a.left.total_cmp(&b.left));
            ws
        })
        .filter(|ws| !ws.is_empty())
        .collect();
    out.sort_by(|a, b| a[0].top.total_cmp(&b[0].top));
    out
}

/// The box around some words, in whole pixels.
pub fn around(words: &[Word]) -> PxBox {
    let (l, t, r, b) = words.iter().fold(
        (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
        |(l, t, r, b), w| (l.min(w.left), t.min(w.top), r.max(w.right), b.max(w.bottom)),
    );
    PxBox {
        left: l.max(0.0).floor() as usize,
        top: t.max(0.0).floor() as usize,
        right: r.max(0.0).ceil() as usize,
        bottom: b.max(0.0).ceil() as usize,
    }
}

/// The words as one line of text.
pub fn text_of(words: &[Word]) -> String {
    words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page part `w` × `h` of one colour, drawn on with `fill`.
    struct Canvas {
        w: usize,
        h: usize,
        px: Vec<u8>,
    }

    impl Canvas {
        fn new(w: usize, h: usize, rgb: [u8; 3]) -> Canvas {
            let mut px = Vec::with_capacity(w * h * 4);
            for _ in 0..w * h {
                px.extend([rgb[2], rgb[1], rgb[0], 255]);
            }
            Canvas { w, h, px }
        }

        fn fill(&mut self, l: usize, t: usize, r: usize, b: usize, rgb: [u8; 3]) {
            for y in t..b {
                for x in l..r {
                    let i = (y * self.w + x) * 4;
                    self.px[i..i + 3].copy_from_slice(&[rgb[2], rgb[1], rgb[0]]);
                }
            }
        }

        fn pixels(&self) -> Pixels<'_> {
            Pixels {
                bgra: &self.px,
                width: self.w,
                height: self.h,
            }
        }
    }

    fn word(text: &str, l: f32, t: f32, r: f32, b: f32) -> Word {
        Word {
            text: text.into(),
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
    }

    #[test]
    fn otsu_splits_two_greys() {
        let mut hist = [0u32; 256];
        hist[30] = 100;
        hist[220] = 900;
        let t = otsu(&hist);
        assert!((30..220).contains(&t), "{t}");
    }

    #[test]
    fn the_letters_leave_out_the_rule_under_them_and_the_neighbour_above() {
        // Cream paper, dark blue T-like "letters" standing on the edge at 40 (one with a tail to
        // 46), a rule right across at rows 48-49, and a neighbour's tail reaching down from above.
        let paper = [250, 248, 240];
        let blue = [20, 40, 120];
        let mut c = Canvas::new(300, 70, paper);
        for i in 0..8 {
            let l = 20 + i * 30;
            c.fill(l, 20, l + 6, 40, blue);
            c.fill(l + 6, 20, l + 18, 24, blue);
        }
        c.fill(260, 20, 266, 46, blue); // a p's tail
        c.fill(5, 48, 295, 50, blue); // the rule
        c.fill(100, 0, 104, 19, blue); // a neighbour's tail, reaching into the margin
        let got = letters(
            &c.pixels(),
            PxBox {
                left: 18,
                top: 17,
                right: 270,
                bottom: 47,
            },
        )
        .expect("letters");
        assert_eq!(
            got.bounds,
            PxBox {
                left: 20,
                top: 20,
                right: 266,
                bottom: 46
            },
            "the rule and the neighbour are not the word's"
        );
        assert_eq!(got.baseline, 40, "the tail does not move the baseline");
        assert_eq!(got.ink, blue);
        assert_eq!(got.paper, paper);
        assert_eq!(got.mask.width, 246);
    }

    #[test]
    fn light_letters_on_a_dark_panel() {
        let mut c = Canvas::new(120, 40, [30, 30, 30]);
        for l in [20, 45, 70] {
            c.fill(l, 10, l + 12, 30, [240, 240, 240]);
            c.fill(l + 4, 14, l + 8, 26, [30, 30, 30]);
        }
        let got = letters(
            &c.pixels(),
            PxBox {
                left: 18,
                top: 8,
                right: 102,
                bottom: 32,
            },
        )
        .unwrap();
        assert_eq!(got.ink, [240, 240, 240]);
        assert_eq!(got.paper, [30, 30, 30]);
        assert_eq!(got.baseline, 30);
    }

    #[test]
    fn nothing_on_plain_paper() {
        let c = Canvas::new(50, 20, [255, 255, 255]);
        assert!(letters(
            &c.pixels(),
            PxBox {
                left: 5,
                top: 5,
                right: 45,
                bottom: 15
            }
        )
        .is_none());
    }

    #[test]
    fn a_click_takes_its_field_not_the_whole_row() {
        // "Country Turkey" far from "Date 6-23-1990" on one row, as a form's fields are.
        let lines = vec![vec![
            word("Country", 10.0, 10.0, 70.0, 30.0),
            word("Turkey", 76.0, 10.0, 130.0, 30.0),
            word("Date", 300.0, 10.0, 340.0, 30.0),
            word("6-23-1990", 346.0, 10.0, 430.0, 30.0),
        ]];
        let got = phrase_at(&lines, 380.0, 22.0).unwrap();
        assert_eq!(text_of(&got), "Date 6-23-1990");
        let got = phrase_at(&lines, 100.0, 12.0).unwrap();
        assert_eq!(text_of(&got), "Country Turkey");
        assert!(
            phrase_at(&lines, 200.0, 20.0).is_none(),
            "between the fields"
        );
        assert!(phrase_at(&lines, 100.0, 60.0).is_none(), "below the row");
        assert_eq!(
            around(&got),
            PxBox {
                left: 10,
                top: 10,
                right: 130,
                bottom: 30
            }
        );
    }

    #[test]
    fn a_word_is_cut_into_its_letters_an_i_with_its_dot() {
        // "hi!" in black on white: an h (two stems and a bridge), an i under its dot, and a !
        // over its point, standing on row 40.
        let (paper, black) = ([255, 255, 255], [0, 0, 0]);
        let mut c = Canvas::new(120, 60, paper);
        c.fill(10, 12, 14, 40, black);
        c.fill(14, 24, 22, 28, black);
        c.fill(22, 24, 26, 40, black);
        c.fill(34, 24, 38, 40, black);
        c.fill(34, 14, 38, 18, black);
        c.fill(46, 12, 50, 32, black);
        c.fill(46, 36, 50, 40, black);
        let hi = word("hi!", 8.0, 10.0, 52.0, 42.0);
        let letters = letters(
            &c.pixels(),
            PxBox {
                left: 8,
                top: 10,
                right: 52,
                bottom: 42,
            },
        )
        .unwrap();
        let cut = cut_letters(&c.pixels(), &letters, std::slice::from_ref(&hi));
        let chars: String = cut.iter().map(|(ch, _)| *ch).collect();
        assert_eq!(chars, "hi!");
        let (_, i) = &cut[1];
        assert_eq!(
            (i.width, i.height),
            (6, 28),
            "the dot and the stem, a pixel of margin round"
        );
        assert_eq!(i.above, 27, "from above the dot down to the baseline");
        assert_eq!(i.ink_height, 26);
        assert_eq!(i.alpha[i.width + 1], 255, "full ink in the dot");
        assert_eq!(i.alpha[0], 0, "no ink in the margin's corner");
        assert_eq!(i.at, (33, 40));

        // Three pieces read as four characters (letters that touched): nothing says which is which.
        let hint = word("hint", 8.0, 10.0, 52.0, 42.0);
        assert!(cut_letters(&c.pixels(), &letters, &[hint]).is_empty());
    }

    #[test]
    fn a_box_takes_the_words_inside_it_line_by_line() {
        let lines = vec![
            vec![word("second", 10.0, 40.0, 60.0, 55.0)],
            vec![
                word("first", 10.0, 10.0, 50.0, 25.0),
                word("line", 55.0, 10.0, 90.0, 25.0),
                word("outside", 200.0, 10.0, 260.0, 25.0),
            ],
        ];
        let got = words_in(
            &lines,
            PxBox {
                left: 0,
                top: 0,
                right: 120,
                bottom: 60,
            },
        );
        let texts: Vec<String> = got.iter().map(|l| text_of(l)).collect();
        assert_eq!(texts, ["first line", "second"]);
    }
}

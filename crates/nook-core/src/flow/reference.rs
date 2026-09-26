//! Ports `flow/Reference.java`: the stretch of the track a voice is cloned from, a few seconds of
//! the speaker with the words they say, which the cloning voices take as their reference.

use std::path::Path;

use anyhow::Result;

use super::audio;
use super::subtitles::Segment;

/// The reference should be at least this long, and is cut at the most.
pub const MIN_SECONDS: f64 = 3.0;
pub const MAX_SECONDS: f64 = 12.0;
pub const BEST_SECONDS: f64 = 8.0;

/// The best stretch: one line, or a run of following lines, between three and twelve seconds long,
/// closest to eight; the lines' words become the reference transcript. None when the track has no
/// such stretch (a few very short lines).
pub fn pick(lines: &[Segment]) -> Option<Segment> {
    let mut best: Option<Segment> = None;
    let mut best_score = f64::MAX;
    for i in 0..lines.len() {
        let start = lines[i].start;
        let mut text = String::new();
        for j in i..lines.len() {
            let s = &lines[j];
            if j > i && s.start - lines[j - 1].end > 1.0 {
                break; // a pause: not one stretch
            }
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(s.text.trim());
            let length = s.end - start;
            if length > MAX_SECONDS {
                break;
            }
            if length < MIN_SECONDS {
                continue;
            }
            let score = (length - BEST_SECONDS).abs();
            if score < best_score {
                best_score = score;
                best = Some(Segment::new(start, s.end, text.clone()));
            }
        }
    }
    best
}

/// What to clone from when [`pick`] finds nothing: the start of the track, up to twelve seconds,
/// with the first few lines' words.
pub fn fallback(lines: &[Segment], duration: f64) -> Segment {
    let text = lines
        .iter()
        .take(3)
        .map(|s| s.text.trim())
        .collect::<Vec<_>>()
        .join(" ");
    let start = lines.first().map(|s| s.start).unwrap_or(0.0).max(0.0);
    Segment::new(start, (start + MAX_SECONDS).min(duration), text)
}

/// Writes the part of `wav` between `start` and `end` seconds to `out`.
pub fn cut(wav: &Path, start: f64, end: f64, out: &Path) -> Result<()> {
    let pcm = audio::cut_wav(wav, start, end)?;
    audio::write_wav(out, &pcm)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: f64, end: f64, text: &str) -> Segment {
        Segment::new(start, end, text)
    }

    #[test]
    fn the_stretch_closest_to_eight_seconds_wins() {
        let lines = vec![
            seg(0.0, 2.0, "Hi."),
            seg(2.2, 6.0, "This is a test."),
            seg(6.1, 10.5, "It goes on."),
            seg(12.0, 30.0, "A very long line."),
        ];
        let best = pick(&lines).unwrap();
        assert_eq!(
            (best.start, best.end),
            (2.2, 10.5),
            "8.3 s beats 10.5 s and 6 s"
        );
        assert_eq!(best.text, "This is a test. It goes on.");
        assert!(pick(&[seg(0.0, 1.0, "a"), seg(5.0, 6.0, "b")]).is_none());
        let f = fallback(&[seg(0.5, 1.0, "a"), seg(5.0, 6.0, "b")], 7.0);
        assert_eq!((f.start, f.end, f.text.as_str()), (0.5, 7.0, "a b"));
    }

    #[test]
    fn a_cut_is_the_stretch_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("audio.wav");
        audio::tests::write_tone(&wav, 16_000, 1, 1.0, 200.0);
        let out = dir.path().join("ref.wav");
        cut(&wav, 0.25, 0.5, &out).unwrap();
        let back = audio::read_wav(&out).unwrap();
        assert_eq!(back.rate, 16_000);
        assert_eq!(back.samples.len(), 4000);
        assert!(cut(&wav, 2.0, 3.0, &out).is_err());
    }
}

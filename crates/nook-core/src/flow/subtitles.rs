//! Ports `flow/Segment.java` and `flow/Subtitles.java`: a stretch of speech with its translation,
//! and the files a run writes from them, subtitles (SRT) and plain text, from the original or the
//! translation.

use serde::{Deserialize, Serialize};

/// One stretch of speech: where it starts and ends in the track (seconds), what was said, and
/// what that is in the target language once translated (None before).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub translation: Option<String>,
}

impl Segment {
    pub fn new(start: f64, end: f64, text: impl Into<String>) -> Segment {
        Segment {
            start,
            end,
            text: text.into(),
            translation: None,
        }
    }

    pub fn with_translation(&self, t: impl Into<String>) -> Segment {
        Segment {
            translation: Some(t.into()),
            ..self.clone()
        }
    }

    fn line(&self, translated: bool) -> Option<&str> {
        let line = if translated {
            self.translation.as_deref()?
        } else {
            self.text.as_str()
        };
        let line = line.trim();
        (!line.is_empty()).then_some(line)
    }
}

/// SubRip: numbered cues with "HH:MM:SS,mmm --> HH:MM:SS,mmm" and the text, a blank line between.
pub fn srt(segments: &[Segment], translated: bool) -> String {
    let mut out = String::new();
    let mut n = 1;
    for s in segments {
        let Some(line) = s.line(translated) else {
            continue;
        };
        out.push_str(&format!(
            "{n}\n{} --> {}\n{line}\n\n",
            timestamp(s.start),
            timestamp(s.end)
        ));
        n += 1;
    }
    out
}

/// The text, one segment per line.
pub fn plain(segments: &[Segment], translated: bool) -> String {
    let mut out = String::new();
    for line in segments.iter().filter_map(|s| s.line(translated)) {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// "01:02:03,450" for 3723.45 seconds.
pub fn timestamp(seconds: f64) -> String {
    let ms = (seconds.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitles_number_the_cues_and_skip_empty_lines() {
        let segments = vec![
            Segment::new(0.0, 1.5, "Hello.").with_translation("Hallo."),
            Segment::new(1.5, 2.0, "  "),
            Segment::new(3723.45, 3725.0, "Bye.").with_translation("Tschüss."),
        ];
        assert_eq!(
            srt(&segments, true),
            "1\n00:00:00,000 --> 00:00:01,500\nHallo.\n\n2\n01:02:03,450 --> 01:02:05,000\nTschüss.\n\n"
        );
        assert_eq!(plain(&segments, false), "Hello.\nBye.\n");
        assert_eq!(timestamp(-1.0), "00:00:00,000");
    }
}

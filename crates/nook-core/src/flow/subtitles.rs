//! Ports `flow/Segment.java` and `flow/Subtitles.java`: a stretch of speech with its translation,
//! and the files a run writes from them, subtitles (SRT) and plain text, from the original or the
//! translation.

use serde::{Deserialize, Serialize};

/// One stretch of speech: where it starts and ends in the track (seconds), what was said, and
/// what that is in the target language once translated (None before). `spoken_start` and
/// `spoken_end` are where the translation is heard in the dubbed track, once it is made: a line
/// moves when the one before it ran long, and a recording's lines are laid one after the other,
/// so the translated subtitles follow the dubbed track, not the original.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub translation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spoken_start: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spoken_end: Option<f64>,
}

impl Segment {
    pub fn new(start: f64, end: f64, text: impl Into<String>) -> Segment {
        Segment {
            start,
            end,
            text: text.into(),
            translation: None,
            spoken_start: None,
            spoken_end: None,
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
/// The translation's cues are timed as it is heard in the dubbed track when there is one.
pub fn srt(segments: &[Segment], translated: bool) -> String {
    let mut out = String::new();
    let mut n = 1;
    for s in segments {
        let Some(line) = s.line(translated) else {
            continue;
        };
        let (start, end) = match (translated, s.spoken_start, s.spoken_end) {
            (true, Some(a), Some(b)) => (a, b),
            _ => (s.start, s.end),
        };
        out.push_str(&format!(
            "{n}\n{} --> {}\n{line}\n\n",
            timestamp(start),
            timestamp(end)
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

/// WebVTT, the web's subtitles: a header, then cues with "HH:MM:SS.mmm --> HH:MM:SS.mmm".
pub fn vtt(segments: &[Segment]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for s in segments {
        let Some(line) = s.line(false) else { continue };
        out.push_str(&format!(
            "{} --> {}\n{line}\n\n",
            timestamp(s.start).replace(',', "."),
            timestamp(s.end).replace(',', ".")
        ));
    }
    out
}

/// The transcript to read: what was said in paragraphs, a new one after a pause of `pause`
/// seconds or more, or when a paragraph has grown long.
pub fn paragraphs(segments: &[Segment], pause: f64) -> String {
    let mut out = String::new();
    let mut now = String::new();
    let mut last_end: Option<f64> = None;
    for s in segments {
        let Some(line) = s.line(false) else { continue };
        let gap = last_end.map_or(0.0, |e| s.start - e);
        if !now.is_empty() && (gap >= pause || now.chars().count() > 700) {
            out.push_str(now.trim());
            out.push_str("\n\n");
            now.clear();
        }
        now.push_str(line);
        now.push(' ');
        last_end = Some(s.end);
    }
    if !now.trim().is_empty() {
        out.push_str(now.trim());
        out.push('\n');
    }
    out
}

/// The transcript with a time before each paragraph ("[01:02]"), for reading along a recording.
pub fn timed(segments: &[Segment], pause: f64) -> String {
    let mut out = String::new();
    let mut now = String::new();
    let mut last_end: Option<f64> = None;
    for s in segments {
        let Some(line) = s.line(false) else { continue };
        let gap = last_end.map_or(f64::INFINITY, |e| s.start - e);
        if gap >= pause || now.chars().count() > 700 {
            if !now.is_empty() {
                out.push_str(now.trim_end());
                out.push_str("\n\n");
            }
            now = format!("[{}] ", clock(s.start));
        }
        now.push_str(line);
        now.push(' ');
        last_end = Some(s.end);
    }
    if !now.trim().is_empty() {
        out.push_str(now.trim_end());
        out.push('\n');
    }
    out
}

/// "1:02:03" for an hour and more, else "02:03".
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
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

        // Once dubbed, the translation's cues follow the dubbed track; the original's stay.
        let mut moved = segments[0].clone();
        moved.spoken_start = Some(0.3);
        moved.spoken_end = Some(2.1);
        assert_eq!(
            srt(&[moved.clone()], true),
            "1\n00:00:00,300 --> 00:00:02,100\nHallo.\n\n"
        );
        assert_eq!(
            srt(&[moved], false),
            "1\n00:00:00,000 --> 00:00:01,500\nHello.\n\n"
        );
    }

    #[test]
    fn a_transcript_reads_in_paragraphs_and_as_web_subtitles() {
        let segments = vec![
            Segment::new(0.0, 1.0, "Good morning."),
            Segment::new(1.2, 2.0, "Let's start."),
            Segment::new(5.0, 6.0, "First, the budget."),
            Segment::new(3725.0, 3726.0, " "),
        ];
        assert_eq!(
            paragraphs(&segments, 2.0),
            "Good morning. Let's start.\n\nFirst, the budget.\n"
        );
        assert_eq!(
            timed(&segments, 2.0),
            "[00:00] Good morning. Let's start.\n\n[00:05] First, the budget.\n"
        );
        assert_eq!(
            vtt(&segments[..1]),
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nGood morning.\n\n"
        );
        assert_eq!(clock(3723.0), "1:02:03");
        assert_eq!(clock(65.4), "01:05");
    }
}

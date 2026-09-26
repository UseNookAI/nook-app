//! Ports `flow/Dub.java`: lays the spoken lines over the original timeline. Each line starts where
//! the original did, or as soon as the line before it ends; a line that runs long for its slot is
//! spoken a little faster (up to [`MAX_SPEED`], pitch kept, [`audio::time_stretch`]), and past
//! that the lines that follow move later. The original sped lines up with FFmpeg; this port does it
//! itself, so a sound track needs no FFmpeg at all.
//!
//! New here: a recording from the microphone is laid out [`Layout::Compact`], the lines one after
//! another with a short pause, so the translation plays at once instead of after the silence the
//! person left before speaking.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::audio::{self, Pcm, TrackWriter};
use super::subtitles::Segment;

/// The most a line is sped up to fit its slot.
pub const MAX_SPEED: f64 = 1.3;
/// A line may overrun its slot by this much before it is sped up.
pub const SLACK: f64 = 0.15;
/// The pause kept between two lines that had to move.
pub const GAP: f64 = 0.08;
/// The pause between two lines of a recording laid out compactly, and before the first.
pub const PAUSE: f64 = 0.3;
/// Quieter than this (about -40 dBFS) counts as the silence a voice leaves around a line.
const SILENCE: f32 = 0.01;
/// What is kept of that silence on either side.
const MARGIN: f64 = 0.04;
/// How long FFmpeg may take to put a video together.
const MUX_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How the spoken lines are laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Each line where the original was: a video or a podcast keeps its timing.
    Timeline,
    /// One line after the other: a recording is heard at once.
    Compact,
}

/// Where a spoken line landed: start and length in seconds, and how much it was sped up.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub index: usize,
    pub start: f64,
    pub seconds: f64,
    pub speed: f64,
}

/// The time a line has before the next one starts (the last line: its own length).
pub fn slot_seconds(lines: &[Segment], i: usize) -> f64 {
    let s = &lines[i];
    lines[i + 1..]
        .iter()
        .find(|next| next.start > s.start)
        .map(|next| next.start - s.start)
        .unwrap_or_else(|| (s.end - s.start).max(0.0))
}

/// The line without the silence the voice left before and after it (a short margin is kept).
pub fn trim(pcm: Pcm) -> Pcm {
    let first = pcm.samples.iter().position(|x| x.abs() > SILENCE);
    let last = pcm.samples.iter().rposition(|x| x.abs() > SILENCE);
    let (Some(first), Some(last)) = (first, last) else {
        return pcm;
    };
    let margin = (MARGIN * pcm.rate as f64) as usize;
    let from = first.saturating_sub(margin);
    let to = (last + 1 + margin).min(pcm.samples.len());
    Pcm {
        samples: pcm.samples[from..to].to_vec(),
        rate: pcm.rate,
    }
}

/// Writes the dubbed track to `out` as 16-bit mono WAV at the first clip's rate, and returns
/// where each line landed, in order.
///
/// - `lines`: the original lines with their times
/// - `clips`: the spoken line for each, or None where there is none
/// - `min_seconds`: on the timeline, the track is padded with silence to at least this long (the
///   original's length), so a video keeps its end
pub fn assemble(
    lines: &[Segment],
    clips: &[Option<PathBuf>],
    layout: Layout,
    min_seconds: f64,
    out: &Path,
) -> Result<Vec<Placed>> {
    if lines.len() != clips.len() {
        bail!("one clip per line");
    }
    let mut writer: Option<TrackWriter> = None;
    let mut placed = Vec::new();
    // the earliest the next line may start
    let mut cursor = 0.0;
    for (i, clip) in clips.iter().enumerate() {
        let Some(clip) = clip else { continue };
        let mut pcm = audio::read_wav(clip)?;
        let rate = writer.as_ref().map_or(pcm.rate, TrackWriter::rate);
        if pcm.rate != rate {
            pcm = audio::resample(&pcm, rate);
        }
        let mut pcm = trim(pcm);
        let mut speed = 1.0;
        let start = match layout {
            Layout::Timeline => {
                let slot = slot_seconds(lines, i);
                let seconds = pcm.seconds();
                if seconds > slot + SLACK && slot > 0.3 {
                    speed = (seconds / slot).min(MAX_SPEED);
                    pcm.samples = audio::time_stretch(&pcm.samples, rate, speed);
                }
                lines[i].start.max(cursor)
            }
            Layout::Compact => {
                if placed.is_empty() {
                    PAUSE
                } else {
                    cursor + PAUSE - GAP
                }
            }
        };
        let w = match writer.as_mut() {
            Some(w) => w,
            None => writer.insert(TrackWriter::create(out, rate)?),
        };
        w.silence_until(start)?;
        w.samples(&pcm.samples)?;
        let seconds = pcm.seconds();
        placed.push(Placed {
            index: i,
            start,
            seconds,
            speed,
        });
        cursor = start + seconds + GAP;
    }
    let Some(mut w) = writer else {
        bail!("No line was spoken.");
    };
    match layout {
        Layout::Timeline => w.silence_until(min_seconds)?,
        Layout::Compact => w.silence_until(cursor - GAP + PAUSE)?,
    }
    w.finish()?;
    Ok(placed)
}

/// The video with the dubbed track as its sound, the picture copied as it is: `.mp4` for
/// MP4-family inputs, `.mkv` for the rest. Needs FFmpeg.
pub async fn mux(
    ffmpeg: &Path,
    video: &Path,
    dub: &Path,
    out_without_extension: &Path,
) -> Result<PathBuf> {
    let ext = audio::extension(video);
    let suffix = if ["mp4", "m4v", "mov", "3gp"].contains(&ext.as_str()) {
        "mp4"
    } else {
        "mkv"
    };
    let out = out_without_extension.with_extension(suffix);
    let mut cmd = crate::process::command(ffmpeg);
    cmd.args(["-y", "-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(video)
        .arg("-i")
        .arg(dub)
        .args([
            "-map", "0:v:0", "-map", "1:a:0", "-c:v", "copy", "-c:a", "aac", "-b:a", "160k",
        ])
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let child = crate::process::spawn_managed(&mut cmd)
        .with_context(|| format!("Could not start FFmpeg ({})", ffmpeg.display()))?;
    let output = match tokio::time::timeout(MUX_TIMEOUT, child.wait_with_output()).await {
        Ok(r) => r?,
        Err(_) => bail!("FFmpeg took over 30 minutes putting the video together and was stopped."),
    };
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr);
        let last = why.trim().lines().last().unwrap_or("").trim().to_string();
        bail!(
            "FFmpeg could not put the video together{}",
            if last.is_empty() {
                ".".to_string()
            } else {
                format!(": {last}")
            }
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: f64, end: f64) -> Segment {
        Segment::new(start, end, "x")
    }

    /// A spoken line: `seconds` of a tone with `lead` seconds of silence before and after.
    fn clip(dir: &Path, name: &str, rate: u32, seconds: f64, lead: f64) -> PathBuf {
        let mut samples = vec![0.0; (lead * rate as f64) as usize];
        samples.extend(
            (0..(seconds * rate as f64) as usize).map(|i| {
                (i as f64 / rate as f64 * 300.0 * std::f64::consts::TAU).sin() as f32 * 0.5
            }),
        );
        samples.extend(vec![0.0; (lead * rate as f64) as usize]);
        let path = dir.join(name);
        audio::write_wav(&path, &Pcm { samples, rate }).unwrap();
        path
    }

    #[test]
    fn lines_keep_their_place_speed_up_to_fit_and_move_when_they_must() {
        let dir = tempfile::tempdir().unwrap();
        let lines = vec![seg(1.0, 3.0), seg(3.0, 4.0), seg(4.0, 5.0), seg(9.0, 10.0)];
        let clips = vec![
            Some(clip(dir.path(), "a.wav", 24_000, 1.5, 0.2)),
            // 2 s for a 1 s slot: sped up 1.3 times, still 1.54 s, so the next line moves
            Some(clip(dir.path(), "b.wav", 24_000, 2.0, 0.0)),
            None,
            // another rate: resampled to the track's
            Some(clip(dir.path(), "d.wav", 48_000, 0.5, 0.0)),
        ];
        let out = dir.path().join("dub.wav");
        let placed = assemble(&lines, &clips, Layout::Timeline, 12.0, &out).unwrap();
        assert_eq!(placed.len(), 3);
        assert_eq!((placed[0].start, placed[0].speed), (1.0, 1.0));
        assert!(
            (placed[0].seconds - 1.58).abs() < 0.02,
            "trimmed to the tone and margins"
        );
        assert_eq!(placed[1].start, 3.0);
        assert_eq!(placed[1].speed, MAX_SPEED);
        assert!(
            (placed[1].seconds - 2.0 / 1.3).abs() < 0.05,
            "{}",
            placed[1].seconds
        );
        assert_eq!(placed[2].index, 3);
        assert_eq!(placed[2].start, 9.0);
        let track = audio::read_wav(&out).unwrap();
        assert_eq!(track.rate, 24_000);
        assert!(
            (track.seconds() - 12.0).abs() < 0.01,
            "padded to the original's length"
        );

        assert_eq!(slot_seconds(&lines, 0), 2.0);
        assert_eq!(
            slot_seconds(&lines, 3),
            1.0,
            "the last line: its own length"
        );
        assert!(assemble(
            &lines,
            &[None, None, None, None],
            Layout::Timeline,
            0.0,
            &out
        )
        .is_err());
    }

    #[test]
    fn a_recording_is_laid_out_line_after_line() {
        let dir = tempfile::tempdir().unwrap();
        let lines = vec![seg(4.0, 5.0), seg(30.0, 31.0)];
        let clips = vec![
            Some(clip(dir.path(), "a.wav", 24_000, 1.0, 0.5)),
            Some(clip(dir.path(), "b.wav", 24_000, 1.0, 0.5)),
        ];
        let out = dir.path().join("dub.wav");
        let placed = assemble(&lines, &clips, Layout::Compact, 60.0, &out).unwrap();
        assert_eq!(placed[0].start, PAUSE);
        let second = placed[0].start + placed[0].seconds + PAUSE;
        assert!((placed[1].start - second).abs() < 1e-9);
        let track = audio::read_wav(&out).unwrap();
        assert!(
            track.seconds() < 3.5,
            "no waiting for the original's silence: {}",
            track.seconds()
        );
    }
}

//! Sound in and out of the flows. Ports `flow/AudioPrep.java` and the WAV work of `flow/Dub.java`
//! and `flow/Reference.java`, with one difference: the original read WAV, AIFF and AU with Java
//! Sound and everything else through FFmpeg; here symphonia reads MP3, AAC (M4A, MP4, MOV), FLAC,
//! Vorbis, ALAC, WAV, AIFF, CAF and Matroska by itself, so FFmpeg is only needed for what it does
//! not read (Opus, WMA, AMR, AC-3...) and for putting a video back together.
//!
//! Tracks are converted streaming into the shape Whisper takes, 16 kHz mono 16-bit WAV, so an
//! hour-long recording never sits in memory whole; spoken lines are small and read whole
//! ([`Pcm`]).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tokio_util::sync::CancellationToken;

use super::Stopped;
use crate::speech::pipeline::{downmix, to_i16, Resampler};

/// What Whisper takes: 16 kHz, one channel, 16-bit.
pub const SPEECH_RATE: u32 = 16_000;

/// Seconds per part sent to the speech engine: seconds on a GPU, a minute or two on a processor,
/// well inside the engine's five-minute limit on one request.
pub const CHUNK_SECONDS: f64 = 300.0;

/// Video files, by extension: the flow gives these back with their picture and the new track.
const VIDEO: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "mpg", "mpeg", "wmv", "3gp", "ts",
];

/// Every file type the Open dialog offers: what symphonia reads, and what FFmpeg reads.
pub const MEDIA_EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "aac", "wav", "aiff", "aif", "flac", "ogg", "oga", "opus", "wma", "amr", "au",
    "caf", "mp4", "m4v", "mkv", "webm", "mov", "avi", "mpg", "mpeg", "wmv", "3gp", "ts",
];

/// Mono samples (-1..1) at a rate: a spoken line, a reference clip.
#[derive(Clone, Debug, PartialEq)]
pub struct Pcm {
    pub samples: Vec<f32>,
    pub rate: u32,
}

impl Pcm {
    pub fn seconds(&self) -> f64 {
        if self.rate == 0 {
            0.0
        } else {
            self.samples.len() as f64 / self.rate as f64
        }
    }
}

/// The file's extension in lower case, "" when it has none.
pub fn extension(file: &Path) -> String {
    file.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Whether the file is a video, by its name.
pub fn is_video(file: &Path) -> bool {
    VIDEO.contains(&extension(file).as_str())
}

// ------------------------------------------------------------------ reading any file

/// A file opened for its sound: the demuxer, the decoder for its first track Nook can decode, and
/// that track's id.
struct Opened {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track: u32,
}

fn open(file: &Path) -> Result<Opened> {
    let f = std::fs::File::open(file)
        .with_context(|| format!("Could not open {}", display_name(file)))?;
    let stream = MediaSourceStream::new(Box::new(f), Default::default());
    let mut hint = Hint::new();
    let ext = extension(file);
    if !ext.is_empty() {
        hint.with_extension(&ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| anyhow!("{e}"))?;
    let format = probed.format;
    let codecs = symphonia::default::get_codecs();
    let (track, decoder) = format
        .tracks()
        .iter()
        .filter(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .find_map(|t| {
            codecs
                .make(&t.codec_params, &DecoderOptions::default())
                .ok()
                .map(|d| (t.id, d))
        })
        .ok_or_else(|| anyhow!("no sound track Nook can decode"))?;
    Ok(Opened {
        format,
        decoder,
        track,
    })
}

/// Whether Nook reads the file's sound by itself (without FFmpeg): it opens and has a track Nook
/// can decode. Reads only the file's headers.
pub fn readable(file: &Path) -> bool {
    open(file).is_ok()
}

/// Decodes the sound of `file`, mixed down to one channel and resampled to `rate`, handing the
/// samples to `sink` as they come. Checks `cancel` between packets.
fn decode_into(
    file: &Path,
    rate: u32,
    cancel: &CancellationToken,
    mut sink: impl FnMut(f32) -> Result<()>,
) -> Result<()> {
    let Opened {
        mut format,
        mut decoder,
        track,
    } = open(file)?;
    let mut resampler: Option<Resampler> = None;
    let mut buffer: Option<SampleBuffer<f32>> = None;
    let mut failed: Option<anyhow::Error> = None;
    loop {
        if cancel.is_cancelled() {
            return Err(Stopped.into());
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break
            }
            // A chained stream changes its parameters: what came before is the track.
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(anyhow!("{e}")),
        };
        if packet.track_id() != track {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // One damaged packet is skipped, as players do.
            Err(SymphoniaError::DecodeError(e)) => {
                tracing::debug!("Skipping a damaged packet in {}: {e}", file.display());
                continue;
            }
            Err(SymphoniaError::IoError(_)) => break,
            Err(e) => return Err(anyhow!("{e}")),
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1);
        let needed = decoded.capacity() as u64;
        if buffer
            .as_ref()
            .is_none_or(|b| (b.capacity() as u64) < needed * channels as u64)
        {
            buffer = Some(SampleBuffer::new(needed, spec));
        }
        let Some(buf) = buffer.as_mut() else {
            continue;
        };
        buf.copy_interleaved_ref(decoded);
        let resampler = resampler.get_or_insert_with(|| Resampler::new(spec.rate, rate));
        for frame in buf.samples().chunks(channels) {
            resampler.push(downmix(frame), |y| {
                if failed.is_none() {
                    if let Err(e) = sink(y) {
                        failed = Some(e);
                    }
                }
            });
        }
        if let Some(e) = failed.take() {
            return Err(e);
        }
    }
    if resampler.is_none() {
        bail!("there is no sound in it");
    }
    Ok(())
}

/// Writes the sound of `input` to `out` as the 16 kHz mono 16-bit WAV Whisper takes, and returns
/// its length in seconds. Nook reads most files itself; the rest go through `ffmpeg` when it is
/// installed. Blocking: run it on the blocking pool.
pub fn to_speech_wav(
    input: &Path,
    out: &Path,
    ffmpeg: Option<&Path>,
    cancel: &CancellationToken,
) -> Result<f64> {
    if !input.is_file() {
        bail!("{} is not there any more.", display_name(input));
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Could not create {}", parent.display()))?;
    }
    let own = (|| -> Result<()> {
        let mut writer = hound::WavWriter::create(out, spec16(SPEECH_RATE))
            .with_context(|| format!("Could not write {}", out.display()))?;
        decode_into(input, SPEECH_RATE, cancel, |x| {
            writer.write_sample(to_i16(x)).map_err(Into::into)
        })?;
        writer.finalize()?;
        Ok(())
    })();
    match own {
        Ok(()) => {}
        Err(e) if e.is::<Stopped>() => return Err(e),
        Err(e) => {
            let _ = std::fs::remove_file(out);
            let Some(ffmpeg) = ffmpeg else {
                tracing::info!("Nook cannot read {} itself: {e:#}", input.display());
                bail!(
                    "Reading {} needs FFmpeg, which is not installed yet.",
                    display_name(input)
                );
            };
            tracing::info!("Reading {} with FFmpeg ({e:#})", display_name(input));
            if let Err(e) = convert_with_ffmpeg(ffmpeg, input, out, cancel) {
                let _ = std::fs::remove_file(out);
                return Err(e);
            }
        }
    }
    wav_seconds(out)
}

/// How long FFmpeg may take to read a file's sound: a feature-length film on a slow disk.
const FFMPEG_LIMIT: std::time::Duration = std::time::Duration::from_secs(2 * 3600);

/// FFmpeg's conversion to 16 kHz mono 16-bit WAV. FFmpeg is Nook's child in its kill-on-close
/// job, and when the run is stopped (or it runs past [`FFMPEG_LIMIT`]) it is killed and waited
/// for before this returns, so nothing is left writing to the run's files as they are removed.
fn convert_with_ffmpeg(
    ffmpeg: &Path,
    input: &Path,
    out: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut child = crate::process::std_command(ffmpeg)
        .args(["-y", "-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(input)
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-sample_fmt",
            "s16",
            "-f",
            "wav",
        ])
        .arg(out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("Could not start FFmpeg ({})", ffmpeg.display()))?;
    crate::process::adopt_std(&child);
    // What FFmpeg says, read on the side so a full pipe never stalls it.
    let reader = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut text = Vec::new();
            let _ = std::io::Read::read_to_end(&mut err, &mut text);
            text
        })
    });
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if cancel.is_cancelled() || started.elapsed() > FFMPEG_LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            // The reader ends when the pipe closes; one of FFmpeg's own children could hold it
            // open, so it is not waited for.
            drop(reader);
            if cancel.is_cancelled() {
                return Err(Stopped.into());
            }
            bail!(
                "FFmpeg took over two hours reading {} and was stopped.",
                display_name(input)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let stderr = reader.and_then(|r| r.join().ok()).unwrap_or_default();
    if !status.success() {
        let why = String::from_utf8_lossy(&stderr);
        let last = why.trim().lines().last().unwrap_or("").trim().to_string();
        bail!(
            "FFmpeg could not read {}{}",
            display_name(input),
            if last.is_empty() {
                format!(" (exit {}).", status.code().unwrap_or(-1))
            } else {
                format!(": {last}")
            }
        );
    }
    Ok(())
}

// ------------------------------------------------------------------ WAV files

fn spec16(rate: u32) -> hound::WavSpec {
    hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    }
}

/// Seconds of sound in a WAV, from its header.
pub fn wav_seconds(wav: &Path) -> Result<f64> {
    let r =
        hound::WavReader::open(wav).with_context(|| format!("Could not read {}", wav.display()))?;
    let spec = r.spec();
    Ok(r.duration() as f64 / spec.sample_rate.max(1) as f64)
}

/// A WAV of any sample format, mixed down to one channel.
pub fn read_wav(wav: &Path) -> Result<Pcm> {
    let mut r =
        hound::WavReader::open(wav).with_context(|| format!("Could not read {}", wav.display()))?;
    let spec = r.spec();
    let channels = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()?
        }
    };
    Ok(Pcm {
        samples: interleaved.chunks(channels).map(downmix).collect(),
        rate: spec.sample_rate,
    })
}

/// Writes mono samples as a 16-bit WAV.
pub fn write_wav(out: &Path, pcm: &Pcm) -> Result<()> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Could not create {}", parent.display()))?;
    }
    let mut w = hound::WavWriter::create(out, spec16(pcm.rate))
        .with_context(|| format!("Could not write {}", out.display()))?;
    for &x in &pcm.samples {
        w.write_sample(to_i16(x))?;
    }
    w.finalize()?;
    Ok(())
}

/// A 16-bit mono WAV written a piece at a time: the dubbed track, however long.
pub struct TrackWriter {
    writer: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
    rate: u32,
    written: u64,
}

impl TrackWriter {
    pub fn create(out: &Path, rate: u32) -> Result<TrackWriter> {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Could not create {}", parent.display()))?;
        }
        Ok(TrackWriter {
            writer: hound::WavWriter::create(out, spec16(rate))
                .with_context(|| format!("Could not write {}", out.display()))?,
            rate,
            written: 0,
        })
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Seconds written so far.
    pub fn seconds(&self) -> f64 {
        self.written as f64 / self.rate as f64
    }

    pub fn samples(&mut self, samples: &[f32]) -> Result<()> {
        for &x in samples {
            self.writer.write_sample(to_i16(x))?;
        }
        self.written += samples.len() as u64;
        Ok(())
    }

    /// Silence up to `seconds` into the track (nothing when it is there already).
    pub fn silence_until(&mut self, seconds: f64) -> Result<()> {
        let target = (seconds.max(0.0) * self.rate as f64).round() as u64;
        while self.written < target {
            self.writer.write_sample(0i16)?;
            self.written += 1;
        }
        Ok(())
    }

    pub fn finish(self) -> Result<()> {
        self.writer.finalize()?;
        Ok(())
    }
}

/// One part of a track: its WAV and where it starts in the whole.
#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    pub file: PathBuf,
    pub offset_seconds: f64,
}

/// Splits a 16-bit WAV into parts of at most `chunk_seconds`, written into `dir` as part-1.wav,
/// part-2.wav, …; a track that fits in one part is returned as it is.
pub fn split(wav: &Path, dir: &Path, chunk_seconds: f64) -> Result<Vec<Chunk>> {
    let mut r =
        hound::WavReader::open(wav).with_context(|| format!("Could not read {}", wav.display()))?;
    let spec = r.spec();
    let frames = r.duration() as u64;
    let per_chunk = ((chunk_seconds * spec.sample_rate as f64) as u64).max(1);
    if frames <= per_chunk {
        return Ok(vec![Chunk {
            file: wav.to_path_buf(),
            offset_seconds: 0.0,
        }]);
    }
    std::fs::create_dir_all(dir).with_context(|| format!("Could not create {}", dir.display()))?;
    let mut samples = r.samples::<i16>();
    let mut out = Vec::new();
    let mut at = 0u64;
    let mut n = 1;
    while at < frames {
        let take = per_chunk.min(frames - at);
        let part = dir.join(format!("part-{n}.wav"));
        let mut w = hound::WavWriter::create(&part, spec16(spec.sample_rate))
            .with_context(|| format!("Could not write {}", part.display()))?;
        for _ in 0..take * spec.channels as u64 {
            match samples.next() {
                Some(s) => w.write_sample(s?)?,
                None => break,
            }
        }
        w.finalize()?;
        out.push(Chunk {
            file: part,
            offset_seconds: at as f64 / spec.sample_rate as f64,
        });
        at += take;
        n += 1;
    }
    Ok(out)
}

/// The part of a mono WAV between `start` and `end` seconds.
pub fn cut_wav(wav: &Path, start: f64, end: f64) -> Result<Pcm> {
    let mut r =
        hound::WavReader::open(wav).with_context(|| format!("Could not read {}", wav.display()))?;
    let rate = r.spec().sample_rate;
    let frames = r.duration();
    let from = ((start.max(0.0) * rate as f64) as u32).min(frames);
    let to = ((end.max(0.0) * rate as f64) as u32).min(frames);
    if to <= from {
        bail!("The reference stretch is empty.");
    }
    r.seek(from)?;
    let samples = r
        .samples::<i16>()
        .take((to - from) as usize)
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Pcm { samples, rate })
}

// ------------------------------------------------------------------ shaping lines

/// The samples at another rate (low-passed first when going down).
pub fn resample(pcm: &Pcm, rate: u32) -> Pcm {
    if pcm.rate == rate || pcm.samples.is_empty() {
        return Pcm {
            samples: pcm.samples.clone(),
            rate,
        };
    }
    let mut r = Resampler::new(pcm.rate, rate);
    let mut out =
        Vec::with_capacity(pcm.samples.len() * rate as usize / pcm.rate.max(1) as usize + 1);
    for &x in &pcm.samples {
        r.push(x, |y| out.push(y));
    }
    Pcm { samples: out, rate }
}

/// Speeds speech up by `speed` (above 1) and keeps its pitch, as FFmpeg's `atempo` did for the
/// original: WSOLA, which overlap-adds 30 ms Hann windows taken from where the input should be,
/// each nudged by up to 10 ms to line up with the one before, so voices do not warble.
pub fn time_stretch(samples: &[f32], rate: u32, speed: f64) -> Vec<f32> {
    let win = ((rate as f64 * 0.030) as usize).max(16) & !1;
    if speed <= 1.001 || samples.len() < win * 4 {
        return samples.to_vec();
    }
    let hop_out = win / 2;
    let hop_in = hop_out as f64 * speed;
    let tolerance = (rate as f64 * 0.010) as usize;
    let window: Vec<f32> = (0..win)
        .map(|i| {
            let x = std::f64::consts::PI * 2.0 * i as f64 / win as f64;
            (0.5 - 0.5 * x.cos()) as f32
        })
        .collect();
    let frames = ((samples.len() - win) as f64 / hop_in) as usize + 1;
    let mut out = vec![0f32; frames * hop_out + win];
    let mut weight = vec![0f32; out.len()];
    let mut prev: usize = 0;
    for k in 0..frames {
        let pos = if k == 0 {
            0
        } else {
            // Where the last window would have gone on: what the next one should match.
            let natural = prev + hop_out;
            let nominal = (k as f64 * hop_in) as usize;
            let lo = nominal.saturating_sub(tolerance);
            let hi = (nominal + tolerance).min(samples.len() - win);
            let overlap = win - hop_out;
            let mut best = nominal.min(hi);
            let mut best_score = f32::MIN;
            if natural + overlap <= samples.len() {
                let target = &samples[natural..natural + overlap];
                for cand in lo..=hi {
                    let score: f32 = samples[cand..cand + overlap]
                        .iter()
                        .zip(target)
                        .map(|(a, b)| a * b)
                        .sum();
                    if score > best_score {
                        best_score = score;
                        best = cand;
                    }
                }
            }
            best
        };
        if pos + win > samples.len() {
            break;
        }
        let at = k * hop_out;
        for i in 0..win {
            out[at + i] += samples[pos + i] * window[i];
            weight[at + i] += window[i];
        }
        prev = pos;
    }
    let len = weight.iter().rposition(|w| *w > 1e-3).map_or(0, |i| i + 1);
    out.truncate(len);
    for (x, w) in out.iter_mut().zip(&weight) {
        if *w > 1e-3 {
            *x /= w;
        }
    }
    out
}

/// Above this typical pitch a speaker is taken for a woman, for a preset voice.
pub const FEMALE_ABOVE_HZ: f64 = 165.0;

/// The speaker's typical pitch in Hz: the median over the voiced 40 ms frames, each found by
/// autocorrelation between 70 and 400 Hz; None when too little is voiced to tell. Meant for a
/// reference clip of a few seconds.
pub fn pitch_hz(pcm: &Pcm) -> Option<f64> {
    let rate = pcm.rate as usize;
    let frame = rate * 40 / 1000;
    let (min_lag, max_lag) = (rate / 400, rate / 70);
    if frame == 0 || min_lag == 0 {
        return None;
    }
    let x = &pcm.samples;
    let mut pitches = Vec::new();
    let mut at = 0;
    while at + frame + max_lag <= x.len() {
        let a = &x[at..at + frame];
        let energy: f32 = a.iter().map(|v| v * v).sum();
        if (energy / frame as f32).sqrt() >= 0.02 {
            let r: Vec<f32> = (min_lag..=max_lag)
                .map(|lag| {
                    let b = &x[at + lag..at + lag + frame];
                    let dot: f32 = a.iter().zip(b).map(|(p, q)| p * q).sum();
                    let eb: f32 = b.iter().map(|v| v * v).sum();
                    dot / (energy * eb).sqrt().max(1e-9)
                })
                .collect();
            let best = r.iter().copied().fold(f32::MIN, f32::max);
            // The shortest period that correlates nearly as well: not a multiple of the true one.
            if best > 0.5 {
                if let Some(mut i) = r.iter().position(|v| *v >= best * 0.9) {
                    // up to that peak's top, then between samples by a parabola through it
                    while i + 1 < r.len() && r[i + 1] > r[i] {
                        i += 1;
                    }
                    let mut lag = (min_lag + i) as f64;
                    if i > 0 && i + 1 < r.len() {
                        let (a, b, c) = (r[i - 1] as f64, r[i] as f64, r[i + 1] as f64);
                        let bend = a - 2.0 * b + c;
                        if bend.abs() > 1e-12 {
                            lag += 0.5 * (a - c) / bend;
                        }
                    }
                    pitches.push(rate as f64 / lag);
                }
            }
        }
        at += frame;
    }
    if pitches.len() < 5 {
        return None;
    }
    pitches.sort_by(|a, b| a.total_cmp(b));
    Some(pitches[pitches.len() / 2])
}

/// A file's name for messages.
pub fn display_name(file: &Path) -> String {
    file.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.display().to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A tone as a WAV: `channels` channels at `rate`, `seconds` long.
    pub(crate) fn write_tone(out: &Path, rate: u32, channels: u16, seconds: f64, hz: f64) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(out, spec).unwrap();
        let frames = (rate as f64 * seconds) as usize;
        for i in 0..frames {
            let v = (i as f64 / rate as f64 * hz * std::f64::consts::TAU).sin() * 0.5;
            for _ in 0..channels {
                w.write_sample((v * 32767.0) as i16).unwrap();
            }
        }
        w.finalize().unwrap();
    }

    fn zero_crossings(x: &[f32]) -> usize {
        x.windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count()
    }

    #[test]
    fn a_stereo_44k_wav_becomes_16k_mono_and_parts() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.wav");
        write_tone(&input, 44_100, 2, 2.5, 440.0);
        assert!(readable(&input));
        let wav = dir.path().join("audio.wav");
        let seconds = to_speech_wav(&input, &wav, None, &CancellationToken::new()).unwrap();
        assert!((seconds - 2.5).abs() < 0.01, "{seconds}");
        let spec = hound::WavReader::open(&wav).unwrap().spec();
        assert_eq!(
            (spec.sample_rate, spec.channels, spec.bits_per_sample),
            (16_000, 1, 16)
        );
        let pcm = read_wav(&wav).unwrap();
        // 440 Hz: about 880 crossings a second, whatever the rate.
        let per_second = zero_crossings(&pcm.samples) as f64 / pcm.seconds();
        assert!((per_second - 880.0).abs() < 20.0, "{per_second}");

        let parts = split(&wav, &dir.path().join("parts"), 1.0).unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[2].offset_seconds, 2.0);
        assert!((wav_seconds(&parts[2].file).unwrap() - 0.5).abs() < 0.01);
        assert_eq!(
            split(&wav, dir.path(), 10.0).unwrap()[0].file,
            wav,
            "fits in one"
        );

        let cut = cut_wav(&wav, 1.0, 1.5).unwrap();
        assert_eq!(cut.samples.len(), 8000);
        assert!(cut_wav(&wav, 3.0, 4.0).is_err());
    }

    #[test]
    fn what_nook_cannot_read_needs_ffmpeg() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("talk.opus");
        std::fs::write(&input, b"OggS not really").unwrap();
        assert!(!readable(&input));
        let err = to_speech_wav(
            &input,
            &dir.path().join("a.wav"),
            None,
            &CancellationToken::new(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            err,
            "Reading talk.opus needs FFmpeg, which is not installed yet."
        );
        let err = to_speech_wav(
            &dir.path().join("gone.mp3"),
            &dir.path().join("a.wav"),
            None,
            &CancellationToken::new(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(err, "gone.mp3 is not there any more.");
        assert!(is_video(Path::new("a.MKV")) && !is_video(Path::new("a.m4a")));
    }

    #[test]
    fn stretching_shortens_and_keeps_the_pitch() {
        let rate = 24_000;
        let tone: Vec<f32> = (0..rate * 2)
            .map(|i| (i as f64 / rate as f64 * 220.0 * std::f64::consts::TAU).sin() as f32 * 0.5)
            .collect();
        let fast = time_stretch(&tone, rate, 1.25);
        let ratio = tone.len() as f64 / fast.len() as f64;
        assert!((ratio - 1.25).abs() < 0.03, "{ratio}");
        let before = zero_crossings(&tone) as f64 / tone.len() as f64;
        let after = zero_crossings(&fast) as f64 / fast.len() as f64;
        assert!(
            (after / before - 1.0).abs() < 0.03,
            "pitch kept: {before} {after}"
        );
        assert_eq!(time_stretch(&tone, rate, 1.0), tone);

        let pcm = Pcm {
            samples: tone.clone(),
            rate,
        };
        let down = resample(&pcm, 16_000);
        assert_eq!(down.rate, 16_000);
        assert!((down.seconds() - 2.0).abs() < 0.01);
    }

    /// Stopping a run stops an FFmpeg that is still reading, at once, not when it finishes.
    #[cfg(windows)]
    #[test]
    fn a_stopped_run_stops_ffmpeg() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("talk.opus");
        std::fs::write(&input, b"OggS not really").unwrap();
        // An "FFmpeg" that takes half a minute over anything.
        let slow = dir.path().join("ffmpeg.cmd");
        std::fs::write(&slow, "@ping -n 30 127.0.0.1 >nul\r\n").unwrap();
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            stop.cancel();
        });
        let started = std::time::Instant::now();
        let out = dir.path().join("a.wav");
        let err = to_speech_wav(&input, &out, Some(&slow), &cancel).unwrap_err();
        assert!(err.is::<Stopped>(), "{err:#}");
        assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
        assert!(!out.exists());
    }

    #[test]
    fn the_pitch_tells_a_high_voice_from_a_low_one() {
        let tone = |hz: f64| Pcm {
            samples: (0..16_000)
                .map(|i| {
                    let t = i as f64 / 16_000.0 * hz * std::f64::consts::TAU;
                    // a voice-like tone: the fundamental and two harmonics
                    (0.3 * t.sin() + 0.2 * (2.0 * t).sin() + 0.1 * (3.0 * t).sin()) as f32
                })
                .collect(),
            rate: 16_000,
        };
        let high = pitch_hz(&tone(220.0)).unwrap();
        assert!((high - 220.0).abs() < 8.0, "{high}");
        let low = pitch_hz(&tone(110.0)).unwrap();
        assert!((low - 110.0).abs() < 4.0, "{low}");
        assert!(high > FEMALE_ABOVE_HZ && low < FEMALE_ABOVE_HZ);
        let quiet = Pcm {
            samples: vec![0.0; 16_000],
            rate: 16_000,
        };
        assert_eq!(pitch_hz(&quiet), None);
    }
}

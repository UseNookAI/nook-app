//! What happens to the microphone's samples on their way to the WAV: down to one channel, to
//! 16 kHz, to 16-bit, with a level for the waveform every 40 ms. No device here, so all of it is
//! tested with made-up signals.
//!
//! The original asked Java Sound for 16 kHz mono 16-bit first and took 44.1 or 48 kHz when the
//! line would not do 16 kHz, writing whatever it got; whisper-server takes 16 kHz mono WAV, so this
//! port always writes that and converts here instead (Windows' shared-mode microphone is almost
//! always 44.1 or 48 kHz float, often stereo).

use std::f64::consts::PI;
use std::path::Path;

use anyhow::{Context, Result};

/// What whisper-server takes: 16 kHz, mono, 16-bit PCM.
pub const SAMPLE_RATE: u32 = 16_000;
/// A level every 640 samples of 16 kHz audio: 40 ms, 25 levels a second (the original's 1024-byte
/// reads at 16 kHz gave about 30).
pub const LEVEL_EVERY: usize = 640;
/// The low-pass filter's cutoff before decimating to 16 kHz, a little under its 8 kHz Nyquist
/// limit: speech keeps everything whisper listens to, and nothing above folds back into it.
const CUTOFF_HZ: f64 = 7_200.0;

/// One frame of interleaved samples as one: the channels' average.
pub fn downmix(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    frame.iter().sum::<f32>() / frame.len() as f32
}

/// Interleaved samples as mono, appended to `out`.
pub fn downmix_into(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    let channels = channels.max(1);
    out.extend(interleaved.chunks(channels).map(downmix));
}

/// A float sample (-1..1) as 16-bit PCM, clipped.
pub fn to_i16(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// A windowed-sinc low-pass FIR, fed one sample at a time.
struct LowPass {
    taps: Vec<f32>,
    history: Vec<f32>,
    pos: usize,
}

impl LowPass {
    /// `cutoff` as a fraction of the sample rate (0..0.5).
    fn new(cutoff: f64, len: usize) -> LowPass {
        let len = len | 1;
        let mid = (len / 2) as f64;
        let mut taps: Vec<f64> = (0..len)
            .map(|i| {
                let n = i as f64 - mid;
                let sinc = if n == 0.0 {
                    2.0 * cutoff
                } else {
                    (2.0 * PI * cutoff * n).sin() / (PI * n)
                };
                // Blackman window: -74 dB side lobes
                let w = 0.42 - 0.5 * (2.0 * PI * i as f64 / (len - 1) as f64).cos()
                    + 0.08 * (4.0 * PI * i as f64 / (len - 1) as f64).cos();
                sinc * w
            })
            .collect();
        let gain: f64 = taps.iter().sum();
        taps.iter_mut().for_each(|t| *t /= gain);
        LowPass {
            taps: taps.into_iter().map(|t| t as f32).collect(),
            // every sample is kept twice, so the last `len` are always one contiguous slice
            history: vec![0.0; 2 * len],
            pos: 0,
        }
    }

    fn push(&mut self, x: f32) -> f32 {
        let len = self.taps.len();
        self.history[self.pos] = x;
        self.history[self.pos + len] = x;
        // the last `len` samples, oldest first; the taps are symmetric, so either order will do
        let window = &self.history[self.pos + 1..self.pos + 1 + len];
        let acc = self.taps.iter().zip(window).map(|(t, s)| t * s).sum();
        self.pos = (self.pos + 1) % len;
        acc
    }
}

/// Converts a stream of mono samples from one rate to another: low-pass first when going down,
/// then linear interpolation between neighbouring samples. Keeps its place between calls, so the
/// stream can arrive in pieces of any size.
pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Where the next output falls, in input samples since the start.
    next: f64,
    /// The index of the next input sample.
    index: u64,
    prev: f32,
    filter: Option<LowPass>,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Resampler {
        let (from, to) = (from.max(1), to.max(1));
        let filter = (from > to).then(|| {
            let ratio = (from as f64 / to as f64).ceil() as usize;
            LowPass::new(
                CUTOFF_HZ * to as f64 / SAMPLE_RATE as f64 / from as f64,
                32 * ratio + 1,
            )
        });
        Resampler {
            step: from as f64 / to as f64,
            next: 0.0,
            index: 0,
            prev: 0.0,
            filter,
        }
    }

    /// Feeds one input sample; calls `out` for each output sample it completes.
    pub fn push(&mut self, x: f32, mut out: impl FnMut(f32)) {
        let x = match &mut self.filter {
            Some(f) => f.push(x),
            None => x,
        };
        let n = self.index as f64;
        // outputs that fall between the previous input (n - 1) and this one (n)
        while self.next <= n {
            let t = (self.next - (n - 1.0)) as f32;
            // on an input sample exactly (always, at the same rate), that sample as it is
            out(if t >= 1.0 {
                x
            } else {
                self.prev + (x - self.prev) * t
            });
            self.next += self.step;
        }
        self.prev = x;
        self.index += 1;
    }
}

/// Mono samples in, 16 kHz 16-bit samples and levels out.
pub struct Pipeline {
    resampler: Resampler,
    samples: Vec<i16>,
    peak: i32,
    counted: usize,
    on_level: Box<dyn FnMut(f32) + Send>,
}

impl Pipeline {
    /// `on_level` gets the loudness of every [`LEVEL_EVERY`] samples: the peak, 0 to 1.
    pub fn new(source_rate: u32, on_level: impl FnMut(f32) + Send + 'static) -> Pipeline {
        Pipeline {
            resampler: Resampler::new(source_rate, SAMPLE_RATE),
            samples: Vec::new(),
            peak: 0,
            counted: 0,
            on_level: Box::new(on_level),
        }
    }

    pub fn push_mono(&mut self, samples: &[f32]) {
        let Pipeline {
            resampler,
            samples: out,
            peak,
            counted,
            on_level,
        } = self;
        for &x in samples {
            resampler.push(x, |y| {
                let s = to_i16(y);
                out.push(s);
                *peak = (*peak).max((s as i32).abs());
                *counted += 1;
                if *counted == LEVEL_EVERY {
                    // normalize the 16-bit peak (at most 32767) to 0..1
                    on_level(*peak as f32 / 32767.0);
                    *peak = 0;
                    *counted = 0;
                }
            });
        }
    }

    pub fn samples(&self) -> &[i16] {
        &self.samples
    }

    pub fn into_samples(self) -> Vec<i16> {
        self.samples
    }
}

/// Writes 16 kHz mono 16-bit PCM as a WAV file (a plain 44-byte header, as Java Sound wrote).
pub fn write_wav(path: &Path, samples: &[i16]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .with_context(|| format!("could not write {}", path.display()))?;
    let mut w = writer.get_i16_writer(samples.len() as u32);
    for &s in samples {
        w.write_sample(s);
    }
    w.flush()
        .with_context(|| format!("could not write {}", path.display()))?;
    writer
        .finalize()
        .with_context(|| format!("could not write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn sine(rate: u32, hz: f64, amplitude: f32, seconds: f64) -> Vec<f32> {
        let n = (rate as f64 * seconds) as usize;
        (0..n)
            .map(|i| amplitude * (2.0 * PI * hz * i as f64 / rate as f64).sin() as f32)
            .collect()
    }

    fn resample(from: u32, input: &[f32]) -> Vec<f32> {
        let mut r = Resampler::new(from, SAMPLE_RATE);
        let mut out = Vec::new();
        for &x in input {
            r.push(x, |y| out.push(y));
        }
        out
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    /// Sign changes per second: twice the frequency of a clean tone.
    fn crossings_per_second(x: &[f32], rate: u32) -> f64 {
        let changes = x
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        changes as f64 / (x.len() as f64 / rate as f64)
    }

    #[test]
    fn channels_are_averaged_into_one() {
        assert_eq!(downmix(&[1.0, 0.0]), 0.5);
        assert_eq!(downmix(&[0.25]), 0.25);
        assert_eq!(downmix(&[]), 0.0);
        let mut mono = Vec::new();
        downmix_into(&[1.0, -1.0, 0.5, 0.5, 0.2, 0.0], 2, &mut mono);
        assert_eq!(mono, vec![0.0, 0.5, 0.1]);
    }

    #[test]
    fn floats_become_clipped_16_bit() {
        assert_eq!(to_i16(0.0), 0);
        assert_eq!(to_i16(1.0), 32767);
        assert_eq!(to_i16(1.5), 32767);
        assert_eq!(to_i16(-1.5), -32767);
        assert_eq!(to_i16(0.5), 16384);
    }

    #[test]
    fn sixteen_khz_passes_through_untouched() {
        let input = sine(16_000, 440.0, 0.5, 0.1);
        assert_eq!(resample(16_000, &input), input);
    }

    #[test]
    fn forty_eight_khz_keeps_a_voice_band_tone() {
        let input = sine(48_000, 440.0, 0.5, 1.0);
        let out = resample(48_000, &input);
        assert!((out.len() as i64 - 16_000).abs() <= 1, "{}", out.len());
        // past the filter's warm-up the tone is all there, at its own pitch
        let steady = &out[400..];
        assert!(
            (rms(steady) - 0.5 / 2f32.sqrt()).abs() < 0.01,
            "{}",
            rms(steady)
        );
        let hz = crossings_per_second(steady, SAMPLE_RATE) / 2.0;
        assert!((hz - 440.0).abs() < 3.0, "{hz}");
    }

    #[test]
    fn forty_four_one_comes_out_at_sixteen_thousand_a_second() {
        let out = resample(44_100, &sine(44_100, 1_000.0, 0.5, 2.0));
        assert!((out.len() as i64 - 32_000).abs() <= 1, "{}", out.len());
        let hz = crossings_per_second(&out[400..], SAMPLE_RATE) / 2.0;
        assert!((hz - 1_000.0).abs() < 5.0, "{hz}");
    }

    #[test]
    fn what_16_khz_cannot_hold_does_not_fold_back_into_the_voice_band() {
        // 12 kHz would alias to 4 kHz in plain decimation
        let out = resample(48_000, &sine(48_000, 12_000.0, 0.5, 1.0));
        assert!(rms(&out[400..]) < 0.005, "{}", rms(&out[400..]));
    }

    #[test]
    fn eight_khz_is_stretched_to_sixteen() {
        let out = resample(8_000, &sine(8_000, 300.0, 0.5, 1.0));
        assert!((out.len() as i64 - 16_000).abs() <= 2, "{}", out.len());
        let hz = crossings_per_second(&out, SAMPLE_RATE) / 2.0;
        assert!((hz - 300.0).abs() < 3.0, "{hz}");
    }

    #[test]
    fn pieces_of_any_size_give_the_same_stream() {
        let input = sine(44_100, 700.0, 0.8, 0.5);
        let whole = resample(44_100, &input);
        let mut r = Resampler::new(44_100, SAMPLE_RATE);
        let mut pieces = Vec::new();
        for chunk in input.chunks(37) {
            for &x in chunk {
                r.push(x, |y| pieces.push(y));
            }
        }
        assert_eq!(whole, pieces);
    }

    #[test]
    fn levels_come_25_times_a_second_as_the_peak() {
        let levels = Arc::new(Mutex::new(Vec::new()));
        let sink = levels.clone();
        let mut p = Pipeline::new(48_000, move |l| sink.lock().unwrap().push(l));
        let mut mono = Vec::new();
        downmix_into(
            &sine(48_000, 440.0, 0.5, 1.0)
                .iter()
                .flat_map(|&x| [x, x])
                .collect::<Vec<_>>(),
            2,
            &mut mono,
        );
        for chunk in mono.chunks(480) {
            p.push_mono(chunk);
        }
        let levels = levels.lock().unwrap().clone();
        assert_eq!(levels.len(), 25);
        assert!(
            levels[5..].iter().all(|l| (l - 0.5).abs() < 0.02),
            "{levels:?}"
        );
        assert_eq!(p.samples().len(), 16_000);
    }

    #[test]
    fn silence_is_level_zero() {
        let levels = Arc::new(Mutex::new(Vec::new()));
        let sink = levels.clone();
        let mut p = Pipeline::new(16_000, move |l| sink.lock().unwrap().push(l));
        p.push_mono(&vec![0.0; 1_280]);
        assert_eq!(*levels.lock().unwrap(), vec![0.0, 0.0]);
    }

    #[test]
    fn the_wav_is_16_khz_mono_16_bit_pcm_with_a_plain_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nook_prompt_test.wav");
        let samples: Vec<i16> = (0..1_600).map(|i| ((i % 200) as i16 - 100) * 300).collect();
        write_wav(&path, &samples).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            bytes.len(),
            44 + samples.len() * 2,
            "a 44-byte header and the samples"
        );
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let u32_at =
            |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        assert_eq!(u32_at(4) as usize, bytes.len() - 8);
        assert_eq!(u32_at(16), 16, "a plain PCM fmt chunk");
        assert_eq!(u16_at(20), 1, "PCM");
        assert_eq!(u16_at(22), 1, "mono");
        assert_eq!(u32_at(24), 16_000);
        assert_eq!(u32_at(28), 32_000, "bytes a second");
        assert_eq!(u16_at(32), 2, "block align");
        assert_eq!(u16_at(34), 16, "bits");
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(u32_at(40) as usize, samples.len() * 2);

        let mut reader = hound::WavReader::open(&path).unwrap();
        let back: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(back, samples);
    }

    #[test]
    fn an_empty_recording_is_still_a_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.wav");
        write_wav(&path, &[]).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 44);
        assert_eq!(
            hound::WavReader::open(&path).unwrap().spec().sample_rate,
            16_000
        );
    }
}

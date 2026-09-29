//! The sound a recording takes: a microphone, what the computer plays (Windows' loopback of an
//! output device), or both, mixed into one 48 kHz stereo stream of 32-bit floats for FFmpeg.
//!
//! cpal streams are not `Send` on Windows, so each source runs on a thread of its own that opens
//! its stream and keeps it until the mixer stops. Their callbacks only hand the device's samples
//! over, as stereo, at the device's own rate. The mixer's thread wakes every 10 ms, brings each
//! source to 48 kHz, and writes as much sound as the wall clock says is due: a source that
//! delivered less (Windows sends nothing from an output device while nothing plays) is silent
//! for the rest, and one that ran ahead is trimmed, so the sound keeps time with the picture.
//! It also reports how loud each source is, ten times a second, for the page's meters.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// The rate the mixed sound is written at.
pub const RATE: u32 = 48_000;
/// How long a device may take to open.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the mixer writes.
const TICK: Duration = Duration::from_millis(10);
/// The most sound a source may run ahead before its oldest is dropped.
const MOST_AHEAD: usize = (RATE as usize / 4) * 2;

/// A sound device, by the name Windows gives it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDevice {
    pub name: String,
    pub default: bool,
}

/// A sound to take; `device` None is Windows' default one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AudioSource {
    /// A microphone (an input device).
    Microphone { device: Option<String> },
    /// What the computer plays through an output device.
    System { device: Option<String> },
}

impl AudioSource {
    fn describe(&self) -> String {
        match self {
            AudioSource::Microphone { device } => format!(
                "The microphone{}",
                device
                    .as_ref()
                    .map(|d| format!(" \"{d}\""))
                    .unwrap_or_default()
            ),
            AudioSource::System { device } => format!(
                "The computer's sound{}",
                device
                    .as_ref()
                    .map(|d| format!(" from \"{d}\""))
                    .unwrap_or_default()
            ),
        }
    }
}

/// The microphones, Windows' default first.
pub fn microphones() -> Vec<AudioDevice> {
    let host = cpal::default_host();
    let default = host.default_input_device().and_then(|d| d.name().ok());
    listed(host.input_devices().ok(), default)
}

/// The output devices whose sound can be taken, Windows' default first.
pub fn speakers() -> Vec<AudioDevice> {
    let host = cpal::default_host();
    let default = host.default_output_device().and_then(|d| d.name().ok());
    listed(host.output_devices().ok(), default)
}

fn listed(
    devices: Option<impl Iterator<Item = cpal::Device>>,
    default: Option<String>,
) -> Vec<AudioDevice> {
    let mut out: Vec<AudioDevice> = devices
        .into_iter()
        .flatten()
        .filter_map(|d| d.name().ok())
        .map(|name| AudioDevice {
            default: default.as_deref() == Some(name.as_str()),
            name,
        })
        .collect();
    out.sort_by_key(|d| !d.default);
    out.dedup_by(|a, b| a.name == b.name);
    out
}

/// Where the mixed sound goes: chunks of interleaved stereo floats. Full, a chunk is dropped
/// rather than hold the mixer up.
pub type Sink = tokio::sync::mpsc::Sender<Vec<f32>>;

/// What a source's callback has handed over and not yet mixed: stereo frames at its rate.
struct Delivered {
    rate: u32,
    samples: Vec<f32>,
}

/// The sources' sound, mixed: running until stopped, into the sink attached (none at first: then
/// it only measures).
pub struct Mixer {
    stop: Arc<AtomicBool>,
    sink: Arc<Mutex<Option<Sink>>>,
    /// Set by `attach`: the mixer starts counting what is due from then.
    restart: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Mixer {
    /// Opens `sources` and starts mixing. `levels` hears each source's loudness (0 to 1, in the
    /// order given) ten times a second. Fails, naming it, when a source does not open.
    pub fn start(
        sources: &[AudioSource],
        levels: impl Fn(&[f32]) + Send + 'static,
    ) -> Result<Mixer> {
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        let mut delivered = Vec::new();
        for source in sources {
            let inbox = Arc::new(Mutex::new(Delivered {
                rate: RATE,
                samples: Vec::new(),
            }));
            let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
            let (s, i, st) = (source.clone(), inbox.clone(), stop.clone());
            let thread = std::thread::Builder::new()
                .name("nook-capture-sound".into())
                .spawn(move || run_source(s, i, st, ready_tx))
                .context("Could not start a sound thread")?;
            threads.push(thread);
            let opened = ready_rx.recv_timeout(OPEN_TIMEOUT);
            if !matches!(opened, Ok(Ok(()))) {
                stop.store(true, Ordering::SeqCst);
                for t in threads {
                    let _ = t.join();
                }
                let why = match opened {
                    Ok(Err(why)) => why,
                    _ => format!("it did not open within {} seconds", OPEN_TIMEOUT.as_secs()),
                };
                bail!("{} could not be opened: {why}", source.describe());
            }
            delivered.push(inbox);
        }
        let sink: Arc<Mutex<Option<Sink>>> = Arc::new(Mutex::new(None));
        let restart = Arc::new(AtomicBool::new(true));
        let (st, sk, rs) = (stop.clone(), sink.clone(), restart.clone());
        threads.push(
            std::thread::Builder::new()
                .name("nook-capture-mixer".into())
                .spawn(move || mix(delivered, sk, rs, st, levels))
                .context("Could not start the sound mixer")?,
        );
        Ok(Mixer {
            stop,
            sink,
            restart,
            threads,
        })
    }

    /// Sends the mixed sound to `sink` from now on.
    pub fn attach(&self, sink: Sink) {
        self.attacher().attach(sink);
    }

    /// A way to attach a sink from elsewhere: the task that waits for FFmpeg to open its pipe.
    pub fn attacher(&self) -> Attacher {
        Attacher {
            sink: self.sink.clone(),
            restart: self.restart.clone(),
        }
    }

    /// Stops sending the sound anywhere (dropping the sink closes it).
    pub fn detach(&self) {
        self.sink.lock().take();
    }

    /// Closes the sources and the sink.
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.sink.lock().take();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for Mixer {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Attaches a sink to a [`Mixer`]: the sound due is counted from then.
#[derive(Clone)]
pub struct Attacher {
    sink: Arc<Mutex<Option<Sink>>>,
    restart: Arc<AtomicBool>,
}

impl Attacher {
    pub fn attach(&self, sink: Sink) {
        *self.sink.lock() = Some(sink);
        self.restart.store(true, Ordering::SeqCst);
    }
}

/// A source's thread: opens the device, keeps its stream until the mixer stops.
fn run_source(
    source: AudioSource,
    inbox: Arc<Mutex<Delivered>>,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(), String>>,
) {
    let stream = match open(&source, inbox) {
        Ok(stream) => stream,
        Err(e) => {
            let _ = ready.send(Err(format!("{e:#}")));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(stream);
}

fn open(source: &AudioSource, inbox: Arc<Mutex<Delivered>>) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let named = |mut devices: Box<dyn Iterator<Item = cpal::Device>>, name: &str| {
        devices.find(|d| d.name().is_ok_and(|n| n == name))
    };
    let (device, config) = match source {
        AudioSource::Microphone { device } => {
            let d = match device {
                Some(name) => named(Box::new(host.input_devices()?), name)
                    .ok_or_else(|| anyhow!("there is no microphone \"{name}\""))?,
                None => host
                    .default_input_device()
                    .ok_or_else(|| anyhow!("there is no microphone"))?,
            };
            let c = d
                .default_input_config()
                .context("it has no usable format")?;
            (d, c)
        }
        AudioSource::System { device } => {
            let d = match device {
                Some(name) => named(Box::new(host.output_devices()?), name)
                    .ok_or_else(|| anyhow!("there is no output device \"{name}\""))?,
                None => host
                    .default_output_device()
                    .ok_or_else(|| anyhow!("there is no output device"))?,
            };
            let c = d
                .default_output_config()
                .context("it has no usable format")?;
            (d, c)
        }
    };
    let format = config.sample_format();
    let config: cpal::StreamConfig = config.into();
    inbox.lock().rate = config.sample_rate.0;
    tracing::info!(
        "Recording sound from {} at {} Hz, {} channel(s), {format:?}",
        device.name().unwrap_or_default(),
        config.sample_rate.0,
        config.channels
    );
    use cpal::SampleFormat as F;
    // On an output device Windows records what it plays (loopback).
    let stream = match format {
        F::F32 => build::<f32>(&device, &config, inbox),
        F::I16 => build::<i16>(&device, &config, inbox),
        F::U16 => build::<u16>(&device, &config, inbox),
        F::I32 => build::<i32>(&device, &config, inbox),
        F::U32 => build::<u32>(&device, &config, inbox),
        F::I8 => build::<i8>(&device, &config, inbox),
        F::U8 => build::<u8>(&device, &config, inbox),
        F::F64 => build::<f64>(&device, &config, inbox),
        other => bail!("its sample format {other:?} is not supported"),
    }?;
    stream.play().context("it did not start")?;
    Ok(stream)
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    inbox: Arc<Mutex<Delivered>>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = config.channels as usize;
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                // As little as possible on the audio thread: stereo floats, handed over.
                let mut inbox = inbox.lock();
                for frame in data.chunks(channels.max(1)) {
                    let l = <f32 as cpal::Sample>::from_sample(frame[0]);
                    let r = frame
                        .get(1)
                        .map_or(l, |&s| <f32 as cpal::Sample>::from_sample(s));
                    inbox.samples.push(l);
                    inbox.samples.push(r);
                }
            },
            |e| tracing::warn!("A sound device failed while recording: {e}"),
            None,
        )
        .context("its stream could not be opened")
}

/// Brings stereo frames from one rate to [`RATE`], by straight lines between them.
#[derive(Default)]
pub struct Resampler {
    /// Where the next frame out falls, in frames after `last`.
    at: f64,
    last: [f32; 2],
}

impl Resampler {
    /// Appends `input` (stereo frames at `from` Hz), brought to [`RATE`], to `out`.
    pub fn push(&mut self, from: u32, input: &[f32], out: &mut VecDeque<f32>) {
        if from == RATE {
            out.extend(input);
            return;
        }
        let frames = input.len() / 2;
        if frames == 0 {
            return;
        }
        let step = f64::from(from) / f64::from(RATE);
        // Frame 0 is the last one of the chunk before; frame k (1..=frames) is input's k-1.
        let frame = |k: usize, c: usize| {
            if k == 0 {
                self.last[c]
            } else {
                input[(k - 1) * 2 + c]
            }
        };
        let mut at = self.at;
        while (at as usize) < frames {
            let k = at as usize;
            let t = (at - k as f64) as f32;
            for c in 0..2 {
                let (a, b) = (frame(k, c), frame(k + 1, c));
                out.push_back(a + (b - a) * t);
            }
            at += step;
        }
        self.at = at - frames as f64;
        self.last = [input[(frames - 1) * 2], input[(frames - 1) * 2 + 1]];
    }
}

/// The mixer's thread: every [`TICK`], brings what each source delivered to 48 kHz and writes as
/// much as is due since the sink was attached.
fn mix(
    delivered: Vec<Arc<Mutex<Delivered>>>,
    sink: Arc<Mutex<Option<Sink>>>,
    restart: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    levels: impl Fn(&[f32]),
) {
    let n = delivered.len();
    let mut resamplers: Vec<Resampler> = (0..n).map(|_| Resampler::default()).collect();
    let mut queued: Vec<VecDeque<f32>> = (0..n).map(|_| VecDeque::new()).collect();
    let mut loudness = vec![0f32; n];
    let mut since_levels = Instant::now();
    let mut clock = Instant::now();
    let mut written: u64 = 0;
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(TICK);
        for (i, d) in delivered.iter().enumerate() {
            let (rate, samples) = {
                let mut d = d.lock();
                (d.rate, std::mem::take(&mut d.samples))
            };
            for s in &samples {
                loudness[i] = loudness[i].max(s.abs());
            }
            resamplers[i].push(rate, &samples, &mut queued[i]);
            if queued[i].len() > MOST_AHEAD {
                let extra = queued[i].len() - MOST_AHEAD;
                queued[i].drain(..extra - extra % 2);
            }
        }
        if restart.swap(false, Ordering::SeqCst) {
            clock = Instant::now();
            written = 0;
            queued.iter_mut().for_each(VecDeque::clear);
        }
        let target = sink.lock().clone();
        match target {
            Some(target) => {
                let due = (clock.elapsed().as_secs_f64() * f64::from(RATE)) as u64;
                let frames = due.saturating_sub(written) as usize;
                if frames > 0 {
                    let mut out = vec![0f32; frames * 2];
                    for q in queued.iter_mut() {
                        let take = q.len().min(frames * 2);
                        for (o, s) in out.iter_mut().zip(q.drain(..take)) {
                            *o += s;
                        }
                    }
                    for o in out.iter_mut() {
                        *o = o.clamp(-1.0, 1.0);
                    }
                    written += frames as u64;
                    let _ = target.try_send(out);
                }
            }
            // Measuring only: nothing is kept for later.
            None => queued.iter_mut().for_each(VecDeque::clear),
        }
        if since_levels.elapsed() >= Duration::from_millis(100) {
            since_levels = Instant::now();
            levels(&loudness);
            loudness.iter_mut().for_each(|l| *l = 0.0);
        }
    }
}

/// The mixed sound as FFmpeg reads it (`-f f32le`): little-endian 32-bit floats.
pub fn as_bytes(samples: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_brought_to_48_khz_across_chunks() {
        // A ramp at 24 kHz, in two chunks: twice the frames, still a ramp.
        let ramp: Vec<f32> = (0..200).flat_map(|i| [i as f32, -(i as f32)]).collect();
        let mut r = Resampler::default();
        let mut out = VecDeque::new();
        r.push(24_000, &ramp[..100], &mut out);
        r.push(24_000, &ramp[100..], &mut out);
        let out: Vec<f32> = out.into_iter().collect();
        assert_eq!(out.len(), 800, "200 frames become 400, two samples each");
        // Left rises by half a step a frame (the first from the silence before), right mirrors it.
        for (k, pair) in out.chunks(2).enumerate().skip(2) {
            assert!(
                (pair[0] - (k as f32 / 2.0 - 1.0)).abs() < 1e-4,
                "{k}: {pair:?}"
            );
            assert_eq!(pair[1], -pair[0]);
        }
        // At 48 kHz nothing changes.
        let mut same = VecDeque::new();
        Resampler::default().push(RATE, &ramp, &mut same);
        assert_eq!(same.len(), ramp.len());
        // 44.1 kHz: about 48/44.1 as many.
        let mut up = VecDeque::new();
        let long: Vec<f32> = vec![0.25; 44_100 * 2];
        Resampler::default().push(44_100, &long, &mut up);
        assert!(
            (up.len() as i64 / 2 - 48_000).abs() <= 2,
            "{}",
            up.len() / 2
        );
        assert!(up.iter().skip(4).all(|&s| (s - 0.25).abs() < 1e-6));
    }

    #[test]
    fn samples_go_out_as_little_endian_floats() {
        assert_eq!(as_bytes(&[1.0, -0.5]), [0, 0, 128, 63, 0, 0, 0, 191]);
    }

    /// This computer's default microphone and speakers, for a second, with the sound that is due
    /// written: `cargo test -p nook-core mixes_the_real_devices -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs a microphone and speakers"]
    async fn mixes_the_real_devices() {
        println!("microphones: {:?}", microphones());
        println!("speakers: {:?}", speakers());
        let heard = Arc::new(Mutex::new(Vec::<Vec<f32>>::new()));
        let h = heard.clone();
        let mixer = Mixer::start(
            &[
                AudioSource::Microphone { device: None },
                AudioSource::System { device: None },
            ],
            move |l| h.lock().push(l.to_vec()),
        )
        .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
        mixer.attach(tx);
        let started = Instant::now();
        let mut frames = 0usize;
        while started.elapsed() < Duration::from_secs(1) {
            if let Ok(Some(chunk)) =
                tokio::time::timeout(Duration::from_millis(50), rx.recv()).await
            {
                frames += chunk.len() / 2;
            }
        }
        mixer.stop();
        println!(
            "{frames} frames in a second; levels {:?}",
            heard.lock().last()
        );
        assert!(
            (44_000..=52_000).contains(&frames),
            "about a second at 48 kHz, whether or not anything played: {frames}"
        );
        assert!(heard.lock().len() >= 8, "levels ten times a second");
    }
}

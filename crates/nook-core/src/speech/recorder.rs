//! Ports `service/AudioRecorderService.kt`: records the default microphone for the voice prompt.
//!
//! cpal streams are not `Send` on Windows, so each recording runs on a thread of its own that
//! opens the stream, plays it and drops it again. The stream's callback only turns the device's
//! samples into mono floats and hands them over; the thread resamples them to 16 kHz 16-bit
//! ([`super::pipeline`]), keeps them, and sends a level for the waveform 25 times a second on
//! [`topic::SPEECH`]. `stop` writes the WAV under `<home>\tmp` and returns it; `cancel` drops the
//! recording (the original stopped and deleted it).

use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::pipeline::{self, Pipeline};
use crate::events::{self, topic};

/// What the original said when no microphone would open; the reason goes to the log.
pub const MICROPHONE_NOT_SUPPORTED: &str =
    "Microphone not supported. Check OS permissions, hardware, or WSL/Docker limitations.";
/// How long opening the microphone may take before it counts as not there.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// What goes out on the `speech` topic while recording.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SpeechEvent {
    /// The microphone is open.
    Started,
    /// The loudness of the last 40 ms, 0 to 1, for the waveform.
    Level { level: f32 },
    /// The recording ended and was written.
    Stopped,
    /// The recording was dropped.
    Cancelled,
    /// The device failed mid-recording (unplugged, taken away); what was recorded is kept.
    Error { message: String },
}

enum Msg {
    Audio(Vec<f32>),
    DeviceError(String),
    Stop,
}

struct Active {
    control: mpsc::Sender<Msg>,
    thread: Option<JoinHandle<Vec<i16>>>,
}

impl Active {
    /// Ends the recording and returns what it captured.
    fn finish(mut self) -> Vec<i16> {
        let _ = self.control.send(Msg::Stop);
        self.thread
            .take()
            .and_then(|t| t.join().ok())
            .unwrap_or_default()
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        // the stream's callback holds a sender too, so the thread only ends when told to
        let _ = self.control.send(Msg::Stop);
    }
}

/// The voice prompt's recorder. One recording at a time.
pub struct Recorder {
    temp_dir: PathBuf,
    active: Mutex<Option<Active>>,
}

impl Recorder {
    /// Recordings are written into `temp_dir` (`Home::temp_dir()`).
    pub fn new(temp_dir: impl Into<PathBuf>) -> Recorder {
        Recorder {
            temp_dir: temp_dir.into(),
            active: Mutex::new(None),
        }
    }

    pub fn is_recording(&self) -> bool {
        self.active.lock().is_some()
    }

    /// Opens the default microphone and starts recording; levels follow as [`SpeechEvent::Level`].
    /// Recording already is not an error. Blocks until the device is open (well under a second).
    pub fn start(&self) -> Result<()> {
        let mut active = self.active.lock();
        if active.is_some() {
            return Ok(());
        }
        let (control, messages) = mpsc::channel::<Msg>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let audio = control.clone();
        let thread = std::thread::Builder::new()
            .name("nook-microphone".into())
            .spawn(move || capture(audio, messages, ready_tx))
            .context("could not start the recording thread")?;
        let mut started = Active {
            control,
            thread: Some(thread),
        };
        match ready_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(())) => {
                *active = Some(started);
                drop(active);
                events::emit(topic::SPEECH, SpeechEvent::Started);
                Ok(())
            }
            Ok(Err(reason)) => {
                tracing::warn!("The microphone did not open: {reason}");
                if let Some(t) = started.thread.take() {
                    let _ = t.join();
                }
                bail!(MICROPHONE_NOT_SUPPORTED)
            }
            Err(_) => {
                tracing::warn!("The microphone did not open within {OPEN_TIMEOUT:?}");
                bail!(MICROPHONE_NOT_SUPPORTED)
            }
        }
    }

    /// Stops the recording and packages what was captured into a 16 kHz mono 16-bit WAV file in
    /// the temp folder; the caller deletes it once transcribed. With nothing recording, the WAV is
    /// empty (as the original's was).
    pub fn stop(&self) -> Result<PathBuf> {
        let active = self.active.lock().take();
        let samples = active.map(Active::finish).unwrap_or_default();
        std::fs::create_dir_all(&self.temp_dir)
            .with_context(|| format!("could not create {}", self.temp_dir.display()))?;
        let path = self
            .temp_dir
            .join(format!("nook_prompt_{}.wav", uuid::Uuid::new_v4().simple()));
        pipeline::write_wav(&path, &samples)?;
        events::emit(topic::SPEECH, SpeechEvent::Stopped);
        Ok(path)
    }

    /// Drops the recording: nothing is written.
    pub fn cancel(&self) {
        if let Some(active) = self.active.lock().take() {
            active.finish();
            events::emit(topic::SPEECH, SpeechEvent::Cancelled);
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(active) = self.active.get_mut().take() {
            active.finish();
        }
    }
}

/// The recording thread: opens the stream, feeds the pipeline until told to stop, returns the
/// samples.
fn capture(
    audio: mpsc::Sender<Msg>,
    messages: mpsc::Receiver<Msg>,
    ready: mpsc::Sender<Result<(), String>>,
) -> Vec<i16> {
    let (stream, rate) = match open(audio) {
        Ok(opened) => opened,
        Err(e) => {
            let _ = ready.send(Err(format!("{e:#}")));
            return Vec::new();
        }
    };
    let _ = ready.send(Ok(()));
    let mut pipeline = Pipeline::new(rate, |level| {
        events::emit(topic::SPEECH, SpeechEvent::Level { level })
    });
    let mut failed = false;
    while let Ok(msg) = messages.recv() {
        match msg {
            Msg::Audio(samples) => pipeline.push_mono(&samples),
            Msg::DeviceError(message) => {
                // reported once; the stream usually stops delivering after this
                if !failed {
                    failed = true;
                    tracing::warn!("The microphone failed while recording: {message}");
                    events::emit(topic::SPEECH, SpeechEvent::Error { message });
                }
            }
            Msg::Stop => break,
        }
    }
    drop(stream);
    // what the callback delivered before the stream closed
    while let Ok(msg) = messages.try_recv() {
        if let Msg::Audio(samples) = msg {
            pipeline.push_mono(&samples);
        }
    }
    pipeline.into_samples()
}

/// Opens and starts the default input device in its own format; returns the stream and its rate.
fn open(audio: mpsc::Sender<Msg>) -> Result<(cpal::Stream, u32)> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("there is no default input device"))?;
    let supported = device
        .default_input_config()
        .context("the input device has no usable format")?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let rate = config.sample_rate.0;
    tracing::info!(
        "Recording from {} at {rate} Hz, {} channel(s), {format:?}",
        device
            .name()
            .unwrap_or_else(|_| "the default microphone".into()),
        config.channels
    );
    use cpal::SampleFormat as F;
    let stream = match format {
        F::F32 => build::<f32>(&device, &config, audio),
        F::I16 => build::<i16>(&device, &config, audio),
        F::U16 => build::<u16>(&device, &config, audio),
        F::I32 => build::<i32>(&device, &config, audio),
        F::U32 => build::<u32>(&device, &config, audio),
        F::I8 => build::<i8>(&device, &config, audio),
        F::U8 => build::<u8>(&device, &config, audio),
        F::F64 => build::<f64>(&device, &config, audio),
        other => bail!("the input device's sample format {other:?} is not supported"),
    }?;
    stream.play().context("the input stream did not start")?;
    Ok((stream, rate))
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    audio: mpsc::Sender<Msg>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = config.channels as usize;
    let errors = audio.clone();
    let mut scratch: Vec<f32> = Vec::new();
    let stream = device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                // as little as possible on the audio thread: mono floats, handed over
                scratch.clear();
                scratch.extend(data.iter().map(|&s| <f32 as cpal::Sample>::from_sample(s)));
                let mut mono = Vec::with_capacity(scratch.len() / channels.max(1) + 1);
                pipeline::downmix_into(&scratch, channels, &mut mono);
                let _ = audio.send(Msg::Audio(mono));
            },
            move |e| {
                let _ = errors.send(Msg::DeviceError(e.to_string()));
            },
            None,
        )
        .context("the input stream could not be opened")?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_carry_their_kind() {
        let level = serde_json::to_value(SpeechEvent::Level { level: 0.5 }).unwrap();
        assert_eq!(level, serde_json::json!({"kind": "level", "level": 0.5}));
        assert_eq!(
            serde_json::to_value(SpeechEvent::Started).unwrap(),
            serde_json::json!({"kind": "started"})
        );
        let error = serde_json::to_value(SpeechEvent::Error {
            message: "gone".into(),
        })
        .unwrap();
        assert_eq!(
            error,
            serde_json::json!({"kind": "error", "message": "gone"})
        );
    }

    #[test]
    fn stopping_with_nothing_recorded_writes_an_empty_wav_in_the_temp_folder() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Recorder::new(dir.path().join("tmp"));
        assert!(!recorder.is_recording());
        recorder.cancel();
        let wav = recorder.stop().unwrap();
        assert_eq!(wav.parent().unwrap(), dir.path().join("tmp"));
        let name = wav.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.starts_with("nook_prompt_") && name.ends_with(".wav"),
            "{name}"
        );
        let reader = hound::WavReader::open(&wav).unwrap();
        assert_eq!(reader.spec().sample_rate, 16_000);
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(reader.len(), 0);
    }

    /// With a real microphone: `cargo test -p nook-core records_the_default_microphone -- --ignored`.
    #[test]
    #[ignore = "needs a microphone"]
    fn records_the_default_microphone() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Recorder::new(dir.path());
        let mut events = events::subscribe();
        recorder.start().unwrap();
        assert!(recorder.is_recording());
        std::thread::sleep(Duration::from_millis(1_000));
        let wav = recorder.stop().unwrap();
        let reader = hound::WavReader::open(&wav).unwrap();
        assert!(
            reader.len() > 12_000,
            "about a second at 16 kHz: {}",
            reader.len()
        );
        let mut levels = 0;
        while let Ok(e) = events.try_recv() {
            if e.topic == topic::SPEECH && e.payload["kind"] == "level" {
                levels += 1;
            }
        }
        assert!((18..=32).contains(&levels), "{levels} levels in a second");
    }
}

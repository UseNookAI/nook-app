//! Ports `flow/Speaker.java` and `flow/SpeechEngine.java`: speaks lines with the audio engine
//! (audio.cpp's `audiocpp_cli`), one process per run with the model loaded once and every line as
//! its own request and its own WAV. A cloning voice takes the reference clip and its words; a
//! preset voice takes a language and a voice id.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::languages;
use super::voices::Voice;
use super::Stopped;

/// How long one run may take before it is stopped: a feature-length track on a processor.
pub const MAX_HOURS: u64 = 6;
/// How often the run's folder is looked at for finished lines.
const POLL: Duration = Duration::from_millis(500);

/// One line to speak; its WAV is `<out_dir>\<id>.wav`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub id: String,
    pub text: String,
}

/// What to speak and how.
///
/// - `model`: the voice's file
/// - `language`: the code of the language the lines are in
/// - `cloned`: whether to clone the speaker from `reference_wav`
/// - `reference_text`: what is said in the reference, when known
/// - `female_speaker`: for a preset voice, whether the speaker sounded like a woman
/// - `out_dir`: where the WAVs go
/// - `on_cpu`: to speak on the processor, whatever the engine's build (the card is short)
#[derive(Clone, Debug)]
pub struct Request {
    pub voice: Voice,
    pub model: PathBuf,
    pub language: String,
    pub lines: Vec<Line>,
    pub cloned: bool,
    pub reference_wav: Option<PathBuf>,
    pub reference_text: Option<String>,
    pub female_speaker: bool,
    pub out_dir: PathBuf,
    pub on_cpu: bool,
}

/// Lines spoken so far, of all: `(done, total)`.
pub type SpokenProgress<'a> = dyn Fn(usize, usize) + Send + Sync + 'a;

/// Something that speaks lines into WAV files: the audio engine, or a stand-in in a test.
#[async_trait]
pub trait Speaker: Send + Sync {
    /// The WAV of each line in order, None where the engine spoke none; fails when nothing was
    /// spoken, and with [`Stopped`] when `cancel` fires.
    async fn speak(
        &self,
        request: &Request,
        progress: &SpokenProgress<'_>,
        cancel: &CancellationToken,
    ) -> Result<Vec<Option<PathBuf>>>;
}

/// audio.cpp's command line.
pub struct AudioEngine {
    cli: PathBuf,
    backend: String,
    threads: usize,
}

impl AudioEngine {
    /// `backend` is the build's: `cuda`, `vulkan` or `cpu`. Every build runs on the processor too
    /// (a request's `on_cpu`).
    pub fn new(cli: impl Into<PathBuf>, backend: &str, threads: usize) -> AudioEngine {
        AudioEngine {
            cli: cli.into(),
            backend: backend.to_string(),
            threads,
        }
    }

    /// The command line: the model, the backend and the lines as one session. In a session the
    /// voice's inputs go with each request (the engine refuses them as options), see
    /// [`requests_json`].
    pub fn command(&self, r: &Request, requests: &Path) -> Vec<String> {
        let mut cmd = vec![
            "--task".to_string(),
            "tts".into(),
            "--family".into(),
            r.voice.family.clone(),
            "--model".into(),
            r.model.display().to_string(),
            "--backend".into(),
            self.backend_for(r).to_string(),
            "--request-sequence".into(),
            requests.display().to_string(),
            "--out-dir".into(),
            r.out_dir.display().to_string(),
            "--metrics".into(),
        ];
        if self.threads > 0 {
            cmd.push("--threads".into());
            cmd.push(self.threads.to_string());
        }
        cmd
    }
}

impl AudioEngine {
    /// The backend `r` is spoken on: the build's, or the processor.
    fn backend_for(&self, r: &Request) -> &str {
        if r.on_cpu {
            "cpu"
        } else {
            &self.backend
        }
    }
}

/// A preset voice: a woman's for a woman speaking, a man's otherwise.
pub fn preset_voice(r: &Request) -> &'static str {
    if r.female_speaker {
        "F1"
    } else {
        "M1"
    }
}

/// The engine's request file: each line with its id and text, and the voice's inputs on every
/// one: the reference clip and its words for a cloning voice, the language and a preset for a
/// preset voice. Qwen3 takes the language's name as a hint; VoxCPM2 tells it from the text.
pub fn requests_json(r: &Request) -> String {
    let requests: Vec<Value> = r
        .lines
        .iter()
        .map(|l| {
            let mut item = json!({ "id": l.id, "text": l.text });
            if r.cloned {
                if let Some(wav) = &r.reference_wav {
                    item["voice_ref"] = json!(wav.display().to_string());
                    if let Some(t) = r.reference_text.as_deref().filter(|t| !t.trim().is_empty()) {
                        item["reference_text"] = json!(t);
                    }
                }
            }
            match r.voice.family.as_str() {
                "supertonic" => {
                    item["language"] = json!(r.language);
                    item["voice_id"] = json!(preset_voice(r));
                }
                "qwen3_tts" => item["language"] = json!(languages::name_of(&r.language)),
                _ => {}
            }
            item
        })
        .collect();
    json!({ "requests": requests }).to_string()
}

fn wav_of(r: &Request, line: &Line) -> PathBuf {
    r.out_dir.join(format!("{}.wav", line.id))
}

fn count_done(r: &Request) -> usize {
    r.lines.iter().filter(|l| wav_of(r, l).exists()).count()
}

/// The last lines of the engine's log, for a message.
fn tail(log: &Path) -> String {
    let text = std::fs::read(log)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(6)..]
        .join(" ")
        .trim()
        .to_string()
}

#[async_trait]
impl Speaker for AudioEngine {
    async fn speak(
        &self,
        r: &Request,
        progress: &SpokenProgress<'_>,
        cancel: &CancellationToken,
    ) -> Result<Vec<Option<PathBuf>>> {
        tokio::fs::create_dir_all(&r.out_dir)
            .await
            .with_context(|| format!("Could not create {}", r.out_dir.display()))?;
        let base = r
            .out_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let requests = r.out_dir.with_file_name(format!("{base}-requests.json"));
        tokio::fs::write(&requests, requests_json(r))
            .await
            .with_context(|| format!("Could not write {}", requests.display()))?;
        let log = r.out_dir.with_file_name(format!("{base}.log"));
        let log_file = std::fs::File::create(&log)
            .with_context(|| format!("Could not write {}", log.display()))?;
        tracing::info!(
            "Speaking {} lines with {} ({})",
            r.lines.len(),
            r.voice.name,
            self.backend_for(r)
        );
        let mut cmd = crate::process::command(&self.cli);
        // Killed when the run stops, even while nothing is left to wait on it.
        cmd.args(self.command(r, &requests))
            .stdout(log_file.try_clone()?)
            .stderr(log_file)
            .kill_on_drop(true);
        if let Some(dir) = self.cli.parent() {
            cmd.current_dir(dir);
        }
        let mut child = crate::process::spawn_managed(&mut cmd).with_context(|| {
            format!("Could not start the voice engine ({})", self.cli.display())
        })?;
        let deadline = Instant::now() + Duration::from_secs(MAX_HOURS * 3600);
        let mut tick = tokio::time::interval(POLL);
        let mut reported = 0;
        let total = r.lines.len();
        let status = loop {
            tokio::select! {
                status = child.wait() => break status?,
                _ = cancel.cancelled() => {
                    let _ = child.kill().await;
                    return Err(Stopped.into());
                }
                _ = tick.tick() => {
                    if Instant::now() > deadline {
                        let _ = child.kill().await;
                        bail!("Speaking took over {MAX_HOURS} hours and was stopped.");
                    }
                    let done = count_done(r);
                    if done != reported {
                        reported = done;
                        progress(done, total);
                    }
                }
            }
        };
        if cancel.is_cancelled() {
            return Err(Stopped.into());
        }
        if !status.success() {
            bail!(
                "The voice engine stopped (exit {}). {}",
                status.code().unwrap_or(-1),
                tail(&log)
            );
        }
        let out: Vec<Option<PathBuf>> = r
            .lines
            .iter()
            .map(|l| {
                let wav = wav_of(r, l);
                std::fs::metadata(&wav)
                    .is_ok_and(|m| m.len() > 44)
                    .then_some(wav)
            })
            .collect();
        let missing = out.iter().filter(|w| w.is_none()).count();
        if missing == total {
            bail!("The voice engine spoke none of the lines. {}", tail(&log));
        }
        if missing > 0 {
            tracing::warn!("The voice engine skipped {missing} of {total} lines");
        }
        progress(total, total);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::voices::VoiceFile;

    fn request(family: &str, cloned: bool) -> Request {
        Request {
            voice: Voice {
                id: family.into(),
                family: family.into(),
                name: family.into(),
                clones: cloned,
                designs: false,
                languages: vec!["de".into()],
                licence: String::new(),
                sample_rate: 24_000,
                file: VoiceFile {
                    name: "v.gguf".into(),
                    url: String::new(),
                    sha256: None,
                    bytes: 1,
                },
            },
            model: PathBuf::from(r"C:\voices\v.gguf"),
            language: "de".into(),
            lines: vec![
                Line {
                    id: "line-0".into(),
                    text: "Hallo.".into(),
                },
                Line {
                    id: "line-3".into(),
                    text: "Tschüss.".into(),
                },
            ],
            cloned,
            reference_wav: Some(PathBuf::from(r"C:\work\reference.wav")),
            reference_text: Some("Hello there.".into()),
            female_speaker: true,
            out_dir: PathBuf::from(r"C:\work\spoken"),
            on_cpu: false,
        }
    }

    #[test]
    fn the_requests_carry_what_each_voice_takes() {
        let clone: Value =
            serde_json::from_str(&requests_json(&request("qwen3_tts", true))).unwrap();
        let first = &clone["requests"][0];
        assert_eq!(first["id"], "line-0");
        assert_eq!(first["voice_ref"], r"C:\work\reference.wav");
        assert_eq!(first["reference_text"], "Hello there.");
        assert_eq!(first["language"], "German", "Qwen3 takes the name");
        assert_eq!(clone["requests"][1]["text"], "Tschüss.");

        let preset: Value =
            serde_json::from_str(&requests_json(&request("supertonic", false))).unwrap();
        let first = &preset["requests"][0];
        assert!(first.get("voice_ref").is_none());
        assert_eq!(first["language"], "de");
        assert_eq!(first["voice_id"], "F1");

        let vox: Value = serde_json::from_str(&requests_json(&request("voxcpm2", true))).unwrap();
        assert!(vox["requests"][0].get("language").is_none());

        let engine = AudioEngine::new(r"C:\bin\audiocpp_cli.exe", "vulkan", 4);
        let cmd = engine.command(
            &request("qwen3_tts", true),
            Path::new(r"C:\work\spoken-requests.json"),
        );
        assert_eq!(
            cmd.join(" "),
            r"--task tts --family qwen3_tts --model C:\voices\v.gguf --backend vulkan --request-sequence C:\work\spoken-requests.json --out-dir C:\work\spoken --metrics --threads 4"
        );
        // Short of memory on the card: the same build speaks on the processor.
        let on_cpu = Request {
            on_cpu: true,
            ..request("qwen3_tts", true)
        };
        let cmd = engine.command(&on_cpu, Path::new(r"C:\work\r.json"));
        assert!(cmd.join(" ").contains("--backend cpu"), "{cmd:?}");
    }

    /// A stand-in for audiocpp_cli: writes a WAV per line it is told of, or fails.
    #[cfg(windows)]
    fn fake_cli(dir: &Path, fails: bool) -> PathBuf {
        let cli = dir.join("fake_cli.cmd");
        let body = if fails {
            "@echo off\r\necho model failed to load\r\nexit /b 3\r\n".to_string()
        } else {
            // one line of two is spoken, into the folder after --out-dir
            "@echo off\r\nset here=%~dp0\r\n:next\r\nif \"%~1\"==\"--out-dir\" goto found\r\nif \"%~1\"==\"\" exit /b 9\r\nshift\r\ngoto next\r\n:found\r\ncopy /y \"%here%tone.wav\" \"%~2\\line-0.wav\" >nul\r\nexit /b 0\r\n".to_string()
        };
        std::fs::write(&cli, body).unwrap();
        crate::flow::audio::tests::write_tone(&dir.join("tone.wav"), 24_000, 1, 0.5, 300.0);
        cli
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn the_engine_speaks_what_it_can_and_says_why_when_it_cannot() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = request("qwen3_tts", true);
        r.out_dir = dir.path().join("spoken");
        let engine = AudioEngine::new(fake_cli(dir.path(), false), "cpu", 0);
        let seen = parking_lot::Mutex::new(Vec::new());
        let out = engine
            .speak(
                &r,
                &|d, t| seen.lock().push((d, t)),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out[0], Some(r.out_dir.join("line-0.wav")));
        assert_eq!(out[1], None);
        assert_eq!(seen.lock().last(), Some(&(2, 2)));
        assert!(dir.path().join("spoken-requests.json").is_file());

        let failing = AudioEngine::new(fake_cli(dir.path(), true), "cpu", 0);
        let err = failing
            .speak(&r, &|_, _| {}, &CancellationToken::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("The voice engine stopped (exit 3)."),
            "{err}"
        );
        assert!(err.contains("model failed to load"), "{err}");
    }
}

/// With the real engine and voices: `NOOK_TEST_AUDIOCPP` names `audiocpp_cli.exe`,
/// `NOOK_TEST_AUDIOCPP_BACKEND` its build (`cuda`, the default, `vulkan` or `cpu`),
/// `NOOK_TEST_VOICES` the folder with the voice files, `NOOK_TEST_SPEECH` an English WAV of
/// someone speaking, and `NOOK_TEST_OUT` where to leave what was spoken:
/// `cargo test -p nook-core speaks_with_the_real_engine -- --ignored --nocapture`.
#[cfg(test)]
mod live {
    use super::*;
    use crate::flow::audio;
    use crate::flow::dub::{self, Layout};
    use crate::flow::subtitles::Segment;
    use crate::flow::voices::Voices;

    fn env(name: &str) -> PathBuf {
        PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("set {name}")))
    }

    #[tokio::test]
    #[ignore = "needs audio.cpp, its voices and a GPU"]
    async fn speaks_with_the_real_engine() {
        let (cli, voices_dir, speech, out) = (
            env("NOOK_TEST_AUDIOCPP"),
            env("NOOK_TEST_VOICES"),
            env("NOOK_TEST_SPEECH"),
            env("NOOK_TEST_OUT"),
        );
        std::fs::create_dir_all(&out).unwrap();
        let wav = out.join("audio.wav");
        let seconds = audio::to_speech_wav(&speech, &wav, None, &CancellationToken::new()).unwrap();
        let reference = out.join("reference.wav");
        crate::flow::reference::cut(&wav, 0.0, 8.0_f64.min(seconds), &reference).unwrap();
        let pitch = audio::pitch_hz(&audio::read_wav(&reference).unwrap());
        println!("speech {seconds:.1} s, pitch {pitch:?} Hz");

        let voices = Voices::bundled();
        let lines = vec![
            Line { id: "line-0".into(), text: "Danke, dass Sie heute dabei sind.".into() },
            Line { id: "line-1".into(), text: "Wir sprechen darüber, künstliche Intelligenz auf dem eigenen Computer laufen zu lassen.".into() },
            Line { id: "line-2".into(), text: "Alles bleibt auf diesem Rechner, nichts geht in die Cloud.".into() },
        ];
        let segments = vec![
            Segment::new(0.3, 2.2, "Thank you for joining us today."),
            Segment::new(
                2.4,
                6.9,
                "We are going to talk about running artificial intelligence on your own computer.",
            ),
            Segment::new(
                7.1,
                seconds,
                "Everything stays on this machine, and nothing goes to the cloud.",
            ),
        ];
        let backend = std::env::var("NOOK_TEST_AUDIOCPP_BACKEND").unwrap_or_else(|_| "cuda".into());
        let engine = AudioEngine::new(&cli, &backend, 4);
        for (voice_id, cloned) in [("qwen3-tts-0.6b", true), ("supertonic-3", false)] {
            let voice = voices.by_id(voice_id).unwrap().clone();
            let request = Request {
                model: voices_dir.join(&voice.file.name),
                voice,
                language: "de".into(),
                lines: lines.clone(),
                cloned,
                reference_wav: cloned.then(|| reference.clone()),
                reference_text: cloned.then(|| "Thank you for joining us today. We are going to talk about running artificial intelligence on your own computer.".to_string()),
                female_speaker: pitch.is_some_and(|p| p > audio::FEMALE_ABOVE_HZ),
                out_dir: out.join(format!("spoken-{voice_id}")),
                on_cpu: false,
            };
            let started = std::time::Instant::now();
            let clips = engine
                .speak(
                    &request,
                    &|d, t| println!("  {voice_id}: {d} of {t}"),
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            println!("{voice_id}: {:.1} s", started.elapsed().as_secs_f64());
            for c in clips.iter().flatten() {
                let pcm = audio::read_wav(c).unwrap();
                let loud = pcm.samples.iter().fold(0f32, |m, x| m.max(x.abs()));
                println!(
                    "  {} {:.2} s at {} Hz, peak {loud:.2}",
                    c.display(),
                    pcm.seconds(),
                    pcm.rate
                );
                assert!(pcm.seconds() > 0.5 && loud > 0.05);
            }
            let track = out.join(format!("dub-{voice_id}.wav"));
            let placed = dub::assemble(
                &segments,
                &clips.into_iter().collect::<Vec<_>>(),
                Layout::Timeline,
                seconds,
                &track,
            )
            .unwrap();
            println!("  laid out: {placed:?}");
        }
    }
}

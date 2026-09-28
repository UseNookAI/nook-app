//! Ports `FlowServiceTest.java`: whole runs against a scripted Whisper, chat model and voice.

use super::*;
use crate::flow::audio::tests::write_tone;
use crate::flow::voice_engine::SpokenProgress;
use crate::flow::voices::VoiceFile;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Whisper hears the lines it is given (at 0.5 s steps in each part, in English unless told);
/// the chat model upper-cases; the card is always free.
struct Scripted {
    facts: Mutex<Facts>,
    heard: Vec<&'static str>,
    detected: &'static str,
    chats: AtomicUsize,
    languages_asked: Mutex<Vec<Option<String>>>,
    /// The chat model takes a minute over each batch, as a big one on a small card does.
    slow_chat: AtomicBool,
}

impl Scripted {
    fn new(dir: &Path) -> Scripted {
        let engine = dir.join("audiocpp_cli.exe");
        std::fs::write(&engine, b"").unwrap();
        Scripted {
            facts: Mutex::new(Facts {
                speech_model: Some("whisper-large-v3-turbo".into()),
                speech_engine_installed: true,
                speech_download: Some(("whisper-large-v3-turbo".into(), 700)),
                speech_engine_bytes: 0,
                translator: Some(("qwen3-4b".into(), "Qwen3 4B".into())),
                ffmpeg: None,
                ffmpeg_bytes: 80,
                voice_engine: Some(engine),
                voice_engine_bytes: 60,
                voice_backend: "vulkan".into(),
                cpu: false,
            }),
            heard: vec!["Hello there.", "[Music]", "How are you today?", "Goodbye."],
            detected: "english",
            chats: AtomicUsize::new(0),
            languages_asked: Mutex::new(Vec::new()),
            slow_chat: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl FlowRuntime for Scripted {
    fn facts(&self) -> Facts {
        self.facts.lock().clone()
    }
    async fn transcribe(&self, _wav: &Path, _model: &str, language: Option<&str>) -> Result<Value> {
        self.languages_asked
            .lock()
            .push(language.map(str::to_string));
        let segments: Vec<Value> = self
            .heard
            .iter()
            .enumerate()
            .map(|(i, t)| json!({"start": i as f64 * 0.5, "end": i as f64 * 0.5 + 0.4, "text": format!(" {t}")}))
            .collect();
        Ok(json!({"language": self.detected, "segments": segments}))
    }
    async fn chat(&self, _model: &str, system: &str, user: &str) -> Result<String> {
        assert!(system.contains("into German"), "{system}");
        self.chats.fetch_add(1, Ordering::SeqCst);
        if self.slow_chat.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        Ok(user.to_uppercase())
    }
    async fn gpu_turn<'a>(
        &'a self,
        _need: u64,
        _cancel: &CancellationToken,
    ) -> Option<Box<dyn Send + 'a>> {
        Some(Box::new(()))
    }
    async fn install_component(
        &self,
        _c: EngineComponent,
        progress: Progress,
        _cancel: &CancellationToken,
    ) -> Result<bool> {
        progress(60, 60);
        self.facts.lock().ffmpeg = Some(PathBuf::from("ffmpeg.exe"));
        Ok(true)
    }
    async fn download_model(
        &self,
        _id: &str,
        progress: Progress,
        _cancel: &CancellationToken,
    ) -> Result<bool> {
        progress(700, 700);
        self.facts.lock().speech_model = Some("whisper-large-v3-turbo".into());
        Ok(true)
    }
}

/// Writes a short tone per line; fails when told to (a voice the engine refuses); or waits to
/// be stopped.
struct FakeSpeaker {
    fails_for: Option<&'static str>,
    waits: bool,
    requests: Mutex<Vec<(String, bool, bool)>>,
}

#[async_trait]
impl Speaker for FakeSpeaker {
    async fn speak(
        &self,
        r: &Request,
        progress: &SpokenProgress<'_>,
        cancel: &CancellationToken,
    ) -> Result<Vec<Option<PathBuf>>> {
        self.requests.lock().push((
            r.voice.id.clone(),
            r.cloned,
            r.reference_wav.as_ref().is_some_and(|w| w.is_file()),
        ));
        if self.waits {
            cancel.cancelled().await;
            return Err(Stopped.into());
        }
        if self.fails_for == Some(r.voice.id.as_str()) {
            bail!("The voice engine stopped (exit 1). unsupported op");
        }
        std::fs::create_dir_all(&r.out_dir)?;
        let mut out = Vec::new();
        for (i, l) in r.lines.iter().enumerate() {
            let wav = r.out_dir.join(format!("{}.wav", l.id));
            write_tone(&wav, r.voice.sample_rate, 1, 0.3, 200.0);
            out.push(Some(wav));
            progress(i + 1, r.lines.len());
        }
        Ok(out)
    }
}

fn voice(id: &str, clones: bool, langs: &[&str]) -> Voice {
    Voice {
        id: id.into(),
        family: id.into(),
        name: id.to_uppercase(),
        clones,
        designs: false,
        languages: langs.iter().map(|s| s.to_string()).collect(),
        licence: String::new(),
        sample_rate: 24_000,
        file: VoiceFile {
            name: format!("{id}.gguf"),
            url: format!("http://127.0.0.1:9/{id}.gguf"),
            sha256: None,
            bytes: 4,
        },
    }
}

struct Rig {
    dir: tempfile::TempDir,
    runtime: Arc<Scripted>,
    speaker: Arc<FakeSpeaker>,
    service: Arc<FlowService>,
}

fn rig_with(speaker: FakeSpeaker, voices_in: &[&str]) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let runtime = Arc::new(Scripted::new(dir.path()));
    let voices_dir = dir.path().join("voices");
    std::fs::create_dir_all(&voices_dir).unwrap();
    for v in voices_in {
        std::fs::write(voices_dir.join(format!("{v}.gguf")), b"gguf").unwrap();
    }
    let speaker = Arc::new(speaker);
    let service = FlowService::new(
        runtime.clone(),
        dir.path().join("flows"),
        voices_dir,
        dir.path().join("tmp"),
        Voices::new(vec![
            voice("clone", true, &["en", "de"]),
            voice("tonic", false, &["en", "de"]),
        ]),
        Some(speaker.clone()),
    );
    Rig {
        dir,
        runtime,
        speaker,
        service,
    }
}

fn rig() -> Rig {
    rig_with(
        FakeSpeaker {
            fails_for: None,
            waits: false,
            requests: Mutex::new(Vec::new()),
        },
        &["clone", "tonic"],
    )
}

async fn finished(service: &FlowService, id: &str) -> Run {
    for _ in 0..400 {
        if let Some(r) = service.run(id).filter(Run::finished) {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("run {id} did not finish: {:?}", service.run(id));
}

fn input(rig: &Rig) -> PathBuf {
    let file = rig.dir.path().join("talk.wav");
    write_tone(&file, 44_100, 2, 3.0, 180.0);
    file
}

#[tokio::test]
async fn a_whole_run_translates_speaks_and_keeps_its_files() {
    let rig = rig();
    let file = input(&rig);
    let run = rig.service.submit_file(&file, None, "de", true).unwrap();
    assert_eq!(run.status, Status::Queued);
    assert_eq!(run.input_name, "talk.wav");
    let done = finished(&rig.service, &run.id).await;
    assert_eq!(done.status, Status::Done, "{:?}", done.error);
    assert_eq!(done.detected_language.as_deref(), Some("en"));
    assert_eq!(done.segments.len(), 3, "the [Music] line is not speech");
    assert_eq!(
        done.segments[1].translation.as_deref(),
        Some("HOW ARE YOU TODAY?")
    );
    assert_eq!(
        (done.voice_name.as_deref(), done.cloned),
        (Some("CLONE"), true)
    );
    assert_eq!(done.model_id.as_deref(), Some("qwen3-4b"));
    assert!((done.duration_seconds - 3.0).abs() < 0.01);
    // The translation's times are where it is heard in the dubbed track.
    assert!(done
        .segments
        .iter()
        .all(|s| s.spoken_start.is_some() && s.spoken_end > s.spoken_start));
    assert_eq!(
        *rig.runtime.languages_asked.lock(),
        vec![None],
        "one part, the language left to Whisper"
    );
    let request = rig.speaker.requests.lock()[0].clone();
    assert_eq!(
        request,
        ("clone".to_string(), true, true),
        "cloned from a cut reference"
    );

    let folder = rig.service.folder(&run.id);
    for name in [
        "talk.de.wav",
        "talk.en.txt",
        "talk.en.srt",
        "talk.de.txt",
        "talk.de.srt",
        "run.json",
    ] {
        assert!(folder.join(name).is_file(), "{name}");
    }
    assert_eq!(
        done.audio.as_deref().map(PathBuf::from),
        Some(folder.join("talk.de.wav"))
    );
    let track = audio::read_wav(&folder.join("talk.de.wav")).unwrap();
    assert!(
        (track.seconds() - 3.0).abs() < 0.05,
        "padded to the original: {}",
        track.seconds()
    );
    assert_eq!(
        std::fs::read_to_string(folder.join("talk.de.txt")).unwrap(),
        "HELLO THERE.\nHOW ARE YOU TODAY?\nGOODBYE.\n"
    );
    assert!(
        !rig.dir.path().join("tmp").join(&run.id).exists(),
        "scratch work is cleaned"
    );
    assert_eq!(rig.service.busy_with(), None);

    // Back after a restart, with its track.
    let again = FlowService::new(
        rig.runtime.clone(),
        rig.dir.path().join("flows"),
        rig.dir.path().join("voices"),
        rig.dir.path().join("tmp"),
        Voices::default(),
        None,
    );
    let back = again.run(&run.id).unwrap();
    assert_eq!(back.status, Status::Done);
    assert_eq!(back.segments, done.segments);
    assert_eq!(back.audio, done.audio);
    assert_eq!(
        back.created_at.timestamp_millis(),
        done.created_at.timestamp_millis()
    );

    assert!(rig.service.delete(&run.id));
    assert!(!folder.exists());
    assert!(rig.service.run(&run.id).is_none());
    assert!(!rig.service.delete(&run.id));
}

#[tokio::test]
async fn the_same_language_needs_no_translation() {
    let rig = rig();
    let file = input(&rig);
    let run = rig
        .service
        .submit_file(&file, Some("en"), "en", false)
        .unwrap();
    let done = finished(&rig.service, &run.id).await;
    assert_eq!(done.status, Status::Done, "{:?}", done.error);
    assert_eq!(rig.runtime.chats.load(Ordering::SeqCst), 0);
    assert_eq!(
        done.segments[0].translation.as_deref(),
        Some("Hello there.")
    );
    assert_eq!(
        *rig.runtime.languages_asked.lock(),
        vec![Some("en".to_string())]
    );
    assert_eq!(
        (done.voice_name.as_deref(), done.cloned),
        (Some("TONIC"), false)
    );
}

#[tokio::test]
async fn a_recording_moves_into_its_run_and_is_laid_out_line_after_line() {
    let rig = rig();
    let recording = rig.dir.path().join("nook_prompt_1.wav");
    write_tone(&recording, 16_000, 1, 3.0, 180.0);
    let run = rig
        .service
        .submit_recording(&recording, None, "de", true)
        .unwrap();
    assert!(!recording.exists(), "moved");
    assert_eq!(run.source, Source::Microphone);
    assert_eq!(run.input_name, "Recording");
    let kept = rig.service.folder(&run.id).join("recording.wav");
    assert_eq!(PathBuf::from(&run.input), kept);
    let done = finished(&rig.service, &run.id).await;
    assert_eq!(done.status, Status::Done, "{:?}", done.error);
    let track = audio::read_wav(Path::new(done.audio.as_deref().unwrap())).unwrap();
    // three lines of about 0.3 s with pauses, not the recording's 3 s timeline
    assert!(track.seconds() < 2.3, "{}", track.seconds());
    assert!(rig
        .service
        .folder(&run.id)
        .join("recording.de.txt")
        .is_file());

    // Run again copies the recording, so deleting either run leaves the other whole.
    let again = rig.service.again(&run.id).unwrap();
    assert_ne!(again.input, run.input);
    assert!(Path::new(&again.input).is_file());
    finished(&rig.service, &again.id).await;
    assert!(rig.service.delete(&run.id));
    assert!(Path::new(&again.input).is_file());
}

#[tokio::test]
async fn a_voice_that_cannot_clone_falls_back_to_a_standard_one() {
    let rig = rig_with(
        FakeSpeaker {
            fails_for: Some("clone"),
            waits: false,
            requests: Mutex::new(Vec::new()),
        },
        &["clone", "tonic"],
    );
    let file = input(&rig);
    let run = rig.service.submit_file(&file, None, "de", true).unwrap();
    let done = finished(&rig.service, &run.id).await;
    assert_eq!(done.status, Status::Done, "{:?}", done.error);
    assert_eq!(
        (done.voice_name.as_deref(), done.cloned),
        (Some("TONIC"), false)
    );
    assert_eq!(
        done.note.as_deref(),
        Some("CLONE could not speak in the speaker's voice, so it is spoken in a standard voice by TONIC.")
    );

    // Without the standard voice in, the run fails and says why.
    let rig = rig_with(
        FakeSpeaker {
            fails_for: Some("clone"),
            waits: false,
            requests: Mutex::new(Vec::new()),
        },
        &["clone"],
    );
    let file = input(&rig);
    let run = rig.service.submit_file(&file, None, "de", true).unwrap();
    let failed = finished(&rig.service, &run.id).await;
    assert_eq!(failed.status, Status::Failed);
    assert!(failed.error.unwrap().contains("unsupported op"));
}

#[tokio::test]
async fn stop_ends_a_run_at_once_while_the_chat_model_thinks() {
    let rig = rig();
    rig.runtime.slow_chat.store(true, Ordering::SeqCst);
    let run = rig
        .service
        .submit_file(&input(&rig), None, "de", true)
        .unwrap();
    for _ in 0..400 {
        if rig.service.run(&run.id).unwrap().stage == Some(Stage::Translating) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // Inside the batch now.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.runtime.chats.load(Ordering::SeqCst), 1);
    let asked = std::time::Instant::now();
    rig.service.cancel(&run.id);
    let stopped = finished(&rig.service, &run.id).await;
    assert_eq!(stopped.status, Status::Cancelled);
    assert!(
        asked.elapsed() < Duration::from_secs(2),
        "stopped after {:?}",
        asked.elapsed()
    );
    rig.service.shutdown().await;
}

#[tokio::test]
async fn runs_wait_their_turn_and_stop_when_asked() {
    let rig = rig_with(
        FakeSpeaker {
            fails_for: None,
            waits: true,
            requests: Mutex::new(Vec::new()),
        },
        &["clone", "tonic"],
    );
    let file = input(&rig);
    let first = rig.service.submit_file(&file, None, "de", true).unwrap();
    let second = rig.service.submit_file(&file, None, "de", true).unwrap();
    assert!(rig.service.busy_with().is_some());
    rig.service.cancel(&second.id);
    assert_eq!(
        rig.service.run(&second.id).unwrap().status,
        Status::Cancelled
    );
    for _ in 0..400 {
        if rig.service.run(&first.id).unwrap().stage == Some(Stage::Speaking) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    rig.service.cancel(&first.id);
    let stopped = finished(&rig.service, &first.id).await;
    assert_eq!(stopped.status, Status::Cancelled);
    assert!(!rig.service.delete("nope"));
    rig.service.shutdown().await;
    assert_eq!(rig.service.busy_with(), None);
}

#[tokio::test]
async fn the_plan_says_what_is_missing_and_what_will_speak() {
    let rig = rig_with(
        FakeSpeaker {
            fails_for: None,
            waits: false,
            requests: Mutex::new(Vec::new()),
        },
        &[],
    );
    let file = input(&rig);
    let plan = rig.service.plan(&PlanInput::File(file.clone()), "de", true);
    assert_eq!(plan.problem, None);
    assert!(!plan.ready);
    assert_eq!(
        plan.spoken_with,
        "German will be spoken by CLONE in the speaker's own voice."
    );
    assert_eq!(plan.needs.len(), 1);
    assert_eq!(
        (plan.needs[0].what.as_str(), plan.total_bytes),
        ("the CLONE voice", 4)
    );
    let mic = rig.service.plan(&PlanInput::Microphone, "de", true);
    assert!(mic.spoken_with.ends_with("in your own voice."));
    let standard = rig.service.plan(&PlanInput::Microphone, "de", false);
    assert_eq!(
        standard.spoken_with,
        "German will be spoken in a standard voice by TONIC."
    );
    let none = rig.service.plan(&PlanInput::Microphone, "bn", true);
    assert!(none.no_voice && none.needs.is_empty());
    assert_eq!(
        none.spoken_with,
        "Nook has no voice for Bengali yet, so this run gives the text and subtitles."
    );
    assert!(none.ready);

    assert_eq!(
        rig.service
            .plan(&PlanInput::Nothing, "de", true)
            .problem
            .as_deref(),
        Some("Choose an audio or video file.")
    );
    assert_eq!(
        rig.service
            .plan(
                &PlanInput::File(rig.dir.path().join("gone.mp3")),
                "de",
                true
            )
            .problem
            .as_deref(),
        Some("gone.mp3 is not there any more.")
    );
    let video = rig.dir.path().join("clip.mp4");
    std::fs::write(&video, b"not really").unwrap();
    let plan = rig.service.plan(&PlanInput::File(video), "bn", true);
    assert_eq!(plan.needs[0].what, "FFmpeg, to put the video back together");
    let opus = rig.dir.path().join("talk.opus");
    std::fs::write(&opus, b"not really").unwrap();
    assert_eq!(
        rig.service.plan(&PlanInput::File(opus), "bn", true).needs[0].what,
        "FFmpeg, to read .opus files"
    );

    {
        let mut facts = rig.runtime.facts.lock();
        facts.translator = None;
        facts.speech_model = None;
    }
    let plan = rig.service.plan(&PlanInput::Microphone, "bn", true);
    assert_eq!(
        plan.problem.as_deref(),
        Some("No model to translate with is installed. Download one in Settings > Models.")
    );
    assert_eq!(
        (plan.needs[0].what.as_str(), plan.needs[0].bytes),
        ("the speech model", 700)
    );
    let err = rig
        .service
        .submit_file(&file, None, "de", true)
        .unwrap_err();
    assert!(err.starts_with("No model to translate with"), "{err}");
}

#[tokio::test]
async fn one_download_brings_in_everything_missing() {
    let rig = rig_with(
        FakeSpeaker {
            fails_for: None,
            waits: false,
            requests: Mutex::new(Vec::new()),
        },
        &["clone"],
    );
    rig.runtime.facts.lock().speech_model = None;
    let opus = rig.dir.path().join("talk.opus");
    std::fs::write(&opus, b"not really").unwrap();
    let input = PlanInput::File(opus);
    let before = rig.service.plan(&input, "de", true);
    assert_eq!(before.needs.len(), 2, "{:?}", before.needs);
    assert_eq!(before.total_bytes, 780);
    rig.service.start_install(&input, "de", true).unwrap();
    for _ in 0..200 {
        if rig.service.install().is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(rig.service.install(), None);
    assert!(rig.service.plan(&input, "de", true).ready);
}

#[test]
fn whisper_segments_become_lines_in_the_whole() {
    let reply = json!({"segments": [
        {"start": 1.0, "end": 2.0, "text": " Hi. "},
        {"start": 2.0, "end": 3.0, "text": "(applause)"},
        {"start": 3.0, "end": 4.0, "text": " ... "},
        {"start": 4.0, "end": 5.0, "text": ""},
    ]});
    let lines = segments_of(&reply, 300.0);
    assert_eq!(lines, vec![Segment::new(301.0, 302.0, "Hi.")]);
    let run = Run {
        input_name: "my.talk.mp3".into(),
        ..Run::default()
    };
    assert_eq!(base_name(&run), "my.talk");
    assert!(plain_id("flow_abc") && !plain_id("../x") && !plain_id(""));
    let progress = Run {
        status: Status::Running,
        stage: Some(Stage::Speaking),
        done: 1,
        total: 2,
        ..Run::default()
    };
    assert!((progress.progress() - 0.75).abs() < 1e-9);
}

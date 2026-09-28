//! Whole runs of the Nooklets on the flows' queue (Transcribe, Summarize, Read aloud) against a
//! scripted Whisper, chat model, voice and document reader.

use super::*;
use crate::flow::audio::tests::write_tone;
use crate::flow::reader::ReaderNeed;
use crate::flow::voice_engine::SpokenProgress;
use crate::flow::voices::VoiceFile;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whisper hears a short meeting; the chat model answers every question with the same notes and
/// remembers what it was told.
struct Scripted {
    facts: Mutex<Facts>,
    asked: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl FlowRuntime for Scripted {
    fn facts(&self) -> Facts {
        self.facts.lock().clone()
    }
    async fn transcribe(
        &self,
        _wav: &Path,
        _model: &str,
        _language: Option<&str>,
    ) -> Result<Value> {
        Ok(json!({"language": "english", "segments": [
            {"start": 0.0, "end": 0.8, "text": " Good morning, everyone."},
            {"start": 0.9, "end": 1.6, "text": " Anna sends the budget by Friday."},
            {"start": 4.0, "end": 4.6, "text": " [Applause]"},
        ]}))
    }
    async fn chat(&self, _model: &str, system: &str, user: &str) -> Result<String> {
        self.asked
            .lock()
            .push((system.to_string(), user.to_string()));
        Ok(
            "```markdown\n# The weekly meeting\n\n## To do\n- Anna: the budget, by Friday\n```"
                .into(),
        )
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
        progress(10, 10);
        Ok(true)
    }
    async fn download_model(
        &self,
        _id: &str,
        progress: Progress,
        _cancel: &CancellationToken,
    ) -> Result<bool> {
        progress(700, 700);
        Ok(true)
    }
}

/// A tone per line, and what it was asked; a line with one of `skips`' words in it is skipped
/// as many times as that says (an engine that chokes on a word).
struct Voice1 {
    asked: Mutex<Vec<(String, bool, Vec<String>)>>,
    skips: Mutex<Vec<(&'static str, usize)>>,
}

#[async_trait]
impl Speaker for Voice1 {
    async fn speak(
        &self,
        r: &Request,
        progress: &SpokenProgress<'_>,
        _cancel: &CancellationToken,
    ) -> Result<Vec<Option<PathBuf>>> {
        self.asked.lock().push((
            r.voice.id.clone(),
            r.female_speaker,
            r.lines.iter().map(|l| l.text.clone()).collect(),
        ));
        std::fs::create_dir_all(&r.out_dir)?;
        let mut out = Vec::new();
        for (i, l) in r.lines.iter().enumerate() {
            let skipped = self
                .skips
                .lock()
                .iter_mut()
                .find(|(word, left)| *left > 0 && l.text.contains(word))
                .map(|s| s.1 -= 1)
                .is_some();
            if skipped {
                out.push(None);
                continue;
            }
            let wav = r.out_dir.join(format!("{}.wav", l.id));
            write_tone(&wav, r.voice.sample_rate, 1, 0.5, 220.0);
            out.push(Some(wav));
            progress(i + 1, r.lines.len());
        }
        Ok(out)
    }
}

/// Reads a `.docx` as fixed Markdown once "Pandoc" is in.
struct Docs {
    pandoc_in: AtomicBool,
}

#[async_trait]
impl Reader for Docs {
    fn needs(&self, path: &Path) -> std::result::Result<Vec<ReaderNeed>, String> {
        if !path.to_string_lossy().ends_with(".docx") {
            return Err("Nook does not read .xyz files.".into());
        }
        Ok(if self.pandoc_in.load(Ordering::SeqCst) {
            Vec::new()
        } else {
            vec![ReaderNeed {
                component: EngineComponent::Pandoc,
                what: "the document engine (Pandoc)".into(),
                bytes: 42,
            }]
        })
    }
    async fn read(
        &self,
        _path: &Path,
        _work: &Path,
        _cancel: &CancellationToken,
    ) -> Result<String> {
        Ok("# Lease\n\nThe tenant pays **€900** a month. Notice is three months.".into())
    }
}

fn voice(id: &str, clones: bool, designs: bool, langs: &[&str]) -> Voice {
    Voice {
        id: id.into(),
        family: if clones { "qwen3_tts" } else { "supertonic" }.into(),
        name: id.to_uppercase(),
        clones,
        designs,
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
    voice: Arc<Voice1>,
    docs: Arc<Docs>,
    service: Arc<FlowService>,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let runtime = Arc::new(Scripted {
        facts: Mutex::new(Facts {
            speech_model: Some("whisper-large-v3-turbo".into()),
            speech_engine_installed: true,
            speech_download: None,
            speech_engine_bytes: 0,
            translator: Some(("qwen3-4b".into(), "Qwen3 4B".into())),
            ffmpeg: None,
            ffmpeg_bytes: 80,
            voice_engine: None,
            voice_engine_bytes: 60,
            voice_backend: "cuda".into(),
            cpu: false,
        }),
        asked: Mutex::new(Vec::new()),
    });
    let voices_dir = dir.path().join("voices");
    std::fs::create_dir_all(&voices_dir).unwrap();
    std::fs::write(voices_dir.join("tonic.gguf"), b"gguf").unwrap();
    let speaker = Arc::new(Voice1 {
        asked: Mutex::new(Vec::new()),
        skips: Mutex::new(Vec::new()),
    });
    let service = FlowService::new(
        runtime.clone(),
        dir.path().join("flows"),
        voices_dir,
        dir.path().join("tmp"),
        Voices::new(vec![
            voice("clone", true, false, &["en", "de", "ja"]),
            voice("tonic", false, false, &["en", "de"]),
        ]),
        Some(speaker.clone()),
    );
    let docs = Arc::new(Docs {
        pandoc_in: AtomicBool::new(false),
    });
    service.set_reader(docs.clone());
    Rig {
        dir,
        runtime,
        voice: speaker,
        docs,
        service,
    }
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

fn order(flow: &str) -> Order {
    Order {
        flow: flow.into(),
        ..Order::default()
    }
}

fn file_names(run: &Run) -> Vec<String> {
    run.files
        .iter()
        .map(|f| {
            Path::new(f)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[tokio::test]
async fn a_recording_is_written_down_with_notes_and_subtitles() {
    let rig = rig();
    let file = rig.dir.path().join("standup.wav");
    write_tone(&file, 16_000, 1, 5.0, 150.0);
    let o = Order {
        notes: true,
        focus: Some("  who does what ".into()),
        ..order(TRANSCRIBE)
    };
    let plan = rig.service.plan_for(&PlanInput::File(file.clone()), &o);
    assert!(plan.ready, "{plan:?}");
    assert_eq!(plan.model_name.as_deref(), Some("Qwen3 4B"));
    assert!(plan.spoken_with.contains("writes the notes"));

    let run = rig.service.submit_for(&PlanInput::File(file), &o).unwrap();
    assert_eq!((run.flow.as_str(), run.notes), (TRANSCRIBE, true));
    assert_eq!(run.focus.as_deref(), Some("who does what"));
    let done = finished(&rig.service, &run.id).await;
    assert_eq!(done.status, Status::Done, "{:?}", done.error);
    assert_eq!(done.segments.len(), 2, "[Applause] is not speech");
    assert_eq!(done.detected_language.as_deref(), Some("en"));
    assert_eq!(done.words, 9);
    assert_eq!(
        done.summary.as_deref(),
        Some("# The weekly meeting\n\n## To do\n- Anna: the budget, by Friday")
    );
    let asked = rig.runtime.asked.lock();
    assert_eq!(asked.len(), 1);
    assert!(
        asked[0].0.contains("transcript of a recording")
            && asked[0].0.contains("Write in English.")
    );
    assert!(asked[0].1.contains("## Decisions") && asked[0].1.contains("who does what"));
    assert!(asked[0]
        .1
        .contains("Good morning, everyone. Anna sends the budget by Friday."));
    assert_eq!(
        file_names(&done),
        vec![
            "standup.txt",
            "standup (timed).txt",
            "standup.srt",
            "standup.vtt",
            "standup notes.md"
        ]
    );
    let folder = rig.service.folder(&run.id);
    let vtt = std::fs::read_to_string(folder.join("standup.vtt")).unwrap();
    assert!(vtt.starts_with("WEBVTT") && vtt.contains("00:00:00.900 --> 00:00:01.600"));
    assert!(folder.join("run.json").is_file());

    // After a restart the run is still there, with its notes.
    let again = FlowService::new(
        rig.runtime.clone(),
        rig.dir.path().join("flows"),
        rig.dir.path().join("voices"),
        rig.dir.path().join("tmp"),
        Voices::default(),
        None,
    );
    let back = again.run(&run.id).unwrap();
    assert_eq!((back.flow.as_str(), back.files.len()), (TRANSCRIBE, 5));
    assert!(back.summary.is_some());
}

#[tokio::test]
async fn notes_want_a_chat_model_and_a_transcript_does_not() {
    let rig = rig();
    rig.runtime.facts.lock().translator = None;
    let file = rig.dir.path().join("call.wav");
    write_tone(&file, 16_000, 1, 1.0, 150.0);
    let with_notes = Order {
        notes: true,
        ..order(TRANSCRIBE)
    };
    let plan = rig
        .service
        .plan_for(&PlanInput::File(file.clone()), &with_notes);
    assert!(plan.problem.unwrap().contains("No chat model"));
    let plain = rig
        .service
        .plan_for(&PlanInput::File(file), &order(TRANSCRIBE));
    assert!(plain.ready && plain.model_name.is_none());
    assert!(rig
        .service
        .plan_for(&PlanInput::Text("hi".into()), &order(TRANSCRIBE))
        .problem
        .is_some());
}

#[tokio::test]
async fn pasted_text_is_summarized_and_kept_for_run_again() {
    let rig = rig();
    let o = Order {
        language: Some("de".into()),
        length: Length::Detailed,
        ..order(SUMMARIZE)
    };
    let text = "The tenant pays €900 a month.\n\nNotice is three months.";
    let run = rig
        .service
        .submit_for(&PlanInput::Text(text.into()), &o)
        .unwrap();
    assert_eq!(run.source, Source::Text);
    assert_eq!(run.input_name, "The tenant pays €900 a month. Notice is…");
    assert_eq!(run.target_language, "de");
    let done = finished(&rig.service, &run.id).await;
    assert_eq!(done.status, Status::Done, "{:?}", done.error);
    assert_eq!(done.words, 10);
    assert!(done
        .summary
        .as_deref()
        .unwrap()
        .starts_with("# The weekly meeting"));
    assert_eq!(file_names(&done), vec!["text summary.md"]);
    let asked = rig.runtime.asked.lock().clone();
    assert!(
        asked[0].0.contains("summaries of documents") && asked[0].0.contains("Write in German.")
    );
    assert!(
        asked[0].1.contains("a ## section for each main part")
            && asked[0].1.contains("Notice is three months.")
    );
    drop(asked);

    let second = rig.service.again(&run.id).unwrap();
    let done2 = finished(&rig.service, &second.id).await;
    assert_eq!(done2.status, Status::Done, "{:?}", done2.error);
    assert_eq!(
        (done2.length, done2.target_language.as_str()),
        (Length::Detailed, "de")
    );
}

#[tokio::test]
async fn a_document_needs_its_engine_before_it_is_summarized() {
    let rig = rig();
    let doc = rig.dir.path().join("lease.docx");
    std::fs::write(&doc, b"PK").unwrap();
    let plan = rig
        .service
        .plan_for(&PlanInput::File(doc.clone()), &order(SUMMARIZE));
    assert_eq!(plan.needs.len(), 1);
    assert_eq!(plan.needs[0].what, "the document engine (Pandoc)");
    assert!(rig
        .service
        .submit_for(&PlanInput::File(doc.clone()), &order(SUMMARIZE))
        .unwrap_err()
        .contains("download first"));
    let odd = rig.dir.path().join("x.xyz");
    std::fs::write(&odd, b"?").unwrap();
    assert_eq!(
        rig.service
            .plan_for(&PlanInput::File(odd), &order(SUMMARIZE))
            .problem
            .as_deref(),
        Some("Nook does not read .xyz files.")
    );

    rig.docs.pandoc_in.store(true, Ordering::SeqCst);
    let run = rig
        .service
        .submit_for(&PlanInput::File(doc), &order(SUMMARIZE))
        .unwrap();
    let done = finished(&rig.service, &run.id).await;
    assert_eq!(done.status, Status::Done, "{:?}", done.error);
    assert_eq!(file_names(&done), vec!["lease summary.md"]);
    let asked = rig.runtime.asked.lock();
    assert!(
        asked[0].1.contains("The tenant pays **€900** a month.")
            && asked[0].1.contains("lease.docx")
    );
}

#[tokio::test]
async fn text_is_read_aloud_line_after_line_into_one_track() {
    let rig = rig();
    let text =
        "# Chapter one\n\nIt was a bright cold day in April. The clocks were striking thirteen.";
    let o = Order {
        language: Some("en".into()),
        female: false,
        ..order(READ_ALOUD)
    };
    let plan = rig.service.plan_for(&PlanInput::Text(text.into()), &o);
    assert!(plan.ready, "{plan:?}");
    assert_eq!(
        plan.voice_name.as_deref(),
        Some("TONIC"),
        "a standard voice, not the cloning one"
    );
    assert!(plan.spoken_with.contains("man's voice"));

    let run = rig
        .service
        .submit_for(&PlanInput::Text(text.into()), &o)
        .unwrap();
    let done = finished(&rig.service, &run.id).await;
    assert_eq!(done.status, Status::Done, "{:?}", done.error);
    let asked = rig.voice.asked.lock().clone();
    assert_eq!(
        asked,
        vec![(
            "tonic".to_string(),
            false,
            vec![
                "Chapter one".to_string(),
                "It was a bright cold day in April. The clocks were striking thirteen.".to_string()
            ]
        )]
    );
    assert_eq!(done.segments.len(), 2);
    assert!(
        done.segments[1].start > done.segments[0].end,
        "one line after the other"
    );
    assert!((done.duration_seconds - done.segments[1].end).abs() < 1e-9);
    let track = PathBuf::from(done.audio.clone().unwrap());
    assert_eq!(
        track.file_name().unwrap(),
        "text.wav",
        "no FFmpeg, so a WAV"
    );
    assert!(audio::wav_seconds(&track).unwrap() > 1.0);
    assert_eq!(done.voice_name.as_deref(), Some("TONIC"));
    assert_eq!(done.words, 15);
}

#[tokio::test]
async fn reading_aloud_wants_a_voice_for_the_language() {
    let rig = rig();
    let text = PlanInput::Text("Bonjour à tous.".into());
    let french = Order {
        language: Some("fr".into()),
        ..order(READ_ALOUD)
    };
    assert_eq!(
        rig.service.plan_for(&text, &french).problem.as_deref(),
        Some("Nook has no voice that reads French yet.")
    );
    let japanese = Order {
        language: Some("ja".into()),
        ..order(READ_ALOUD)
    };
    assert!(
        rig.service.plan_for(&text, &japanese).problem.is_some(),
        "only a cloning voice speaks it, and there is no one to clone"
    );
    assert_eq!(
        rig.service
            .plan_for(&text, &order(READ_ALOUD))
            .problem
            .as_deref(),
        Some("Choose the language the text is in.")
    );
    // A voice not downloaded yet is one download, with the voice engine when that is missing.
    std::fs::remove_file(rig.dir.path().join("voices").join("tonic.gguf")).unwrap();
    let english = Order {
        language: Some("en".into()),
        ..order(READ_ALOUD)
    };
    let plan = rig.service.plan_for(&text, &english);
    assert_eq!(
        plan.needs
            .iter()
            .map(|n| n.what.as_str())
            .collect::<Vec<_>>(),
        vec!["the TONIC voice"]
    );
}

#[tokio::test]
async fn a_look_before_a_run_tells_the_language_and_the_words() {
    let rig = rig();
    let peek = rig
        .service
        .peek(&PlanInput::Text(
            "Der Mieter zahlt die Miete am ersten Tag jedes Monats an den Vermieter.".into(),
        ))
        .await;
    assert_eq!(peek.language.as_deref(), Some("de"));
    assert_eq!(peek.words, 13);
    let doc = rig.dir.path().join("lease.docx");
    std::fs::write(&doc, b"PK").unwrap();
    assert_eq!(
        rig.service.peek(&PlanInput::File(doc.clone())).await,
        Peek::default(),
        "Pandoc is not in"
    );
    rig.docs.pandoc_in.store(true, Ordering::SeqCst);
    assert_eq!(rig.service.peek(&PlanInput::File(doc)).await.words, 11);
}

#[tokio::test]
async fn a_line_the_voice_skips_is_asked_for_again_and_one_it_never_speaks_is_said() {
    let rig = rig();
    *rig.voice.skips.lock() = vec![("hiccup", 1), ("never", 9)];
    let o = Order {
        language: Some("en".into()),
        ..order(READ_ALOUD)
    };
    let text = "A line with a hiccup in it.\n\nA line it will never say.\n\nA plain line.";
    let run = rig
        .service
        .submit_for(&PlanInput::Text(text.into()), &o)
        .unwrap();
    let done = finished(&rig.service, &run.id).await;
    assert_eq!(done.status, Status::Done, "{:?}", done.error);
    let asked = rig.voice.asked.lock().clone();
    assert_eq!(asked.len(), 2, "once for all, once more for the skipped");
    assert_eq!(
        asked[1].2,
        vec![
            "A line with a hiccup in it.".to_string(),
            "A line it will never say.".to_string()
        ]
    );
    assert_eq!(
        done.segments.len(),
        2,
        "the hiccup came back; the other is silent"
    );
    assert_eq!(
        done.note.as_deref(),
        Some("1 of 3 lines could not be spoken, so it is silent in the track.")
    );
}

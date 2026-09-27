//! The Nooklets that share the flows' queue with the translator, so no two runs want the card
//! at once: Transcribe (a recording written down, with notes when asked), Summarize (a document
//! or pasted text, by the chat model) and Read aloud (a document or pasted text, spoken by a
//! standard voice as one track). A child of `service.rs`, working on the service's own state.
//!
//! Each run is asked for with an [`Order`]; [`FlowService::plan_for`] says what it would do and
//! still needs, [`FlowService::start_install_for`] downloads that, [`FlowService::submit_for`]
//! (a file or pasted text) and [`FlowService::submit_recording_for`] (the microphone, for
//! Transcribe) queue it.

use super::*;
use crate::flow::aloud;
use crate::flow::reader;
use crate::flow::summarize::{self, Brief, Kind};

/// A transcript starts a new paragraph after a pause this long.
pub const PARAGRAPH_PAUSE: f64 = 2.0;
/// The most pasted text a run takes, in bytes.
pub const MAX_PASTED: usize = 4_000_000;
/// Read aloud's words a minute, for how long a reading will be.
pub const WORDS_A_MINUTE: u32 = 150;
/// How long FFmpeg may take to pack a reading.
const ENCODE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

const NO_CHAT_MODEL: &str =
    "No chat model is installed. Download one in Settings > Models, and it writes this.";

/// What a document or pasted text holds before a run: its language, when it can be told, and
/// how many words it has.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Peek {
    pub language: Option<String>,
    pub words: u32,
}

impl FlowService {
    /// What a Nooklet's run with these inputs would do and still needs. Opens a chosen file's
    /// headers: call it on the blocking pool.
    pub fn plan_for(&self, input: &PlanInput, order: &Order) -> Plan {
        let facts = self.runtime.facts();
        self.plan_for_with(&facts, input, order).0
    }

    fn plan_for_with(
        &self,
        facts: &Facts,
        input: &PlanInput,
        order: &Order,
    ) -> (Plan, Option<Choice>, Vec<NeedKind>) {
        let mut problem: Option<String>;
        let mut needs: Vec<Need> = Vec::new();
        let mut choice: Option<Choice> = None;
        let mut about = String::new();
        let model = facts.translator.clone();
        let model_word = model
            .as_ref()
            .map_or_else(|| "The chat model".to_string(), |m| m.1.clone());
        let mut uses_model = false;
        match order.flow.as_str() {
            TRANSCRIBE => {
                problem = match input {
                    PlanInput::Nothing | PlanInput::Text(_) => {
                        Some("Choose a recording, or record one.".to_string())
                    }
                    PlanInput::File(p) if !p.is_file() => {
                        Some(format!("{} is not there any more.", audio::display_name(p)))
                    }
                    _ => None,
                };
                if problem.is_none() && order.notes && model.is_none() {
                    problem = Some(NO_CHAT_MODEL.into());
                }
                speech_needs(facts, &mut needs);
                if let PlanInput::File(p) = input {
                    if facts.ffmpeg.is_none() && p.is_file() && !audio::readable(p) {
                        needs.push(Need {
                            what: format!("FFmpeg, to read .{} files", audio::extension(p)),
                            bytes: facts.ffmpeg_bytes,
                            kind: NeedKind::Ffmpeg,
                        });
                    }
                }
                uses_model = order.notes;
                about = if order.notes {
                    format!("Whisper writes it down, and {model_word} writes the notes.")
                } else {
                    "Whisper writes it down, with the time of every line.".into()
                };
            }
            SUMMARIZE => {
                problem = self.document_problem(input, &mut needs);
                if problem.is_none() && model.is_none() {
                    problem = Some(NO_CHAT_MODEL.into());
                }
                uses_model = true;
                about = format!("{model_word} writes the summary.");
            }
            READ_ALOUD => {
                problem = self.document_problem(input, &mut needs);
                match languages::code_of(order.language.as_deref()) {
                    None => {
                        if problem.is_none() {
                            problem = Some("Choose the language the text is in.".into());
                        }
                    }
                    Some(code) => match self.reading_voice(&code) {
                        None => {
                            if problem.is_none() {
                                problem = Some(format!(
                                    "Nook has no voice that reads {} yet.",
                                    languages::name_of(&code)
                                ));
                            }
                        }
                        Some(c) => {
                            if facts.voice_engine.is_none() && self.speaker.is_none() {
                                needs.push(Need {
                                    what: "the voice engine".into(),
                                    bytes: facts.voice_engine_bytes,
                                    kind: NeedKind::VoiceEngine,
                                });
                            }
                            if !self.voice_installed(&c.voice) {
                                needs.push(Need {
                                    what: format!("the {} voice", c.voice.name),
                                    bytes: c.voice.file.bytes,
                                    kind: NeedKind::Voice(c.voice.id.clone()),
                                });
                            }
                            about = if c.voice.family == "supertonic" {
                                format!(
                                    "Read by {} in a {} voice.",
                                    c.voice.name,
                                    if order.female { "woman's" } else { "man's" }
                                )
                            } else {
                                format!("Read by {}.", c.voice.name)
                            };
                            choice = Some(c);
                        }
                    },
                }
            }
            other => problem = Some(format!("Nook has no Nooklet called {other}.")),
        }
        let kinds = needs.iter().map(|n| n.kind.clone()).collect();
        let total_bytes = needs.iter().map(|n| n.bytes).sum();
        let ready = problem.is_none() && needs.is_empty();
        let plan = Plan {
            voice_name: choice.as_ref().map(|c| c.voice.name.clone()),
            cloned: false,
            spoken_with: about,
            no_voice: order.flow == READ_ALOUD && choice.is_none(),
            needs,
            total_bytes,
            problem,
            ready,
            model_id: model.as_ref().filter(|_| uses_model).map(|m| m.0.clone()),
            model_name: model.as_ref().filter(|_| uses_model).map(|m| m.1.clone()),
        };
        (plan, choice, kinds)
    }

    /// The voice that reads `language`: one with its own voices, or one that makes a voice up,
    /// never one that only clones (there is no one to clone).
    fn reading_voice(&self, language: &str) -> Option<Choice> {
        self.voices.pick(language, false).filter(|c| !c.cloned)
    }

    /// Why a document or pasted text cannot be read, and the engines it needs downloaded.
    fn document_problem(&self, input: &PlanInput, needs: &mut Vec<Need>) -> Option<String> {
        match input {
            PlanInput::Nothing | PlanInput::Microphone => {
                Some("Choose a document, or paste text.".into())
            }
            PlanInput::Text(t) if t.trim().is_empty() => Some("Paste some text first.".into()),
            PlanInput::Text(t) if t.len() > MAX_PASTED => Some(
                "That is a lot of text to paste. Save it as a file and choose the file instead."
                    .into(),
            ),
            PlanInput::Text(_) => None,
            PlanInput::File(p) if !p.is_file() => {
                Some(format!("{} is not there any more.", audio::display_name(p)))
            }
            PlanInput::File(p) if reader::is_plain(p) => None,
            PlanInput::File(p) => match self.reader.get() {
                None => Some("Nook cannot read documents here.".into()),
                Some(r) => match r.needs(p) {
                    Err(why) => Some(why),
                    Ok(engines) => {
                        for e in engines {
                            needs.push(Need {
                                what: e.what,
                                bytes: e.bytes,
                                kind: NeedKind::Component(e.component),
                            });
                        }
                        None
                    }
                },
            },
        }
    }

    /// Downloads everything [`plan_for`](Self::plan_for) says is missing, in the background.
    pub fn start_install_for(&self, input: &PlanInput, order: &Order) -> Result<(), String> {
        if self.stopping.is_cancelled() {
            return Err("Nook is closing.".into());
        }
        if self
            .install
            .lock()
            .as_ref()
            .is_some_and(|i| i.error.is_none())
        {
            return Ok(());
        }
        let facts = self.runtime.facts();
        let (plan, choice, kinds) = self.plan_for_with(&facts, input, order);
        self.begin_install(plan, choice.map(|c| c.voice), kinds)
    }

    fn check_for(&self, input: &PlanInput, order: &Order) -> Result<(), String> {
        if self.stopping.is_cancelled() {
            return Err("Nook is closing.".into());
        }
        let plan = self.plan_for(input, order);
        if let Some(problem) = plan.problem {
            return Err(problem);
        }
        if let Some(need) = plan.needs.first() {
            return Err(format!("This needs a download first: {}.", need.what));
        }
        Ok(())
    }

    /// Queues a Nooklet's run on a file, or on pasted text (kept in the run's folder).
    pub fn submit_for(&self, input: &PlanInput, order: &Order) -> Result<Run, String> {
        self.check_for(input, order)?;
        let id = new_id();
        let (path, name, source) = match input {
            PlanInput::File(p) => {
                let p = std::path::absolute(p).unwrap_or_else(|_| p.clone());
                let name = audio::display_name(&p);
                (p, name, Source::File)
            }
            PlanInput::Text(t) => {
                let kept = self.folder(&id).join("text.txt");
                std::fs::create_dir_all(self.folder(&id))
                    .and_then(|_| std::fs::write(&kept, t))
                    .map_err(|e| format!("Could not keep the text: {e}"))?;
                (kept, first_words(t), Source::Text)
            }
            _ => return Err("Choose a file.".into()),
        };
        let run = self.nooklet_run(id, &path, name, source, order)?;
        self.queue_run(run)
    }

    /// Queues the transcript of what was just said into the microphone: `recording` moves into
    /// the run's folder.
    pub fn submit_recording_for(&self, recording: &Path, order: &Order) -> Result<Run, String> {
        if order.flow != TRANSCRIBE {
            return Err("Only a transcript is made from the microphone.".into());
        }
        self.check_for(&PlanInput::Microphone, order)?;
        let id = new_id();
        let kept = self.folder(&id).join("recording.wav");
        move_file(recording, &kept).map_err(|e| format!("{e:#}"))?;
        let run = self.nooklet_run(id, &kept, "Recording".into(), Source::Microphone, order)?;
        self.queue_run(run)
    }

    /// Run again and Try again, for a Nooklet's run.
    pub(super) fn again_nooklet(&self, old: &Run) -> Result<Run, String> {
        let language = match old.flow.as_str() {
            TRANSCRIBE => old.source_language.clone(),
            _ => Some(old.target_language.clone()).filter(|l| !l.is_empty()),
        };
        let order = Order {
            flow: old.flow.clone(),
            language,
            notes: old.notes,
            length: old.length,
            focus: old.focus.clone(),
            female: old.female,
        };
        match old.source {
            Source::File => self.submit_for(&PlanInput::File(PathBuf::from(&old.input)), &order),
            Source::Text => {
                let text = reader::read_plain(Path::new(&old.input))
                    .map_err(|_| "The text is not there any more.".to_string())?;
                self.submit_for(&PlanInput::Text(text), &order)
            }
            Source::Microphone => {
                self.check_for(&PlanInput::Microphone, &order)?;
                let from = PathBuf::from(&old.input);
                if !from.is_file() {
                    return Err("The recording is not there any more.".into());
                }
                let new = new_id();
                let kept = self.folder(&new).join("recording.wav");
                std::fs::create_dir_all(self.folder(&new))
                    .and_then(|_| std::fs::copy(&from, &kept))
                    .map_err(|e| format!("Could not copy the recording: {e}"))?;
                let run = self.nooklet_run(
                    new,
                    &kept,
                    old.input_name.clone(),
                    Source::Microphone,
                    &order,
                )?;
                self.queue_run(run)
            }
        }
    }

    fn nooklet_run(
        &self,
        id: String,
        input: &Path,
        input_name: String,
        source: Source,
        order: &Order,
    ) -> Result<Run, String> {
        let language = languages::code_of(order.language.as_deref());
        let (source_language, target_language) = match order.flow.as_str() {
            TRANSCRIBE => (language, String::new()),
            SUMMARIZE => (None, language.unwrap_or_default()),
            READ_ALOUD => (
                None,
                language.ok_or_else(|| "Choose the language the text is in.".to_string())?,
            ),
            other => return Err(format!("Nook has no Nooklet called {other}.")),
        };
        let notes = order.flow == TRANSCRIBE && order.notes;
        let uses_model = order.flow == SUMMARIZE || notes;
        Ok(Run {
            id,
            flow: order.flow.clone(),
            input: input.display().to_string(),
            input_name,
            source,
            source_language,
            target_language,
            model_id: uses_model
                .then(|| self.runtime.facts().translator.map(|t| t.0))
                .flatten(),
            keep_voice: false,
            status: Status::Queued,
            created_at: Utc::now(),
            notes,
            length: order.length,
            focus: order
                .focus
                .as_deref()
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .map(str::to_string),
            female: order.female,
            ..Run::default()
        })
    }

    /// The language and the length of a document or pasted text, before a run: for the Read
    /// aloud picker and the card. Reads the whole document; a file whose engine is not in yet
    /// tells nothing.
    pub async fn peek(&self, input: &PlanInput) -> Peek {
        let text = match input {
            PlanInput::Text(t) => Some(t.clone()),
            PlanInput::File(p) if reader::is_plain(p) => {
                let p = p.clone();
                blocking(move || reader::read_plain(&p)).await.ok()
            }
            PlanInput::File(p) => match self.reader.get() {
                Some(r) if r.needs(p).is_ok_and(|n| n.is_empty()) => {
                    let work = self.temp.join(format!("peek-{}", new_id()));
                    let text = r.read(p, &work, &self.stopping.child_token()).await.ok();
                    let _ =
                        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(work)).await;
                    text
                }
                _ => None,
            },
            _ => None,
        };
        match text {
            Some(t) => Peek {
                language: aloud::detect_language(&t),
                words: aloud::word_count(&t),
            },
            None => Peek::default(),
        }
    }

    // ------------------------------------------------------------------ the runs

    /// The words of a Summarize or Read aloud run's document or text.
    async fn read_input(
        &self,
        run: &Run,
        work: &Path,
        cancel: &CancellationToken,
    ) -> Result<String> {
        self.stage(&run.id, Stage::Reading, 0, 0);
        let input = PathBuf::from(&run.input);
        let text = if run.source == Source::Text || reader::is_plain(&input) {
            let p = input.clone();
            blocking(move || reader::read_plain(&p)).await?
        } else {
            let reader = self
                .reader
                .get()
                .ok_or_else(|| anyhow!("Nook cannot read documents here."))?
                .clone();
            reader.read(&input, &work.join("read"), cancel).await?
        };
        stop_if(cancel)?;
        if !text.chars().any(char::is_alphanumeric) {
            bail!("Nook found no words in {}.", run.input_name);
        }
        let words = aloud::word_count(&text);
        self.update(&run.id, move |r| r.words = words);
        Ok(text)
    }

    /// The chat model the run uses: the one it was queued with while it is still there, else
    /// the one chosen now.
    fn run_model(&self, run: &Run, facts: &Facts) -> Result<String> {
        let now = facts.translator.clone().map(|t| t.0);
        run.model_id
            .clone()
            .filter(|m| now.as_deref() == Some(m.as_str()))
            .or(now)
            .ok_or_else(|| anyhow!("No chat model is installed."))
    }

    async fn write_summary(
        &self,
        run: &Run,
        text: &str,
        brief: &Brief,
        model: &str,
        cancel: &CancellationToken,
    ) -> Result<String> {
        let chat = RunChat {
            runtime: self.runtime.as_ref(),
            model: model.to_string(),
            system: summarize::system(brief),
        };
        let id = run.id.as_str();
        self.stage(id, Stage::Summarizing, 0, 1);
        summarize::summarize(text, brief, &chat, cancel, &|d, t| {
            self.stage(id, Stage::Summarizing, d, t)
        })
        .await
    }

    pub(super) async fn transcribe(
        &self,
        run: &Run,
        work: &Path,
        cancel: &CancellationToken,
    ) -> Result<Run> {
        let id = run.id.as_str();
        let facts = self.runtime.facts();
        let input = PathBuf::from(&run.input);
        let (wav, duration) = self
            .prepare_audio(run, &input, work, &facts, cancel)
            .await?;
        let (heard, detected) = self.listen(run, &wav, work, &facts, cancel).await?;
        let text = subtitles::paragraphs(&heard, PARAGRAPH_PAUSE);
        let words = aloud::word_count(&text);
        {
            let (heard, detected) = (heard.clone(), detected.clone());
            self.update(id, move |r| {
                r.detected_language = detected;
                r.duration_seconds = duration;
                r.segments = heard;
                r.words = words;
            });
        }

        // The notes, when asked for: a failure keeps the transcript and says why.
        let mut summary = None;
        let mut note = None;
        let mut model_id = run.model_id.clone();
        if run.notes {
            let spoken = detected
                .as_deref()
                .or(run.source_language.as_deref())
                .and_then(languages::by_code)
                .map(|l| l.name.to_string());
            let brief = Brief {
                kind: Kind::Talk,
                length: run.length,
                language: spoken,
                focus: run.focus.clone(),
                title: if run.source == Source::Microphone {
                    String::new()
                } else {
                    run.input_name.clone()
                },
            };
            let written = match self.run_model(run, &facts) {
                Ok(model) => {
                    model_id = Some(model.clone());
                    self.write_summary(run, &text, &brief, &model, cancel).await
                }
                Err(e) => Err(e),
            };
            match written {
                Ok(s) => summary = Some(s),
                Err(e) if e.is::<Stopped>() || cancel.is_cancelled() => return Err(e),
                Err(e) => {
                    tracing::warn!("Run {id}: the notes could not be written: {e:#}");
                    note = Some(format!("The notes could not be written: {e:#}"));
                }
            }
        }
        stop_if(cancel)?;

        // Save.
        self.stage(id, Stage::Saving, 0, 0);
        let folder = self.folder(id);
        let base = base_name(run);
        let mut files = vec![
            (folder.join(format!("{base}.txt")), text.clone()),
            (
                folder.join(format!("{base} (timed).txt")),
                subtitles::timed(&heard, PARAGRAPH_PAUSE),
            ),
            (
                folder.join(format!("{base}.srt")),
                subtitles::srt(&heard, false),
            ),
            (folder.join(format!("{base}.vtt")), subtitles::vtt(&heard)),
        ];
        if let Some(s) = &summary {
            files.push((folder.join(format!("{base} notes.md")), format!("{s}\n")));
        }
        let now = self.current(id);
        let done = Run {
            model_id,
            note,
            status: Status::Done,
            stage: None,
            done: 0,
            total: 0,
            detected_language: detected,
            duration_seconds: duration,
            segments: heard,
            summary,
            words,
            files: files.iter().map(|(p, _)| p.display().to_string()).collect(),
            elapsed_ms: elapsed_since(now.started_at),
            error: None,
            started_at: now.started_at,
            ..run.clone()
        };
        save(&done, &folder, files).await?;
        Ok(done)
    }

    pub(super) async fn summarize(
        &self,
        run: &Run,
        work: &Path,
        cancel: &CancellationToken,
    ) -> Result<Run> {
        let id = run.id.as_str();
        let facts = self.runtime.facts();
        let text = self.read_input(run, work, cancel).await?;
        let model = self.run_model(run, &facts)?;
        let brief = Brief {
            kind: Kind::Document,
            length: run.length,
            language: Some(run.target_language.as_str())
                .filter(|l| !l.is_empty())
                .map(languages::name_of),
            focus: run.focus.clone(),
            title: if run.source == Source::Text {
                String::new()
            } else {
                run.input_name.clone()
            },
        };
        let summary = self
            .write_summary(run, &text, &brief, &model, cancel)
            .await?;
        stop_if(cancel)?;

        self.stage(id, Stage::Saving, 0, 0);
        let folder = self.folder(id);
        let base = base_name(run);
        let files = vec![(
            folder.join(format!("{base} summary.md")),
            format!("{summary}\n"),
        )];
        let now = self.current(id);
        let done = Run {
            model_id: Some(model),
            status: Status::Done,
            stage: None,
            done: 0,
            total: 0,
            summary: Some(summary),
            words: aloud::word_count(&text),
            files: files.iter().map(|(p, _)| p.display().to_string()).collect(),
            elapsed_ms: elapsed_since(now.started_at),
            error: None,
            started_at: now.started_at,
            ..run.clone()
        };
        save(&done, &folder, files).await?;
        Ok(done)
    }

    pub(super) async fn read_aloud(
        &self,
        run: &Run,
        work: &Path,
        cancel: &CancellationToken,
    ) -> Result<Run> {
        let id = run.id.as_str();
        let facts = self.runtime.facts();
        let language = run.target_language.clone();
        let text = self.read_input(run, work, cancel).await?;
        let lines = aloud::lines(&text);
        if lines.is_empty() {
            bail!("Nook found nothing to read aloud in {}.", run.input_name);
        }
        let choice = self.reading_voice(&language).ok_or_else(|| {
            anyhow!(
                "Nook has no voice that reads {} yet.",
                languages::name_of(&language)
            )
        })?;
        if !self.can_speak(&facts, &choice.voice) {
            bail!("The voice was not installed when the reading started.");
        }

        // Speak, line after line.
        let request = Request {
            voice: choice.voice.clone(),
            model: self.voice_file(&choice.voice),
            language: language.clone(),
            lines: lines
                .iter()
                .enumerate()
                .map(|(i, t)| Line {
                    id: format!("line-{i}"),
                    text: t.clone(),
                })
                .collect(),
            cloned: false,
            reference_wav: None,
            reference_text: None,
            female_speaker: run.female,
            out_dir: work.join("spoken"),
        };
        let clips = self.speak_with(id, &request, &facts, cancel).await?;
        stop_if(cancel)?;

        // One track, the lines one after the other.
        self.stage(id, Stage::Assembling, 0, 0);
        let folder = self.folder(id);
        tokio::fs::create_dir_all(&folder)
            .await
            .with_context(|| format!("Could not create {}", folder.display()))?;
        let base = base_name(run);
        let wav = folder.join(format!("{base}.wav"));
        let placed = {
            let (lines, clips, wav) = (
                lines
                    .iter()
                    .map(|t| Segment::new(0.0, 0.0, t.clone()))
                    .collect::<Vec<_>>(),
                clips.clone(),
                wav.clone(),
            );
            blocking(move || dub::assemble(&lines, &clips, Layout::Compact, 0.0, &wav)).await?
        };
        let segments: Vec<Segment> = placed
            .iter()
            .map(|p| Segment::new(p.start, p.start + p.seconds, lines[p.index].clone()))
            .collect();
        let duration = segments.last().map_or(0.0, |s| s.end);
        let missing = clips.iter().filter(|c| c.is_none()).count();
        let mut note = (missing > 0).then(|| {
            format!(
                "{missing} of {} lines could not be spoken and are left out.",
                lines.len()
            )
        });
        let mut track = wav.clone();
        if let Some(ffmpeg) = &facts.ffmpeg {
            let m4a = folder.join(format!("{base}.m4a"));
            match encode(ffmpeg, &wav, &m4a).await {
                Ok(()) => {
                    let _ = tokio::fs::remove_file(&wav).await;
                    track = m4a;
                }
                Err(e) => {
                    tracing::warn!("Run {id}: the reading stays a WAV: {e:#}");
                    let why = "It is kept as a WAV file, as it could not be packed smaller.";
                    note = Some(note.map_or(why.to_string(), |n| format!("{n} {why}")));
                }
            }
        }
        stop_if(cancel)?;

        self.stage(id, Stage::Saving, 0, 0);
        let now = self.current(id);
        let done = Run {
            voice_name: Some(choice.voice.name.clone()),
            note,
            status: Status::Done,
            stage: None,
            done: 0,
            total: 0,
            duration_seconds: duration,
            segments,
            audio: Some(track.display().to_string()),
            words: aloud::word_count(&text),
            files: vec![track.display().to_string()],
            elapsed_ms: elapsed_since(now.started_at),
            error: None,
            started_at: now.started_at,
            ..run.clone()
        };
        save(&done, &folder, Vec::new()).await?;
        Ok(done)
    }
}

/// Writes a run's files and its `run.json` into its folder.
async fn save(run: &Run, folder: &Path, files: Vec<(PathBuf, String)>) -> Result<()> {
    let (run, folder) = (run.clone(), folder.to_path_buf());
    blocking(move || {
        std::fs::create_dir_all(&folder)
            .with_context(|| format!("Could not create {}", folder.display()))?;
        for (path, text) in files {
            std::fs::write(&path, text)
                .with_context(|| format!("Could not write {}", path.display()))?;
        }
        let json = folder.join("run.json");
        std::fs::write(&json, serde_json::to_string_pretty(&run)?)
            .with_context(|| format!("Could not write {}", json.display()))?;
        Ok(())
    })
    .await
}

/// The reading packed as AAC in an M4A, a tenth of the WAV's size.
async fn encode(ffmpeg: &Path, wav: &Path, out: &Path) -> Result<()> {
    let mut cmd = crate::process::command(ffmpeg);
    cmd.args(["-y", "-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(wav)
        .args(["-c:a", "aac", "-b:a", "96k", "-movflags", "+faststart"])
        .arg(out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let child = crate::process::spawn_managed(&mut cmd)
        .with_context(|| format!("Could not start FFmpeg ({})", ffmpeg.display()))?;
    let output = match tokio::time::timeout(ENCODE_TIMEOUT, child.wait_with_output()).await {
        Ok(r) => r?,
        Err(_) => bail!("FFmpeg took over 30 minutes and was stopped."),
    };
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr);
        bail!(
            "FFmpeg failed: {}",
            why.trim().lines().last().unwrap_or("").trim()
        );
    }
    Ok(())
}

/// What a card calls pasted text: its first words.
fn first_words(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().take(8).collect();
    let mut name = words.join(" ");
    if name.chars().count() > 48 {
        name = name
            .chars()
            .take(47)
            .collect::<String>()
            .trim_end()
            .to_string();
        name.push('…');
    } else if text.split_whitespace().nth(8).is_some() {
        name.push('…');
    }
    if name.is_empty() {
        "Pasted text".into()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_text_is_named_by_its_first_words() {
        assert_eq!(
            first_words("  Dear tenant, the rent  goes up."),
            "Dear tenant, the rent goes up."
        );
        assert_eq!(
            first_words("one two three four five six seven eight nine ten"),
            "one two three four five six seven eight…"
        );
        assert_eq!(first_words("   "), "Pasted text");
        assert!(first_words(&"long".repeat(40)).ends_with('…'));
    }
}

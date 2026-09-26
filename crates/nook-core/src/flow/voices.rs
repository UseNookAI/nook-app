//! Ports `flow/Voices.java`: the voices the flows speak with, from `runtime/voices.json`, models
//! the audio engine runs, each with the languages it speaks and whether it can clone a voice from
//! a few seconds of someone speaking. The list is in order of preference.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::languages;
use crate::runtime::model_catalog::{long, text};

/// A voice's model file, downloaded into `<home>\voices` on first use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceFile {
    pub name: String,
    pub url: String,
    pub sha256: Option<String>,
    /// 0 when the list does not say.
    pub bytes: u64,
}

/// One voice model.
///
/// - `family`: the audio engine's model family (`qwen3_tts`, `voxcpm2`, `supertonic`)
/// - `clones`: whether it speaks in a voice cloned from reference audio
/// - `designs`: whether it can make up a voice with no reference
/// - `sample_rate`: what its WAVs come out at
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Voice {
    pub id: String,
    pub family: String,
    pub name: String,
    pub clones: bool,
    pub designs: bool,
    pub languages: Vec<String>,
    pub licence: String,
    pub sample_rate: u32,
    pub file: VoiceFile,
}

impl Voice {
    pub fn speaks(&self, language: &str) -> bool {
        self.languages.iter().any(|l| l == language)
    }
}

/// A language as the pickers offer it, and whether any voice speaks it: only those are offered
/// to translate into, the rest only to translate from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Offered {
    pub code: &'static str,
    pub name: &'static str,
    pub spoken: bool,
}

/// What a language is spoken with: the voice, whether it clones the speaker, and a note for the
/// person when it is not what they asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub voice: Voice,
    pub cloned: bool,
    pub note: Option<String>,
}

/// The voices, in order of preference.
#[derive(Clone, Debug, Default)]
pub struct Voices {
    voices: Vec<Voice>,
}

impl Voices {
    pub fn new(voices: Vec<Voice>) -> Voices {
        Voices { voices }
    }

    /// The voices built into the app.
    pub fn bundled() -> Voices {
        Voices::parse(crate::resources::VOICES_JSON).unwrap_or_else(|e| {
            tracing::error!("Cannot read runtime/voices.json: {e:#}");
            Voices::default()
        })
    }

    /// Voices from a list in the format of `runtime/voices.json`, read as leniently as the
    /// original's Jackson `path(..)` calls.
    pub fn parse(json: &str) -> Result<Voices> {
        let root: Value = serde_json::from_str(json).context("voices.json is not JSON")?;
        let voices = root
            .get("voices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|v| {
                let f = v.get("file").cloned().unwrap_or(Value::Null);
                Voice {
                    id: text(v.get("id")).unwrap_or_default(),
                    family: text(v.get("family")).unwrap_or_default(),
                    name: text(v.get("name")).unwrap_or_default(),
                    clones: v.get("clones").and_then(Value::as_bool).unwrap_or(false),
                    designs: v.get("designs").and_then(Value::as_bool).unwrap_or(false),
                    languages: v
                        .get("languages")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|l| l.as_str().map(str::to_string))
                        .collect(),
                    licence: text(v.get("licence")).unwrap_or_default(),
                    sample_rate: long(v.get("sampleRate"))
                        .filter(|r| *r > 0)
                        .unwrap_or(24_000) as u32,
                    file: VoiceFile {
                        name: text(f.get("name")).unwrap_or_default(),
                        url: text(f.get("url")).unwrap_or_default(),
                        sha256: text(f.get("sha256")).filter(|s| !s.is_empty()),
                        bytes: long(f.get("bytes")).filter(|b| *b > 0).unwrap_or(0) as u64,
                    },
                }
            })
            .collect();
        Ok(Voices { voices })
    }

    /// Every language the pickers list, each with whether a voice speaks it.
    pub fn offered(&self) -> Vec<Offered> {
        languages::ALL
            .iter()
            .map(|l| Offered {
                code: l.code,
                name: l.name,
                spoken: self.voices.iter().any(|v| v.speaks(l.code)),
            })
            .collect()
    }

    pub fn all(&self) -> &[Voice] {
        &self.voices
    }

    pub fn by_id(&self, id: &str) -> Option<&Voice> {
        self.voices.iter().find(|v| v.id == id)
    }

    /// The voice for `language` (a code). With `keep_voice`, the first that clones the speaker;
    /// else a voice with its own presets, or one that makes a voice up. When only the other kind
    /// speaks the language it is used and the note says so; None when nothing speaks it.
    pub fn pick(&self, language: &str, keep_voice: bool) -> Option<Choice> {
        let name = languages::name_of(language);
        let find = |f: &dyn Fn(&Voice) -> bool| {
            self.voices
                .iter()
                .find(|v| f(v) && v.speaks(language))
                .cloned()
        };
        let cloning = find(&|v| v.clones);
        let preset = find(&|v| !v.clones);
        let designing = find(&|v| v.designs);
        let choice = |voice: Voice, cloned: bool, note: Option<String>| Choice {
            voice,
            cloned,
            note,
        };
        if keep_voice {
            if let Some(v) = cloning {
                return Some(choice(v, true, None));
            }
            return preset.map(|v| {
                choice(
                    v,
                    false,
                    Some(format!(
                        "No voice can clone a speaker in {name} yet, so it is spoken in a standard voice."
                    )),
                )
            });
        }
        if let Some(v) = preset {
            return Some(choice(v, false, None));
        }
        if let Some(v) = designing {
            return Some(choice(v, false, None));
        }
        cloning.map(|v| {
            choice(
                v,
                true,
                Some(format!(
                    "The only voice for {name} clones the speaker, so it is spoken in their voice."
                )),
            )
        })
    }

    /// The standard voice to fall back on when the cloning voice of `choice` cannot speak (a new
    /// Nook: the engine refused the model), or None when there is no other.
    pub fn fallback(&self, language: &str, choice: &Choice) -> Option<Choice> {
        if !choice.cloned {
            return None;
        }
        self.voices
            .iter()
            .find(|v| !v.clones && v.speaks(language) && v.id != choice.voice.id)
            .map(|v| Choice {
                voice: v.clone(),
                cloned: false,
                note: None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(id: &str, clones: bool, designs: bool, langs: &[&str]) -> Voice {
        Voice {
            id: id.into(),
            family: id.into(),
            name: id.to_uppercase(),
            clones,
            designs,
            languages: langs.iter().map(|s| s.to_string()).collect(),
            licence: String::new(),
            sample_rate: 24_000,
            file: VoiceFile {
                name: format!("{id}.gguf"),
                url: String::new(),
                sha256: None,
                bytes: 1,
            },
        }
    }

    #[test]
    fn the_ladder_prefers_what_was_asked_and_says_when_it_cannot() {
        let voices = Voices::new(vec![
            voice("qwen", true, false, &["en", "de"]),
            voice("vox", true, true, &["en", "de", "ar"]),
            voice("tonic", false, false, &["en", "de", "uk"]),
        ]);
        let c = voices.pick("de", true).unwrap();
        assert_eq!(
            (c.voice.id.as_str(), c.cloned, c.note),
            ("qwen", true, None)
        );
        let c = voices.pick("de", false).unwrap();
        assert_eq!((c.voice.id.as_str(), c.cloned), ("tonic", false));
        let c = voices.pick("uk", true).unwrap();
        assert_eq!((c.voice.id.as_str(), c.cloned), ("tonic", false));
        assert_eq!(
            c.note.as_deref(),
            Some("No voice can clone a speaker in Ukrainian yet, so it is spoken in a standard voice.")
        );
        let c = voices.pick("ar", false).unwrap();
        assert_eq!(
            (c.voice.id.as_str(), c.cloned, c.note),
            ("vox", false, None),
            "a designing voice speaks with a voice of its own"
        );
        assert!(voices.pick("bn", true).is_none());
        assert!(voices.pick("bn", false).is_none());

        let clone = voices.pick("de", true).unwrap();
        assert_eq!(voices.fallback("de", &clone).unwrap().voice.id, "tonic");
        assert!(voices
            .fallback("ar", &voices.pick("ar", true).unwrap())
            .is_none());
        assert!(voices
            .fallback("de", &voices.pick("de", false).unwrap())
            .is_none());
    }

    #[test]
    fn the_bundled_voices_are_complete() {
        let voices = Voices::bundled();
        assert_eq!(voices.all().len(), 3);
        for v in voices.all() {
            assert!(!v.family.is_empty() && !v.languages.is_empty(), "{}", v.id);
            assert!(
                v.file.url.starts_with("https://huggingface.co/"),
                "{}",
                v.id
            );
            assert_eq!(v.file.sha256.as_deref().map(str::len), Some(64), "{}", v.id);
            assert!(v.file.bytes > 0, "{}", v.id);
            for l in &v.languages {
                assert!(
                    languages::by_code(l).is_some() || ["km", "lo", "my"].contains(&l.as_str()),
                    "{} speaks {l}",
                    v.id
                );
            }
        }
        // Every language a picker offers is spoken by something, or the plan says it is not.
        let spoken = languages::ALL
            .iter()
            .filter(|l| voices.pick(l.code, true).is_some())
            .count();
        assert!(spoken >= 38, "{spoken}");
        assert!(voices.pick("en", true).unwrap().cloned);

        // Malayalam is heard but no voice says it: offered to translate from only.
        let offered = voices.offered();
        let of = |code: &str| offered.iter().find(|o| o.code == code).unwrap().spoken;
        assert!(of("en") && of("tr") && of("sw"));
        assert!(!of("ml") && !of("ca"));
        assert_eq!(offered.len(), languages::ALL.len());
    }
}

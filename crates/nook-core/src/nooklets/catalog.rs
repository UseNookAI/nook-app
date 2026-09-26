//! The Nooklets the finder chooses between: each with what it does, requests people make of it
//! (in several languages, as the model reads them all), words that point to it, and what a
//! request can set for it before it opens (the language to translate into, the format to
//! convert to).

use serde::Serialize;

use crate::convert::formats;
use crate::flow::languages;

pub struct Nooklet {
    /// The id the page opens it by: "translate", "pdf", "convert".
    pub id: &'static str,
    pub title: &'static str,
    pub blurb: &'static str,
    pub examples: &'static [&'static str],
    /// Words that point to it (lower case, whole words).
    pub hints: &'static [&'static str],
}

pub const NOOKLETS: &[Nooklet] = &[
    Nooklet {
        id: "translate",
        title: "Translate speech",
        blurb: "Speak or drop a recording or a video; hear it in another language.",
        examples: &[
            "translate speech into another language",
            "translate what I say into Spanish",
            "translate this audio into German",
            "translate a video into English",
            "dub a video in another language",
            "hear this recording in French",
            "speak my words in Japanese",
            "translate a podcast",
            "voice translation in my own voice",
            "interpret my speech",
            "translate a voice message",
            "make subtitles in another language for a video",
            "turn my recording into Italian",
            "translate an interview recording",
            "sesimi İngilizceye çevir",
            "bu videoyu Türkçeye çevir",
            "übersetze meine Rede ins Englische",
            "traduce este audio al inglés",
            "traduis cette vidéo en français",
        ],
        hints: &[
            "translate",
            "translation",
            "translator",
            "dub",
            "dubbing",
            "interpret",
            "subtitle",
            "subtitles",
            "speech",
            "spoken",
            "voice",
            "podcast",
            "çevir",
            "tercüme",
            "übersetze",
            "übersetzen",
            "traduce",
            "traducir",
            "traduis",
            "traduire",
            "traduci",
        ],
    },
    Nooklet {
        id: "pdf",
        title: "Edit a PDF",
        blurb: "Change any text in a PDF, scans too; the font and size stay the same.",
        examples: &[
            "edit a PDF",
            "change the text in a PDF",
            "fix a typo in a PDF",
            "replace a name in a PDF",
            "change a date in a PDF",
            "update the price on an invoice PDF",
            "edit the text of a scanned document",
            "change a word on a scanned form",
            "correct a mistake in my PDF contract",
            "edit a PDF form without Acrobat",
            "change the address on a PDF letter",
            "remove a line of text from a PDF",
            "PDF'deki metni değiştir",
            "PDF'deki ismi düzelt",
            "PDF bearbeiten",
            "Text in einer PDF ändern",
            "modifier le texte d'un PDF",
            "editar un PDF",
        ],
        hints: &[
            "edit",
            "editing",
            "typo",
            "fix",
            "correct",
            "replace",
            "change",
            "düzenle",
            "değiştir",
            "düzelt",
            "bearbeiten",
            "ändern",
            "modifier",
            "editar",
            "scan",
            "scanned",
            "form",
            "invoice",
            "contract",
        ],
    },
    Nooklet {
        id: "convert",
        title: "Convert documents",
        blurb:
            "Word, Excel, PowerPoint, PDF, pictures, e-books and more, into the format you want.",
        examples: &[
            "convert a document to another format",
            "convert Word to PDF",
            "save a document as PDF",
            "convert PDF to Word",
            "turn a docx into a pdf",
            "convert Excel to CSV",
            "export a spreadsheet to PDF",
            "convert a PowerPoint to PDF",
            "make a PDF from pictures",
            "convert images to JPG",
            "PNG to JPG",
            "convert Markdown to Word",
            "turn a PDF into images",
            "extract the text from a PDF",
            "convert an e-book to PDF",
            "change a file's format",
            "convert CSV to Excel",
            "open an old .doc file as docx",
            "bu belgeyi pdf yap",
            "Word dosyasını PDF'ye dönüştür",
            "in PDF umwandeln",
            "convertir un document en PDF",
            "convertir este archivo a Word",
        ],
        hints: &[
            "convert",
            "conversion",
            "converter",
            "export",
            "format",
            "dönüştür",
            "çevir",
            "yap",
            "umwandeln",
            "konvertieren",
            "convertir",
            "converti",
        ],
    },
];

pub fn by_id(id: &str) -> Option<&'static Nooklet> {
    NOOKLETS.iter().find(|n| n.id == id)
}

/// The words of a request, lower case, apostrophes and punctuation dropped ("pdf'yi" is "pdf" and
/// "yi").
pub fn words(text: &str) -> Vec<String> {
    // A capital dotted I (Turkish) lowers to "i" and a combining dot: the dot goes.
    text.to_lowercase()
        .replace('\u{307}', "")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// What a request sets for a Nooklet before it opens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    /// "language" (the translator's target) or "format" (the converter's).
    pub key: String,
    pub value: String,
    pub label: String,
}

/// The target a request names: after "to", "into", "as" or "in" when it has one, else the last
/// one it mentions (Turkish and others put the target last).
fn named_target<T>(ws: &[String], find: impl Fn(&str) -> Option<T>) -> Option<T> {
    let mut after_to = None;
    let mut last = None;
    for (i, w) in ws.iter().enumerate() {
        let Some(t) = find(w) else { continue };
        if i > 0
            && [
                "to", "into", "as", "in", "en", "a", "al", "ins", "zu", "nach",
            ]
            .contains(&ws[i - 1].as_str())
        {
            after_to = Some(i);
        }
        last = Some((i, t));
    }
    let (i, t) = last?;
    match after_to {
        Some(j) if j != i => find(&ws[j]),
        _ => Some(t),
    }
}

/// What `request` sets for the Nooklet `id`.
pub fn preset(id: &str, request: &str) -> Option<Preset> {
    let ws = words(request);
    match id {
        "translate" => {
            let lang = named_target(&ws, |w| {
                languages::ALL
                    .iter()
                    .find(|l| l.name.eq_ignore_ascii_case(w) || native(w) == Some(l.code))
                    .copied()
            })?;
            Some(Preset {
                key: "language".into(),
                value: lang.code.into(),
                label: format!("into {}", lang.name),
            })
        }
        "convert" => {
            let id = named_target(&ws, formats::named)?;
            let f = formats::by_id(id)?;
            Some(Preset {
                key: "format".into(),
                value: f.id.into(),
                label: format!("to {}", f.name),
            })
        }
        _ => None,
    }
}

/// Languages named in their own tongue or in Turkish ("türkçe", "deutsch", "ingilizce").
fn native(w: &str) -> Option<&'static str> {
    let w = w.to_lowercase();
    let stem = |s: &str| w.starts_with(s);
    Some(match () {
        _ if stem("türkçe") || stem("turkce") => "tr",
        _ if stem("ingilizce")
            || stem("englisch")
            || w == "anglais"
            || w == "inglés"
            || w == "ingles" =>
        {
            "en"
        }
        _ if stem("almanca") || stem("deutsch") || w == "allemand" || w == "alemán" => "de",
        _ if stem("fransızca") || stem("französisch") || w == "français" || w == "francés" => {
            "fr"
        }
        _ if stem("ispanyolca") || stem("spanisch") || w == "español" || w == "espagnol" => "es",
        _ if stem("italyanca") || stem("italienisch") || w == "italiano" || w == "italien" => "it",
        _ if stem("türkisch") || w == "turc" || w == "turco" => "tr",
        _ if stem("rusça") || w == "русский" => "ru",
        _ if stem("arapça") || w == "العربية" => "ar",
        _ if stem("japonca") || w == "日本語" => "ja",
        _ if w == "中文" || stem("çince") => "zh",
        _ => return None,
    })
}

/// How strongly the words of a request point to each Nooklet by themselves, 0 to 1: a hint
/// counts, a format or language named counts, and so do words shared with its examples.
pub fn by_words(request: &str) -> Vec<f32> {
    let ws = words(request);
    if ws.is_empty() {
        return vec![0.0; NOOKLETS.len()];
    }
    NOOKLETS
        .iter()
        .map(|n| {
            let mut score = 0.0f32;
            for w in &ws {
                if n.hints.contains(&w.as_str()) {
                    score += 1.0;
                }
            }
            match n.id {
                "convert" if ws.iter().filter(|w| formats::named(w).is_some()).count() >= 1 => {
                    score += 0.8
                }
                "translate" if preset("translate", request).is_some() => score += 1.0,
                "pdf" if ws.iter().any(|w| w == "pdf") => score += 0.5,
                _ => {}
            }
            let shared = ws
                .iter()
                .filter(|w| w.len() > 3 && n.examples.iter().any(|e| words(e).contains(w)))
                .count();
            score += shared as f32 * 0.3;
            (score / 2.5).min(1.0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_sets_the_language_or_the_format() {
        assert_eq!(
            preset("translate", "translate what I say into German")
                .unwrap()
                .value,
            "de"
        );
        assert_eq!(
            preset("translate", "sesimi İngilizceye çevir")
                .unwrap()
                .value,
            "en"
        );
        assert_eq!(
            preset("translate", "übersetze meine Rede ins Englische")
                .unwrap()
                .value,
            "en"
        );
        assert_eq!(preset("translate", "dub this").map(|p| p.value), None);
        let p = preset("convert", "convert this PDF to Word").unwrap();
        assert_eq!(
            (p.value.as_str(), p.label.as_str()),
            ("docx", "to Word document")
        );
        assert_eq!(
            preset("convert", "pdf'yi word'e çevir").unwrap().value,
            "docx"
        );
        assert_eq!(
            preset("convert", "bu belgeyi pdf yap").unwrap().value,
            "pdf"
        );
        assert_eq!(
            preset("convert", "turn my excel into csv").unwrap().value,
            "csv"
        );
        assert_eq!(preset("pdf", "fix a typo"), None);
    }

    #[test]
    fn words_alone_point_the_right_way() {
        let best = |q: &str| {
            let s = by_words(q);
            let i = (0..s.len()).max_by(|a, b| s[*a].total_cmp(&s[*b])).unwrap();
            (NOOKLETS[i].id, s[i])
        };
        assert_eq!(best("translate my speech into French").0, "translate");
        assert_eq!(best("convert excel to csv").0, "convert");
        assert_eq!(best("fix a typo in my pdf").0, "pdf");
        assert!(best("book a flight").1 < 0.3, "no match");
    }
}

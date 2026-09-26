//! Ports `flow/Languages.java`: the languages the flows offer, by their ISO 639-1 code and English
//! name. Whisper takes the code and reports the name in lower case ("english"); the translation
//! model is told the name.

use serde::Serialize;

/// One language: its ISO 639-1 code and its English name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Language {
    pub code: &'static str,
    pub name: &'static str,
}

const fn lang(code: &'static str, name: &'static str) -> Language {
    Language { code, name }
}

/// Every language the pickers list, in the original's order.
pub const ALL: &[Language] = &[
    lang("en", "English"),
    lang("es", "Spanish"),
    lang("fr", "French"),
    lang("de", "German"),
    lang("it", "Italian"),
    lang("pt", "Portuguese"),
    lang("nl", "Dutch"),
    lang("sv", "Swedish"),
    lang("da", "Danish"),
    lang("no", "Norwegian"),
    lang("fi", "Finnish"),
    lang("is", "Icelandic"),
    lang("pl", "Polish"),
    lang("cs", "Czech"),
    lang("sk", "Slovak"),
    lang("hu", "Hungarian"),
    lang("ro", "Romanian"),
    lang("bg", "Bulgarian"),
    lang("hr", "Croatian"),
    lang("sr", "Serbian"),
    lang("sl", "Slovenian"),
    lang("uk", "Ukrainian"),
    lang("ru", "Russian"),
    lang("el", "Greek"),
    lang("tr", "Turkish"),
    lang("ar", "Arabic"),
    lang("he", "Hebrew"),
    lang("fa", "Persian"),
    lang("ur", "Urdu"),
    lang("hi", "Hindi"),
    lang("bn", "Bengali"),
    lang("ta", "Tamil"),
    lang("te", "Telugu"),
    lang("ml", "Malayalam"),
    lang("id", "Indonesian"),
    lang("ms", "Malay"),
    lang("vi", "Vietnamese"),
    lang("th", "Thai"),
    lang("zh", "Chinese"),
    lang("ja", "Japanese"),
    lang("ko", "Korean"),
    lang("ca", "Catalan"),
    lang("et", "Estonian"),
    lang("lv", "Latvian"),
    lang("lt", "Lithuanian"),
    lang("sw", "Swahili"),
    lang("tl", "Tagalog"),
];

/// The language with this code, whatever its case.
pub fn by_code(code: &str) -> Option<Language> {
    let c = code.trim().to_lowercase();
    ALL.iter().copied().find(|l| l.code == c)
}

/// The language with this English name, whatever its case ("english", as Whisper says it).
pub fn by_name(name: &str) -> Option<Language> {
    let n = name.trim().to_lowercase();
    ALL.iter().copied().find(|l| l.name.to_lowercase() == n)
}

/// The language named by a code or a name.
pub fn find(code_or_name: &str) -> Option<Language> {
    by_code(code_or_name).or_else(|| by_name(code_or_name))
}

/// The code for a code or a name, or the name itself in lower case when Nook does not list it;
/// None for nothing or blank.
pub fn code_of(code_or_name: Option<&str>) -> Option<String> {
    let s = code_or_name?.trim();
    if s.is_empty() {
        return None;
    }
    Some(
        find(s)
            .map(|l| l.code.to_string())
            .unwrap_or_else(|| s.to_lowercase()),
    )
}

/// The name to show for a code or a name; an unknown one is shown with a capital.
pub fn name_of(code_or_name: &str) -> String {
    let s = code_or_name.trim();
    if s.is_empty() {
        return String::new();
    }
    if let Some(l) = find(s) {
        return l.name.to_string();
    }
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_names_find_each_other() {
        assert_eq!(by_code("DE").map(|l| l.name), Some("German"));
        assert_eq!(by_name("english").map(|l| l.code), Some("en"));
        assert_eq!(code_of(Some("english")).as_deref(), Some("en"));
        assert_eq!(code_of(Some(" fr ")).as_deref(), Some("fr"));
        assert_eq!(
            code_of(Some("Welsh")).as_deref(),
            Some("welsh"),
            "an unlisted name stays itself"
        );
        assert_eq!(code_of(Some("  ")), None);
        assert_eq!(code_of(None), None);
        assert_eq!(name_of("ja"), "Japanese");
        assert_eq!(name_of("welsh"), "Welsh");
        assert_eq!(name_of(""), "");
    }

    #[test]
    fn the_table_has_no_duplicates() {
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                assert_ne!(a.code, b.code);
                assert_ne!(a.name, b.name);
            }
        }
        assert_eq!(ALL.len(), 47);
    }
}

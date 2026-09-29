//! The installed font a PDF's font is, for writing characters its embedded copy does not have.
//!
//! PDFs embed only the glyphs they use ("ABCDEF+Calibri-Bold"), so a new character often has no
//! glyph there. Windows lists its fonts in the registry by name ("Calibri Bold (TrueType)" →
//! `calibrib.ttf`); a PDF names them by PostScript name ("Calibri-Bold", "ArialMT",
//! "TimesNewRomanPS-BoldItalicMT"). Both are reduced to one key ("calibribold") and matched.

use std::collections::HashMap;
use std::path::PathBuf;

/// What is known of a PDF font.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontTraits {
    /// The font's name in the PDF, "AAAAAA+Georgia-Bold".
    pub name: String,
    /// Its family as PDFium reads it, "Georgia" (may be empty).
    pub family: String,
    pub bold: bool,
    pub italic: bool,
    pub serif: bool,
    pub mono: bool,
}

/// The name without a subset's six-letter prefix ("AAAAAA+").
pub fn base_name(name: &str) -> &str {
    match name.split_once('+') {
        Some((tag, rest)) if tag.len() == 6 && tag.chars().all(|c| c.is_ascii_uppercase()) => rest,
        _ => name,
    }
}

/// Whether the PDF embeds only part of the font (a subset), by its name.
pub fn is_subset(name: &str) -> bool {
    base_name(name).len() != name.len()
}

/// Lower-case letters and digits only: "Times New Roman Bold" and "TimesNewRoman-Bold" meet.
pub fn key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A PostScript family without its foundry suffixes: "ArialMT" → "Arial", "TimesNewRomanPS" →
/// "TimesNewRoman".
fn plain_family(f: &str) -> &str {
    let mut f = f.trim();
    for suffix in ["PSMT", "MT", "PS", ",", "-"] {
        if let Some(rest) = f.strip_suffix(suffix) {
            f = rest;
        }
    }
    f
}

/// A style as Windows names it: "BoldItalicMT" → "Bold Italic", "Oblique" → "Italic",
/// "Regular" → "".
fn plain_style(s: &str) -> String {
    let s = plain_family(s)
        .replace("Oblique", "Italic")
        .replace("Roman", "");
    let s = s.trim();
    if s.eq_ignore_ascii_case("regular")
        || s.eq_ignore_ascii_case("normal")
        || s.eq_ignore_ascii_case("book")
    {
        return String::new();
    }
    s.to_string()
}

fn flag_style(bold: bool, italic: bool) -> &'static str {
    match (bold, italic) {
        (true, true) => "Bold Italic",
        (true, false) => "Bold",
        (false, true) => "Italic",
        (false, false) => "",
    }
}

/// The keys to look the font up by, the likeliest first.
pub fn wanted(t: &FontTraits) -> Vec<String> {
    let base = base_name(&t.name);
    let (fam, style) = match base.split_once(['-', ',']) {
        Some((f, s)) => (plain_family(f), plain_style(s)),
        None => (plain_family(base), String::new()),
    };
    let style = if style.is_empty() {
        flag_style(t.bold, t.italic).to_string()
    } else {
        style
    };
    let mut out = Vec::new();
    for family in [fam, t.family.as_str()] {
        if family.trim().is_empty() {
            continue;
        }
        out.push(key(&format!("{family}{style}")));
        // "Bold Italic" is also listed as "BoldItalic", and "Semibold" as "SemiBold": the key
        // is the same either way.
    }
    for family in [fam, t.family.as_str()] {
        if !family.trim().is_empty() {
            out.push(key(&format!("{family}{}", flag_style(t.bold, t.italic))));
        }
    }
    // The family's plain face, when its bold or italic is not installed.
    for family in [fam, t.family.as_str()] {
        if !family.trim().is_empty() {
            out.push(key(family));
        }
    }
    // A face of the same kind that every Windows has.
    let style = flag_style(t.bold, t.italic);
    let kind: &[&str] = if t.mono {
        &["Consolas", "Courier New"]
    } else if t.serif {
        &["Times New Roman", "Georgia"]
    } else {
        &["Arial", "Segoe UI"]
    };
    for family in kind {
        out.push(key(&format!("{family}{style}")));
        out.push(key(family));
    }
    // Wide coverage for scripts the others lack.
    out.push(key("Segoe UI"));
    out.push(key("Arial Unicode MS"));
    let mut seen = std::collections::HashSet::new();
    out.retain(|k| !k.is_empty() && seen.insert(k.clone()));
    out
}

/// Installed fonts by key, with the name Windows shows for each file.
#[derive(Clone, Debug, Default)]
pub struct SystemFonts {
    by_key: HashMap<String, PathBuf>,
    names: HashMap<PathBuf, String>,
}

impl SystemFonts {
    /// From registry entries: display name ("Calibri Bold (TrueType)", "Cambria & Cambria Math
    /// (TrueType)") and file (a name in `fonts_dir`, or a whole path for a font installed for one
    /// user).
    pub fn from_entries(
        entries: impl IntoIterator<Item = (String, String)>,
        fonts_dir: &std::path::Path,
    ) -> SystemFonts {
        let mut by_key: HashMap<String, PathBuf> = HashMap::new();
        let mut names: HashMap<PathBuf, String> = HashMap::new();
        for (name, file) in entries {
            let lower = file.to_lowercase();
            // Only TrueType and OpenType-with-TrueType outlines load as TrueType.
            if !(lower.ends_with(".ttf") || lower.ends_with(".ttc") || lower.ends_with(".otf")) {
                continue;
            }
            let path = if std::path::Path::new(&file).is_absolute() {
                PathBuf::from(&file)
            } else {
                fonts_dir.join(&file)
            };
            let name = name
                .rsplit_once('(')
                .map(|(n, _)| n)
                .unwrap_or(&name)
                .trim()
                .to_string();
            names.entry(path.clone()).or_insert_with(|| name.clone());
            for part in name.split('&') {
                // A single font file wins over a collection of the same name.
                let k = key(part);
                let replace = match by_key.get(&k) {
                    None => true,
                    Some(p) => is_collection(p) && !lower.ends_with(".ttc"),
                };
                if replace {
                    by_key.insert(k, path.clone());
                }
            }
        }
        SystemFonts { by_key, names }
    }

    /// The fonts Windows has installed, for everyone and for this user. On a Mac: the fonts in
    /// the system's, the computer's and the person's font folders, and those Microsoft Office
    /// keeps inside its apps (Calibri, Cambria, Consolas...), each by its full name.
    pub fn installed() -> SystemFonts {
        if cfg!(target_os = "macos") {
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let mut dirs = vec![
                PathBuf::from("/System/Library/Fonts"),
                PathBuf::from("/System/Library/Fonts/Supplemental"),
                PathBuf::from("/Library/Fonts"),
            ];
            dirs.extend(home.iter().map(|h| h.join("Library").join("Fonts")));
            for app in ["Word", "Excel", "PowerPoint", "Outlook"] {
                dirs.push(
                    PathBuf::from(format!("/Applications/Microsoft {app}.app"))
                        .join("Contents/Resources/DFonts"),
                );
            }
            let entries = dirs.iter().flat_map(|d| named_files(d)).collect::<Vec<_>>();
            return SystemFonts::from_entries(entries, std::path::Path::new("/"));
        }
        let windir = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let fonts_dir = windir.join("Fonts");
        let mut entries = registry::fonts(true);
        entries.extend(registry::fonts(false));
        SystemFonts::from_entries(entries, &fonts_dir)
    }

    /// The installed font for these traits, closest first: the same face, the family's plain
    /// face, then a face of the same kind. Only files that exist, each with its name.
    pub fn candidates(&self, t: &FontTraits) -> Vec<(PathBuf, String)> {
        let mut out: Vec<(PathBuf, String)> = Vec::new();
        for k in wanted(t) {
            if let Some(p) = self.by_key.get(&k) {
                if p.is_file() && !out.iter().any(|(q, _)| q == p) {
                    let name = self.names.get(p).cloned().unwrap_or_else(|| k.clone());
                    out.push((p.clone(), name));
                }
            }
        }
        out
    }

    /// The installed file for a key, with its Windows name, when it exists.
    pub fn file(&self, key: &str) -> Option<(PathBuf, String)> {
        let p = self.by_key.get(key).filter(|p| p.is_file())?;
        let name = self
            .names
            .get(p)
            .cloned()
            .unwrap_or_else(|| key.to_string());
        Some((p.clone(), name))
    }

    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }
}

fn is_collection(p: &std::path::Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ttc"))
}

/// `(full name, whole path)` of each TrueType or OpenType font directly in `dir` (a collection by
/// its first face, the one a font file is loaded as).
pub fn named_files(dir: &std::path::Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| matches!(x.to_lowercase().as_str(), "ttf" | "ttc" | "otf"))
        })
        .filter_map(|p| Some((full_name(&p)?, p.to_string_lossy().into_owned())))
        .collect()
}

/// A font file's full name ("Arial Bold") from its `name` table, reading only the table
/// directory and that table: the English Windows name, else the Macintosh one, else Unicode's.
pub fn full_name(path: &std::path::Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let mut read_at = |at: u64, len: usize| -> Option<Vec<u8>> {
        let mut buf = vec![0u8; len];
        f.seek(SeekFrom::Start(at)).ok()?;
        f.read_exact(&mut buf).ok()?;
        Some(buf)
    };
    let u16_at = |b: &[u8], i: usize| -> Option<u16> {
        Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]))
    };
    let u32_at = |b: &[u8], i: usize| -> Option<u32> {
        Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?))
    };
    // A collection ("ttcf") names its first face's table directory at byte 12.
    let first = read_at(0, 16)?;
    let (base, head) = if &first[0..4] == b"ttcf" {
        let base = u64::from(u32_at(&first, 12)?);
        (base, read_at(base, 12)?)
    } else {
        (0, first)
    };
    let tables = usize::from(u16_at(&head, 4)?);
    if tables == 0 || tables > 200 {
        return None;
    }
    let dir = read_at(base + 12, tables * 16)?;
    let record = (0..tables).find(|i| &dir[i * 16..i * 16 + 4] == b"name")?;
    let offset = u64::from(u32_at(&dir, record * 16 + 8)?);
    let length = u32_at(&dir, record * 16 + 12)? as usize;
    if length > 1 << 20 {
        return None;
    }
    let name = read_at(offset, length)?;
    let count = usize::from(u16_at(&name, 2)?);
    let strings = usize::from(u16_at(&name, 4)?);
    // (rank, text): Windows English, Windows any, Macintosh English, Unicode.
    let mut best: Option<(u8, String)> = None;
    for i in 0..count {
        let r = 6 + i * 12;
        let (Some(platform), Some(encoding), Some(language), Some(id), Some(len), Some(at)) = (
            u16_at(&name, r),
            u16_at(&name, r + 2),
            u16_at(&name, r + 4),
            u16_at(&name, r + 6),
            u16_at(&name, r + 8),
            u16_at(&name, r + 10),
        ) else {
            break;
        };
        if id != 4 {
            continue;
        }
        let Some(bytes) =
            name.get(strings + usize::from(at)..strings + usize::from(at) + usize::from(len))
        else {
            continue;
        };
        let utf16 = || {
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes(*c))
                .collect();
            String::from_utf16_lossy(&units)
        };
        let (rank, text) = match (platform, encoding, language) {
            (3, 1 | 10, 0x409) => (0, utf16()),
            (3, 1 | 10, _) => (1, utf16()),
            (1, 0, 0) => (2, bytes.iter().map(|&b| b as char).collect()),
            (0, _, _) => (3, utf16()),
            _ => continue,
        };
        let text = text.trim().to_string();
        if !text.is_empty() && best.as_ref().is_none_or(|(r, _)| rank < *r) {
            best = Some((rank, text));
        }
    }
    best.map(|(_, t)| t)
}

#[cfg(windows)]
mod registry {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
        KEY_READ, REG_SZ,
    };

    /// `(display name, file)` of every font under `...\CurrentVersion\Fonts`.
    pub fn fonts(machine: bool) -> Vec<(String, String)> {
        let path: Vec<u16> = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let root = if machine {
            HKEY_LOCAL_MACHINE
        } else {
            HKEY_CURRENT_USER
        };
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: a valid root, a NUL-terminated path, and an out pointer for the key.
        if unsafe { RegOpenKeyExW(root, path.as_ptr(), 0, KEY_READ, &mut key) } != ERROR_SUCCESS {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut index = 0;
        loop {
            let mut name = [0u16; 512];
            let mut name_len = name.len() as u32;
            let mut data = [0u16; 1024];
            let mut data_len = (data.len() * 2) as u32;
            let mut kind = 0u32;
            // SAFETY: buffers and their lengths as the call wants them; the key is open.
            let r = unsafe {
                RegEnumValueW(
                    key,
                    index,
                    name.as_mut_ptr(),
                    &mut name_len,
                    std::ptr::null(),
                    &mut kind,
                    data.as_mut_ptr() as *mut u8,
                    &mut data_len,
                )
            };
            if r != ERROR_SUCCESS {
                break;
            }
            index += 1;
            if kind != REG_SZ {
                continue;
            }
            let name = String::from_utf16_lossy(&name[..name_len as usize]);
            let chars = (data_len as usize / 2).min(data.len());
            let file = String::from_utf16_lossy(&data[..chars])
                .trim_end_matches('\0')
                .to_string();
            out.push((name, file));
        }
        // SAFETY: the key was opened above.
        unsafe { RegCloseKey(key) };
        out
    }
}

#[cfg(not(windows))]
mod registry {
    pub fn fonts(_machine: bool) -> Vec<(String, String)> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn traits(name: &str, family: &str, bold: bool, italic: bool) -> FontTraits {
        FontTraits {
            name: name.into(),
            family: family.into(),
            bold,
            italic,
            ..FontTraits::default()
        }
    }

    #[test]
    fn postscript_names_meet_windows_names() {
        assert_eq!(base_name("AAAAAA+Georgia-Bold"), "Georgia-Bold");
        assert_eq!(base_name("Georgia"), "Georgia");
        assert!(is_subset("BAAAAA+Calibri") && !is_subset("Calibri"));
        assert_eq!(
            wanted(&traits("AAAAAA+Georgia-Bold", "Georgia", true, false))[0],
            "georgiabold"
        );
        assert_eq!(
            wanted(&traits("ArialMT", "Arial", false, false))[0],
            "arial"
        );
        assert_eq!(
            wanted(&traits(
                "TimesNewRomanPS-BoldItalicMT",
                "Times New Roman",
                true,
                true
            ))[0],
            "timesnewromanbolditalic"
        );
        assert_eq!(
            wanted(&traits("Calibri,Italic", "", false, true))[0],
            "calibriitalic"
        );
        assert_eq!(
            wanted(&traits("Helvetica-Oblique", "", false, true))[0],
            "helveticaitalic"
        );
        assert_eq!(
            wanted(&traits("DAAAAA+SegoeUI-Semibold", "Segoe UI", false, false))[0],
            "segoeuisemibold"
        );
        // a sans face every Windows has comes after the family
        let keys = wanted(&traits("Helvetica-Bold", "Helvetica", true, false));
        assert!(
            keys.iter().position(|k| k == "arialbold")
                > keys.iter().position(|k| k == "helveticabold")
        );
    }

    #[test]
    fn registry_entries_become_keys_and_files() {
        let dir = tempfile::tempdir().unwrap();
        for f in [
            "georgiab.ttf",
            "georgia.ttf",
            "cambria.ttc",
            "arial.ttf",
            "consola.ttf",
        ] {
            std::fs::write(dir.path().join(f), b"x").unwrap();
        }
        let user = dir.path().join("user-font.ttf");
        std::fs::write(&user, b"x").unwrap();
        let fonts = SystemFonts::from_entries(
            [
                (
                    "Georgia Bold (TrueType)".to_string(),
                    "georgiab.ttf".to_string(),
                ),
                ("Georgia (TrueType)".into(), "georgia.ttf".into()),
                (
                    "Cambria & Cambria Math (TrueType)".into(),
                    "cambria.ttc".into(),
                ),
                ("Arial (TrueType)".into(), "arial.ttf".into()),
                ("Consolas (TrueType)".into(), "consola.ttf".into()),
                ("Marlett (TrueType)".into(), "marlett.fon".into()),
                ("My Font (TrueType)".into(), user.display().to_string()),
            ],
            dir.path(),
        );
        assert_eq!(fonts.len(), 7);
        let bold = fonts.candidates(&traits("AAAAAA+Georgia-Bold", "Georgia", true, false));
        assert_eq!(
            bold[0],
            (dir.path().join("georgiab.ttf"), "Georgia Bold".to_string())
        );
        assert!(bold
            .iter()
            .any(|(p, _)| p == &dir.path().join("georgia.ttf")));
        let cambria = fonts.candidates(&traits("Cambria", "Cambria", false, false));
        assert_eq!(cambria[0].0, dir.path().join("cambria.ttc"));
        let mono = fonts.candidates(&FontTraits {
            name: "CAAAAA+Menlo-Regular".into(),
            mono: true,
            ..FontTraits::default()
        });
        assert_eq!(
            mono[0].0,
            dir.path().join("consola.ttf"),
            "a mono face of the same kind"
        );
        assert_eq!(
            fonts.candidates(&traits("MyFont", "My Font", false, false))[0].0,
            user
        );
    }

    #[cfg(windows)]
    #[test]
    fn this_windows_lists_its_fonts() {
        let fonts = SystemFonts::installed();
        assert!(fonts.len() > 20, "{} fonts", fonts.len());
        assert!(!fonts
            .candidates(&traits("ArialMT", "Arial", false, false))
            .is_empty());
    }

    /// The name table is read the same on any system; Windows' own fonts are there to read.
    #[cfg(windows)]
    #[test]
    fn a_font_files_full_name_is_read_from_its_name_table() {
        let fonts = std::path::PathBuf::from(
            std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into()),
        )
        .join("Fonts");
        assert_eq!(
            full_name(&fonts.join("arial.ttf")).as_deref(),
            Some("Arial")
        );
        assert_eq!(
            full_name(&fonts.join("timesbd.ttf")).as_deref(),
            Some("Times New Roman Bold")
        );
        // a collection by its first face
        assert_eq!(
            full_name(&fonts.join("cambria.ttc")).as_deref(),
            Some("Cambria")
        );
        assert_eq!(full_name(&fonts.join("no-such-font.ttf")), None);
        let named = named_files(&fonts);
        assert!(named
            .iter()
            .any(|(n, f)| n == "Arial Bold" && f.ends_with("arialbd.ttf")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn this_mac_lists_its_fonts() {
        let fonts = SystemFonts::installed();
        assert!(fonts.len() > 20, "{} fonts", fonts.len());
        // Arial, Times New Roman and Courier New come with macOS
        assert!(!fonts
            .candidates(&traits("ArialMT", "Arial", false, false))
            .is_empty());
        assert!(fonts.file("timesnewromanbold").is_some());
    }

    #[test]
    fn what_is_not_a_font_has_no_name() {
        let dir = tempfile::tempdir().unwrap();
        let junk = dir.path().join("junk.ttf");
        std::fs::write(&junk, b"not a font at all, only some bytes").unwrap();
        assert_eq!(full_name(&junk), None);
        assert!(named_files(dir.path()).is_empty());
    }
}

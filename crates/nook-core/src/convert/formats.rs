//! The formats the document converter reads and writes, each with its kind, its file extension
//! and the other extensions it goes by.

use serde::Serialize;

/// What a format holds; the format picker groups targets by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Kind {
    /// Word processor files: Word, OpenDocument, RTF.
    Document,
    Pdf,
    /// Web pages.
    Web,
    /// Text with light markup: Markdown, plain text, LaTeX and their kin.
    Text,
    Ebook,
    /// Tables: workbooks, CSV, JSON rows.
    Sheet,
    Slides,
    Image,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Format {
    /// The id the UI and the router use ("docx").
    pub id: &'static str,
    /// What the picker calls it ("Word document").
    pub name: &'static str,
    /// The extension written.
    pub ext: &'static str,
    /// Other extensions read as this format.
    pub also: &'static [&'static str],
    pub kind: Kind,
}

const fn f(
    id: &'static str,
    name: &'static str,
    also: &'static [&'static str],
    kind: Kind,
) -> Format {
    Format {
        id,
        name,
        ext: id,
        also,
        kind,
    }
}

/// Every format, in the picker's order within each kind.
pub const FORMATS: &[Format] = &[
    f("pdf", "PDF", &[], Kind::Pdf),
    f("docx", "Word document", &["docm", "dotx"], Kind::Document),
    f("doc", "Word 97-2003 document", &["dot"], Kind::Document),
    f("odt", "OpenDocument text", &[], Kind::Document),
    f("rtf", "Rich Text", &[], Kind::Document),
    f("html", "Web page", &["htm", "xhtml"], Kind::Web),
    f("md", "Markdown", &["markdown"], Kind::Text),
    f("txt", "Plain text", &["text"], Kind::Text),
    f("tex", "LaTeX", &["latex"], Kind::Text),
    f("rst", "reStructuredText", &[], Kind::Text),
    f("org", "Org", &[], Kind::Text),
    f("adoc", "AsciiDoc", &["asciidoc"], Kind::Text),
    f("typ", "Typst", &[], Kind::Text),
    f("wiki", "MediaWiki", &["mediawiki"], Kind::Text),
    f("ipynb", "Jupyter notebook", &[], Kind::Text),
    f("epub", "EPUB e-book", &[], Kind::Ebook),
    f("fb2", "FictionBook", &[], Kind::Ebook),
    f("xlsx", "Excel workbook", &["xlsm", "xltx"], Kind::Sheet),
    f("xls", "Excel 97-2003 workbook", &["xlt"], Kind::Sheet),
    f("ods", "OpenDocument spreadsheet", &[], Kind::Sheet),
    f("csv", "CSV", &[], Kind::Sheet),
    f("tsv", "Tab-separated values", &["tab"], Kind::Sheet),
    f("json", "JSON rows", &[], Kind::Sheet),
    f(
        "pptx",
        "PowerPoint presentation",
        &["pptm", "ppsx"],
        Kind::Slides,
    ),
    f(
        "ppt",
        "PowerPoint 97-2003 presentation",
        &["pps"],
        Kind::Slides,
    ),
    f("odp", "OpenDocument presentation", &[], Kind::Slides),
    f("png", "PNG picture", &[], Kind::Image),
    f("jpg", "JPEG picture", &["jpeg", "jfif", "jpe"], Kind::Image),
    f("webp", "WebP picture", &[], Kind::Image),
    f("bmp", "Bitmap picture", &["dib"], Kind::Image),
    f("tiff", "TIFF picture", &["tif"], Kind::Image),
    f("gif", "GIF picture", &[], Kind::Image),
    f("ico", "Windows icon", &[], Kind::Image),
];

/// The format with this id.
pub fn by_id(id: &str) -> Option<&'static Format> {
    FORMATS.iter().find(|f| f.id.eq_ignore_ascii_case(id))
}

/// The format a file is, by its extension.
pub fn of_path(path: &std::path::Path) -> Option<&'static Format> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    FORMATS
        .iter()
        .find(|f| f.ext == ext || f.also.contains(&ext.as_str()))
}

/// A format named in words ("to word", "as a pdf", "into excel"): its id.
pub fn named(word: &str) -> Option<&'static str> {
    let w = word.trim().to_lowercase();
    let id = match w.as_str() {
        "pdf" | "pdfs" => "pdf",
        "word" | "docx" | "msword" => "docx",
        "doc" => "doc",
        "odt" | "opendocument" | "writer" => "odt",
        "rtf" => "rtf",
        "html" | "htm" | "web" | "webpage" | "website" => "html",
        "markdown" | "md" => "md",
        "text" | "txt" | "plain" | "plaintext" => "txt",
        "latex" | "tex" => "tex",
        "rst" | "restructuredtext" => "rst",
        "org" => "org",
        "asciidoc" | "adoc" => "adoc",
        "typst" | "typ" => "typ",
        "mediawiki" | "wiki" | "wikitext" => "wiki",
        "jupyter" | "ipynb" | "notebook" => "ipynb",
        "epub" | "ebook" | "e-book" | "kindle" => "epub",
        "fb2" | "fictionbook" => "fb2",
        "excel" | "xlsx" | "spreadsheet" | "workbook" => "xlsx",
        "xls" => "xls",
        "ods" | "calc" => "ods",
        "csv" => "csv",
        "tsv" => "tsv",
        "json" => "json",
        "powerpoint" | "pptx" | "slides" | "presentation" | "deck" => "pptx",
        "ppt" => "ppt",
        "odp" | "impress" => "odp",
        "png" => "png",
        "jpg" | "jpeg" | "jpegs" | "jpgs" => "jpg",
        "webp" => "webp",
        "bmp" | "bitmap" => "bmp",
        "tiff" | "tif" => "tiff",
        "gif" => "gif",
        "ico" | "icon" | "favicon" => "ico",
        "image" | "images" | "picture" | "pictures" | "photo" | "photos" => "png",
        _ => return None,
    };
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_known_by_their_extensions() {
        assert_eq!(of_path("a/Report.DOCX".as_ref()).unwrap().id, "docx");
        assert_eq!(of_path("scan.jpeg".as_ref()).unwrap().id, "jpg");
        assert_eq!(of_path("page.htm".as_ref()).unwrap().id, "html");
        assert_eq!(of_path("notes.markdown".as_ref()).unwrap().id, "md");
        assert!(of_path("song.mp3".as_ref()).is_none());
        assert!(of_path("README".as_ref()).is_none());
        assert_eq!(by_id("XLSX").unwrap().kind, Kind::Sheet);
        assert_eq!(named("Excel"), Some("xlsx"));
        assert_eq!(named("word"), Some("docx"));
        assert_eq!(named("nope"), None);
        let mut ids: Vec<&str> = FORMATS.iter().map(|f| f.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), FORMATS.len(), "ids are unique");
    }
}

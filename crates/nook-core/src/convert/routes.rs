//! How a file becomes another format: the steps, each done by an engine. Office files go through
//! an office suite, so they keep their layout: Word, Excel or PowerPoint when they are on the
//! computer, else LibreOffice. Text documents go through Pandoc, web pages to PDF through Edge,
//! PDFs through PDFium, pictures and tables through Nook itself.

use serde::{Deserialize, Serialize};

use super::formats::{Format, Kind};

/// What does a conversion's work. Pandoc, PDFium and LibreOffice are downloaded when a
/// conversion needs them; Edge and Microsoft Office are the computer's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Engine {
    Pandoc,
    Pdf,
    Edge,
    MsOffice,
    LibreOffice,
}

/// The office program a step runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum App {
    Word,
    Excel,
    PowerPoint,
    Libre,
}

/// One step of a conversion; each takes the file the step before it wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Pandoc, between two of its formats.
    Pandoc {
        from: &'static str,
        to: &'static str,
    },
    /// A web page printed to PDF by Edge.
    Print,
    /// Opened in an office program and saved as `to` (a format id).
    Office { app: App, to: &'static str },
    /// A PDF's pages drawn as pictures.
    PdfPages { to: &'static str },
    /// A PDF's text read out as Markdown (or plain text), paragraphs and headings found by size.
    PdfText { plain: bool },
    /// Pictures on PDF pages, one each.
    ImagesToPdf,
    /// A picture in another picture format.
    Image { to: &'static str },
    /// A table (a workbook's sheets, CSV, JSON rows) written as another.
    Table { to: &'static str },
}

impl Step {
    pub fn engine(&self) -> Option<Engine> {
        match self {
            Step::Pandoc { .. } => Some(Engine::Pandoc),
            Step::Print => Some(Engine::Edge),
            Step::Office {
                app: App::Libre, ..
            } => Some(Engine::LibreOffice),
            Step::Office { .. } => Some(Engine::MsOffice),
            Step::PdfPages { .. } | Step::PdfText { .. } | Step::ImagesToPdf => Some(Engine::Pdf),
            Step::Image { .. } | Step::Table { .. } => None,
        }
    }
}

/// The office programs on this computer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Have {
    pub word: bool,
    pub excel: bool,
    pub powerpoint: bool,
}

impl Have {
    fn app(&self, kind: Kind) -> App {
        match kind {
            Kind::Sheet if self.excel => App::Excel,
            Kind::Slides if self.powerpoint => App::PowerPoint,
            Kind::Document if self.word => App::Word,
            _ => App::Libre,
        }
    }
}

/// Pandoc's name for reading a format.
pub fn pandoc_reads(id: &str) -> Option<&'static str> {
    Some(match id {
        "docx" => "docx",
        "odt" => "odt",
        "rtf" => "rtf",
        "html" => "html",
        "md" => "markdown",
        // Plain text is Markdown as far as it goes, its line breaks kept.
        "txt" => "markdown+hard_line_breaks",
        "tex" => "latex",
        "rst" => "rst",
        "org" => "org",
        "adoc" => "asciidoc",
        "typ" => "typst",
        "wiki" => "mediawiki",
        "ipynb" => "ipynb",
        "epub" => "epub",
        "fb2" => "fb2",
        "pptx" => "pptx",
        _ => return None,
    })
}

/// Pandoc's name for writing a format.
pub fn pandoc_writes(id: &str) -> Option<&'static str> {
    Some(match id {
        "docx" => "docx",
        "odt" => "odt",
        "rtf" => "rtf",
        "html" => "html5",
        "md" => "gfm",
        "txt" => "plain",
        "tex" => "latex",
        "rst" => "rst",
        "org" => "org",
        "adoc" => "asciidoc",
        "typ" => "typst",
        "wiki" => "mediawiki",
        "ipynb" => "ipynb",
        "epub" => "epub3",
        "fb2" => "fb2",
        "pptx" => "pptx",
        _ => return None,
    })
}

const OFFICE_DOCS: &[&str] = &["docx", "doc", "odt", "rtf"];
const WORKBOOKS: &[&str] = &["xlsx", "xls", "ods"];
const PAGE_PICTURES: &[&str] = &["png", "jpg", "webp", "tiff", "bmp"];

/// The steps from `from` to `to`, None when Nook cannot make one into the other (or they are
/// the same).
pub fn route(from: &Format, to: &Format, have: &Have) -> Option<Vec<Step>> {
    use Kind::*;
    if from.id == to.id {
        return None;
    }
    let pandoc = |a: &str, b: &str| -> Option<Step> {
        Some(Step::Pandoc {
            from: pandoc_reads(a)?,
            to: pandoc_writes(b)?,
        })
    };
    let office = |kind: Kind, to: &'static str| Step::Office {
        app: have.app(kind),
        to,
    };
    let steps = match (from.kind, to.kind) {
        (Image, Image) => vec![Step::Image { to: to.id }],
        (Image, Pdf) => vec![Step::ImagesToPdf],
        (Pdf, Image) if PAGE_PICTURES.contains(&to.id) => vec![Step::PdfPages { to: to.id }],
        (Pdf, Text) if to.id == "txt" => vec![Step::PdfText { plain: true }],
        (Pdf, Text) if to.id == "md" => vec![Step::PdfText { plain: false }],
        // (Word reads a PDF better, but a hidden Word stops on a question no one can answer.)
        (Pdf, Document) if to.id == "doc" => vec![
            Step::PdfText { plain: false },
            pandoc("md", "docx")?,
            office(Document, "doc"),
        ],
        (Pdf, Document | Text | Web | Ebook) => {
            vec![Step::PdfText { plain: false }, pandoc("md", to.id)?]
        }

        (Document, Pdf | Document) => vec![office(Document, to.id)],
        (Document, Image) if PAGE_PICTURES.contains(&to.id) => {
            vec![office(Document, "pdf"), Step::PdfPages { to: to.id }]
        }
        (Document, Text | Web | Ebook | Slides) if to.id != "ppt" && to.id != "odp" => {
            if from.id == "doc" {
                vec![office(Document, "docx"), pandoc("docx", to.id)?]
            } else {
                vec![pandoc(from.id, to.id)?]
            }
        }

        (Web, Pdf) => vec![Step::Print],
        (Text | Ebook, Pdf) => vec![pandoc(from.id, "html")?, Step::Print],
        (Text | Web | Ebook, Document) if to.id == "doc" => {
            vec![pandoc(from.id, "docx")?, office(Document, "doc")]
        }
        (Text | Web | Ebook, Document | Text | Web | Ebook) => vec![pandoc(from.id, to.id)?],
        (Text | Web | Ebook, Slides) if to.id == "pptx" => vec![pandoc(from.id, "pptx")?],
        (Text | Web | Ebook, Slides) => vec![pandoc(from.id, "pptx")?, office(Slides, to.id)],

        (Sheet, Sheet) if ["csv", "tsv", "json", "xlsx"].contains(&to.id) => {
            vec![Step::Table { to: to.id }]
        }
        (Sheet, Sheet) if WORKBOOKS.contains(&from.id) => vec![office(Sheet, to.id)],
        (Sheet, Sheet) => vec![Step::Table { to: "xlsx" }, office(Sheet, to.id)],
        (Sheet, Web) => vec![Step::Table { to: "html" }],
        (Sheet, Text) if to.id == "md" => vec![Step::Table { to: "md" }],
        (Sheet, Pdf) if WORKBOOKS.contains(&from.id) => vec![office(Sheet, "pdf")],
        (Sheet, Pdf) => vec![Step::Table { to: "html" }, Step::Print],

        (Slides, Pdf | Slides) => vec![office(Slides, to.id)],
        (Slides, Image) if PAGE_PICTURES.contains(&to.id) => {
            vec![office(Slides, "pdf"), Step::PdfPages { to: to.id }]
        }
        (Slides, Text | Web) if from.id == "pptx" => vec![pandoc("pptx", to.id)?],
        (Slides, Text | Web) => vec![office(Slides, "pptx"), pandoc("pptx", to.id)?],
        _ => return None,
    };
    // An office step between office formats keeps to the formats the suite writes.
    for s in &steps {
        if let Step::Office { app, to } = s {
            let writes = match app {
                App::Word => OFFICE_DOCS.contains(to) || *to == "pdf",
                App::Excel => WORKBOOKS.contains(to) || *to == "pdf",
                App::PowerPoint => ["pptx", "ppt", "odp", "pdf"].contains(to),
                App::Libre => true,
            };
            if !writes {
                return None;
            }
        }
    }
    Some(steps)
}

/// The engines a route needs, once each, in order.
pub fn engines(steps: &[Step]) -> Vec<Engine> {
    let mut out: Vec<Engine> = Vec::new();
    for e in steps.iter().filter_map(Step::engine) {
        if !out.contains(&e) {
            out.push(e);
        }
    }
    out
}

/// Whether `from` goes by way of an office suite to `to` (for the plan's words).
pub fn uses_office(steps: &[Step]) -> bool {
    steps.iter().any(|s| matches!(s, Step::Office { .. }))
}

#[cfg(test)]
mod tests {
    use super::super::formats::{by_id, FORMATS};
    use super::*;

    fn r(from: &str, to: &str, have: Have) -> Option<Vec<Step>> {
        route(by_id(from).unwrap(), by_id(to).unwrap(), &have)
    }

    const NONE: Have = Have {
        word: false,
        excel: false,
        powerpoint: false,
    };
    const ALL: Have = Have {
        word: true,
        excel: true,
        powerpoint: true,
    };

    #[test]
    fn office_files_go_through_an_office_suite_word_when_there_is_one() {
        assert_eq!(
            r("docx", "pdf", ALL).unwrap(),
            [Step::Office {
                app: App::Word,
                to: "pdf"
            }]
        );
        assert_eq!(
            r("docx", "pdf", NONE).unwrap(),
            [Step::Office {
                app: App::Libre,
                to: "pdf"
            }]
        );
        assert_eq!(engines(&r("xlsx", "pdf", ALL).unwrap()), [Engine::MsOffice]);
        assert_eq!(
            engines(&r("pptx", "odp", NONE).unwrap()),
            [Engine::LibreOffice]
        );
        assert_eq!(
            r("pptx", "png", ALL).unwrap(),
            [
                Step::Office {
                    app: App::PowerPoint,
                    to: "pdf"
                },
                Step::PdfPages { to: "png" }
            ]
        );
    }

    #[test]
    fn text_goes_through_pandoc_and_to_pdf_through_edge() {
        assert_eq!(
            r("md", "docx", NONE).unwrap(),
            [Step::Pandoc {
                from: "markdown",
                to: "docx"
            }]
        );
        assert_eq!(
            engines(&r("md", "pdf", NONE).unwrap()),
            [Engine::Pandoc, Engine::Edge]
        );
        assert_eq!(r("html", "pdf", NONE).unwrap(), [Step::Print]);
        assert_eq!(
            engines(&r("docx", "md", NONE).unwrap()),
            [Engine::Pandoc],
            "a Word file's text needs no office suite"
        );
        assert_eq!(
            engines(&r("doc", "md", NONE).unwrap()),
            [Engine::LibreOffice, Engine::Pandoc],
            "an old Word file is opened by one first"
        );
    }

    #[test]
    fn pdfs_become_pictures_text_or_editable_documents() {
        assert_eq!(
            r("pdf", "jpg", NONE).unwrap(),
            [Step::PdfPages { to: "jpg" }]
        );
        assert_eq!(
            r("pdf", "txt", NONE).unwrap(),
            [Step::PdfText { plain: true }]
        );
        for have in [ALL, NONE] {
            assert_eq!(
                engines(&r("pdf", "docx", have).unwrap()),
                [Engine::Pdf, Engine::Pandoc]
            );
        }
        assert_eq!(
            engines(&r("pdf", "doc", ALL).unwrap()),
            [Engine::Pdf, Engine::Pandoc, Engine::MsOffice]
        );
        assert!(r("pdf", "xlsx", ALL).is_none());
        assert!(
            r("pdf", "gif", ALL).is_none(),
            "pages as GIF are not offered"
        );
    }

    #[test]
    fn tables_and_pictures_need_no_engine() {
        assert_eq!(engines(&r("csv", "xlsx", NONE).unwrap()), []);
        assert_eq!(engines(&r("ods", "json", NONE).unwrap()), []);
        assert_eq!(engines(&r("png", "ico", NONE).unwrap()), []);
        assert_eq!(engines(&r("csv", "pdf", NONE).unwrap()), [Engine::Edge]);
        assert_eq!(
            engines(&r("xls", "ods", NONE).unwrap()),
            [Engine::LibreOffice]
        );
        assert_eq!(engines(&r("jpg", "pdf", NONE).unwrap()), [Engine::Pdf]);
    }

    #[test]
    fn every_format_goes_somewhere_and_nothing_to_itself() {
        for from in FORMATS {
            assert!(r(from.id, from.id, ALL).is_none());
            let reachable = FORMATS
                .iter()
                .filter(|to| r(from.id, to.id, NONE).is_some())
                .count();
            assert!(reachable >= 2, "{} goes to {reachable} formats", from.id);
        }
    }
}

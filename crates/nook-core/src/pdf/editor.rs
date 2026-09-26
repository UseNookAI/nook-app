//! The PDF editor's engine: PDFium on a thread of its own, holding the open documents.
//!
//! PDFium is not made for several threads, and its documents borrow the library, so one thread
//! owns both and every call is a job sent to it. Documents are read into memory, so the file on
//! disk is never held open and can be saved over.
//!
//! An edit replaces text objects, not pixels: the first object of a line takes the new text in
//! the line's own font, size, colour, spacing and position, and the rest of the line's objects go.
//! A PDF embeds only the glyphs it uses, so the new text is read back from the object: when a
//! character did not make it (or the embedded subset never had it), the line is set again in the
//! same font installed on Windows, at the same size and place.
//!
//! Text that is part of a picture (a scan, a form printed flat, letters turned into shapes) has
//! no text objects: the page there is drawn at 288 dpi and read by Windows, and the letters' ink
//! gives their colour, the paper's, their baseline and height, and the installed face they are
//! set in. An edit covers the old letters with the paper's colour and sets the new text over
//! them as text, so it can be changed again like any other. Or, asked for, out of the page's own
//! letters: each word the page shows is cut into its letters, and the new text is made of them
//! (the face's where the page has none), laid over the old as a picture with the words beneath it
//! as invisible text, as a scan's text recognition leaves them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

use anyhow::{anyhow, bail, Context, Result};
use image::ImageEncoder;
use parking_lot::Mutex;
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};

use super::fontmatch::{self, Kind};
use super::fonts::{self, FontTraits, SystemFonts};
use super::layout::{self, Line, Rect, Run};
use super::ocr;
use super::raster::{self, Pixels, PxBox};
use crate::convert::images;
use crate::convert::pdftext::{PageText, TextLine};

/// How many edits Undo can take back.
pub const UNDO_DEPTH: usize = 30;
/// The widest page image drawn, in pixels.
pub const MAX_RENDER_WIDTH: i32 = 3200;
/// Pixels per point a page is drawn at to read text in a picture (288 dpi).
const READ_SCALE: f32 = 4.0;
/// How far above and below a click the page is read, in points (right across it sideways).
const READ_BAND: f32 = 40.0;
const NO_TEXT: &str = "There is no text there. Drag across the words you want to change.";

/// An open document, as the page shows it.
///
/// - `pages`: each page's size in points and its version (bumped by every edit to it, so the page
///   knows to draw it again)
/// - `edits`: edits since it was opened; `dirty`: some are not saved yet
/// - `saved_to`: where it was last saved
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfDoc {
    pub id: String,
    pub name: String,
    pub path: String,
    pub pages: Vec<PdfPageInfo>,
    pub edits: u32,
    pub can_undo: bool,
    pub dirty: bool,
    pub saved_to: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfPageInfo {
    pub width: f32,
    pub height: f32,
    pub version: u32,
}

/// Where the person pointed: a dragged box, or a click.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Area {
    Rect { rect: Rect },
    Point { x: f32, y: f32 },
}

/// Which edge of a line stays put when its length changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Align {
    #[default]
    Left,
    Right,
    Center,
}

/// How the picked text looks, for the editing box.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockFont {
    pub name: String,
    pub family: String,
    pub bold: bool,
    pub italic: bool,
    pub serif: bool,
    pub mono: bool,
    /// "#1f4e79".
    pub color: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockLine {
    /// The page objects the line is made of.
    pub objects: Vec<usize>,
    pub text: String,
    pub rect: Rect,
    pub baseline: f32,
    /// The font size on the page, in points.
    pub size: f32,
}

/// Picked text: its lines, where they are, and how they look. Handed back with the new text to
/// [`PdfEditor::replace`]; `version` says which state of the page it was picked from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Block {
    pub page: u32,
    pub version: u32,
    pub lines: Vec<BlockLine>,
    pub rect: Rect,
    pub font: BlockFont,
    /// The edge kept by default: the right one for figures.
    pub align: Align,
    /// Set when the text is part of a picture, not text objects.
    #[serde(default)]
    pub drawn: Option<Drawn>,
}

/// Text that is part of a picture: the paper's colour, laid over the old letters, and the
/// installed face they matched, for the new text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Drawn {
    /// "#fbfaf6".
    pub paper: String,
    /// The face's key (see `fontmatch::FACES`).
    pub face: String,
    /// How much wider the letters are than the face at their size; the new text is widened as
    /// much.
    pub stretch: f32,
    /// Set by the person: the new text is made of letters cut from the page itself.
    #[serde(default)]
    pub from_page: bool,
}

/// What a pick found: text, or why there is none.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pick {
    pub block: Option<Block>,
    pub why: Option<String>,
}

/// An edit done: the document now, and what the person should know (a font from Windows).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Replaced {
    pub doc: PdfDoc,
    pub note: Option<String>,
}

type Job = Box<dyn FnOnce(&mut Engine) + Send>;

/// The open documents and the thread that works on them. Build one for the app; the thread and
/// the library start with the first call.
pub struct PdfEditor {
    /// Where `pdfium.dll` is when it is installed.
    library: Box<dyn Fn() -> Option<PathBuf> + Send + Sync>,
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
    next: AtomicU64,
    /// Open documents with changes not saved yet, counted by the engine's thread after each job,
    /// so an automatic update waits for them ([`BusyWork`](crate::busy::BusyWork)).
    unsaved: Arc<AtomicUsize>,
}

impl PdfEditor {
    pub fn new(library: impl Fn() -> Option<PathBuf> + Send + Sync + 'static) -> PdfEditor {
        PdfEditor {
            library: Box::new(library),
            jobs: Mutex::new(None),
            next: AtomicU64::new(1),
            unsaved: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Whether PDFium is installed (the engine can start).
    pub fn available(&self) -> bool {
        self.jobs.lock().is_some() || (self.library)().is_some_and(|p| p.is_file())
    }

    /// The engine's thread, started with the library on first use.
    fn sender(&self) -> Result<mpsc::Sender<Job>> {
        let mut jobs = self.jobs.lock();
        if let Some(tx) = jobs.as_ref() {
            return Ok(tx.clone());
        }
        let library = (self.library)()
            .filter(|p| p.is_file())
            .ok_or_else(|| anyhow!("The PDF engine is not installed yet."))?;
        let (tx, rx) = mpsc::channel::<Job>();
        let (ready_tx, ready_rx) = mpsc::channel::<std::result::Result<(), String>>();
        let unsaved = self.unsaved.clone();
        std::thread::Builder::new()
            .name("nook-pdf".into())
            .spawn(move || {
                let bindings = match Pdfium::bind_to_library(&library) {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("The PDF engine did not start: {e}")));
                        return;
                    }
                };
                // The documents borrow the library for as long as the app runs.
                let pdfium: &'static Pdfium = Box::leak(Box::new(Pdfium::new(bindings)));
                let _ = ready_tx.send(Ok(()));
                let mut engine = Engine {
                    pdfium,
                    docs: HashMap::new(),
                    fonts: None,
                };
                while let Ok(job) = rx.recv() {
                    job(&mut engine);
                    let count = engine.docs.values().filter(|d| d.dirty).count();
                    unsaved.store(count, Ordering::SeqCst);
                }
            })
            .context("Could not start the PDF engine's thread")?;
        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(why)) => bail!(why),
            Err(_) => bail!("The PDF engine did not start."),
        }
        *jobs = Some(tx.clone());
        Ok(tx)
    }

    async fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Engine) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let tx = self.sender()?;
        let (reply, answer) = tokio::sync::oneshot::channel();
        tx.send(Box::new(move |engine| {
            let _ = reply.send(work(engine));
        }))
        .map_err(|_| anyhow!("The PDF engine stopped."))?;
        answer
            .await
            .map_err(|_| anyhow!("The PDF engine stopped."))?
    }

    /// Opens a PDF (read into memory).
    pub async fn open(&self, path: &Path) -> Result<PdfDoc> {
        let id = format!("pdf_{}", self.next.fetch_add(1, Ordering::SeqCst));
        let path = path.to_path_buf();
        self.call(move |e| e.open(id, path)).await
    }

    /// Every open document.
    pub async fn docs(&self) -> Result<Vec<PdfDoc>> {
        if self.jobs.lock().is_none() {
            return Ok(Vec::new());
        }
        self.call(|e| {
            let mut all: Vec<PdfDoc> = e.docs.values().map(Doc::info).collect();
            all.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(all)
        })
        .await
    }

    /// A page drawn `width` pixels wide, as PNG.
    pub async fn render(&self, id: &str, page: u32, width: i32) -> Result<Vec<u8>> {
        let id = id.to_string();
        self.call(move |e| e.render(&id, page, width)).await
    }

    /// The text under a click or in a dragged box, as lines ready to edit.
    pub async fn pick(&self, id: &str, page: u32, area: Area) -> Result<Pick> {
        let id = id.to_string();
        self.call(move |e| e.pick(&id, page, area)).await
    }

    /// Puts `texts` in place of the picked lines, one for each; more lines go below the last, an
    /// empty one removes its line.
    pub async fn replace(
        &self,
        id: &str,
        block: Block,
        texts: Vec<String>,
        align: Align,
    ) -> Result<Replaced> {
        let id = id.to_string();
        self.call(move |e| e.replace(&id, block, texts, align))
            .await
    }

    /// Takes the last edit back.
    pub async fn undo(&self, id: &str) -> Result<PdfDoc> {
        let id = id.to_string();
        self.call(move |e| e.undo(&id)).await
    }

    /// Saves to `to`, or beside the original as "<name> (edited).pdf".
    pub async fn save(&self, id: &str, to: Option<PathBuf>) -> Result<PdfDoc> {
        let id = id.to_string();
        self.call(move |e| e.save(&id, to)).await
    }

    /// A PDF's text, page by page and line by line, as PDFium reads it out (in the order the
    /// PDF draws it, which is the reading order of most documents); a page with no text (a scan)
    /// is read by Windows' text recognition. For the document converter.
    pub async fn text_lines(&self, path: &Path) -> Result<Vec<PageText>> {
        let path = path.to_path_buf();
        self.call(move |e| e.text_lines(&path)).await
    }

    /// A PDF's pages drawn at `dpi` and written as `to` pictures (a format id): `one` when it
    /// has one page, else `<dir>/<name> <n>.<ext>`. Returns what was written.
    pub async fn page_pictures(
        &self,
        path: &Path,
        to: &str,
        dpi: f32,
        one: &Path,
        dir: &Path,
    ) -> Result<Vec<PathBuf>> {
        let (path, to, one, dir) = (
            path.to_path_buf(),
            to.to_string(),
            one.to_path_buf(),
            dir.to_path_buf(),
        );
        self.call(move |e| e.page_pictures(&path, &to, dpi, &one, &dir))
            .await
    }

    /// Pictures on the pages of a new PDF, one each, fitted to an A4 page turned their way;
    /// `work` is for pictures made JPEG on the way (a JPEG goes in as it is).
    pub async fn pictures_pdf(
        &self,
        pictures: Vec<PathBuf>,
        out: &Path,
        work: &Path,
    ) -> Result<()> {
        let (out, work) = (out.to_path_buf(), work.to_path_buf());
        self.call(move |e| e.pictures_pdf(&pictures, &out, &work))
            .await
    }

    pub async fn close(&self, id: &str) -> Result<()> {
        if self.jobs.lock().is_none() {
            return Ok(());
        }
        let id = id.to_string();
        self.call(move |e| {
            e.docs.remove(&id);
            Ok(())
        })
        .await
    }
}

impl crate::busy::BusyWork for PdfEditor {
    fn busy_with(&self) -> Option<String> {
        (self.unsaved.load(Ordering::SeqCst) > 0)
            .then(|| "a PDF has changes that are not saved yet".to_string())
    }
}

struct Doc {
    id: String,
    path: PathBuf,
    document: PdfDocument<'static>,
    undo: Vec<Vec<u8>>,
    edits: u32,
    dirty: bool,
    saved_to: Option<PathBuf>,
    versions: Vec<u32>,
    /// The characters seen with each font, read once: a subset has glyphs for these only.
    known: Option<HashMap<String, HashSet<char>>>,
    /// Windows fonts loaded into this document, by file.
    loaded: HashMap<PathBuf, PdfFontToken>,
    /// The letters each page has to give, cut once when first asked for.
    cuts: HashMap<u32, Arc<PageCuts>>,
}

impl Doc {
    fn info(&self) -> PdfDoc {
        let pages = self
            .document
            .pages()
            .iter()
            .enumerate()
            .map(|(i, p)| PdfPageInfo {
                width: p.width().value,
                height: p.height().value,
                version: self.versions.get(i).copied().unwrap_or(0),
            })
            .collect();
        PdfDoc {
            id: self.id.clone(),
            name: self
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: self.path.display().to_string(),
            pages,
            edits: self.edits,
            can_undo: !self.undo.is_empty(),
            dirty: self.dirty,
            saved_to: self.saved_to.as_ref().map(|p| p.display().to_string()),
        }
    }

    fn known(&mut self) -> &HashMap<String, HashSet<char>> {
        if self.known.is_none() {
            let mut known: HashMap<String, HashSet<char>> = HashMap::new();
            for page in self.document.pages().iter() {
                for object in page.objects().iter() {
                    if let Some(t) = object.as_text_object() {
                        known
                            .entry(t.font().name())
                            .or_default()
                            .extend(t.text().chars());
                    }
                }
            }
            self.known = Some(known);
        }
        self.known.get_or_insert_with(HashMap::new)
    }
}

struct Engine {
    pdfium: &'static Pdfium,
    docs: HashMap<String, Doc>,
    fonts: Option<SystemFonts>,
}

fn pdf_error(e: PdfiumError) -> anyhow::Error {
    match e {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => {
            anyhow!("This PDF is locked with a password.")
        }
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::FormatError) => {
            anyhow!("This file is not a PDF Nook can read.")
        }
        other => anyhow!("{other}"),
    }
}

/// The page's text objects as runs; text turned or slanted on the page is left out.
fn runs(page: &PdfPage) -> Vec<Run> {
    page.objects()
        .iter()
        .enumerate()
        .filter_map(|(index, object)| {
            let t = object.as_text_object()?;
            // A scan's own text layer (from its text recognition, drawn invisibly) is not what is
            // seen: the picture's letters are edited, and the layer goes with them.
            if matches!(t.render_mode(), PdfPageTextRenderMode::Invisible) {
                return None;
            }
            let m = t.matrix().ok()?;
            if m.b().abs() > 0.01 || m.c().abs() > 0.01 {
                return None;
            }
            let b = t.bounds().ok()?.to_rect();
            let size = t.scaled_font_size().value;
            let color = t.fill_color().map(hex).unwrap_or_default();
            Some(Run {
                index,
                text: t.text(),
                bounds: Rect::new(
                    b.left().value,
                    b.bottom().value,
                    b.right().value,
                    b.top().value,
                ),
                origin_x: m.e(),
                baseline: m.f(),
                size,
                style: format!("{} {size:.2} {color}", t.font().name()),
            })
        })
        .collect()
}

fn hex(c: PdfColor) -> String {
    format!("#{:02x}{:02x}{:02x}", c.red(), c.green(), c.blue())
}

fn traits_of(font: &PdfFont) -> FontTraits {
    let bold = match font.weight() {
        Ok(
            PdfFontWeight::Weight600
            | PdfFontWeight::Weight700Bold
            | PdfFontWeight::Weight800
            | PdfFontWeight::Weight900,
        ) => true,
        Ok(PdfFontWeight::Custom(w)) => w >= 600,
        _ => font.is_bold_reenforced(),
    };
    let name = font.name();
    let lower = fonts::base_name(&name).to_lowercase();
    FontTraits {
        family: font.family(),
        bold: bold || lower.contains("bold") || lower.contains("black") || lower.contains("heavy"),
        italic: font.is_italic() || lower.contains("italic") || lower.contains("oblique"),
        serif: font.is_serif(),
        mono: font.is_fixed_pitch(),
        name,
    }
}

/// Whether `wanted` came back as it was set (spaces aside): a character the font cannot write
/// is dropped by PDFium.
fn came_through(wanted: &str, got: &str) -> bool {
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    squash(wanted) == squash(got)
}

impl Engine {
    fn doc(&mut self, id: &str) -> Result<&mut Doc> {
        self.docs
            .get_mut(id)
            .ok_or_else(|| anyhow!("That PDF is not open any more."))
    }

    fn open(&mut self, id: String, path: PathBuf) -> Result<PdfDoc> {
        let bytes =
            std::fs::read(&path).with_context(|| format!("Could not read {}", path.display()))?;
        let document = self
            .pdfium
            .load_pdf_from_byte_vec(bytes, None)
            .map_err(pdf_error)?;
        let pages = document.pages().len() as usize;
        if pages == 0 {
            bail!("This PDF has no pages.");
        }
        let doc = Doc {
            id: id.clone(),
            path,
            document,
            undo: Vec::new(),
            edits: 0,
            dirty: false,
            saved_to: None,
            versions: vec![0; pages],
            known: None,
            loaded: HashMap::new(),
            cuts: HashMap::new(),
        };
        let info = doc.info();
        self.docs.insert(id, doc);
        Ok(info)
    }

    fn render(&mut self, id: &str, page: u32, width: i32) -> Result<Vec<u8>> {
        let doc = self.doc(id)?;
        let p = doc.document.pages().get(page as _).map_err(pdf_error)?;
        let config = PdfRenderConfig::new()
            .set_target_width(width.clamp(16, MAX_RENDER_WIDTH) as _)
            .render_form_data(true)
            .render_annotations(true);
        let image = p
            .render_with_config(&config)
            .map_err(pdf_error)?
            .as_image()
            .map_err(pdf_error)?
            .into_rgb8();
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new_with_quality(
            &mut png,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Adaptive,
        )
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgb8,
        )
        .context("Could not draw the page")?;
        Ok(png)
    }

    fn pick(&mut self, id: &str, page: u32, area: Area) -> Result<Pick> {
        let doc = self.doc(id)?;
        let version = doc.versions.get(page as usize).copied().unwrap_or(0);
        let p = doc.document.pages().get(page as _).map_err(pdf_error)?;
        let all = runs(&p);
        let lines: Vec<Line> = match area {
            Area::Point { x, y } => layout::line_at(&all, x, y).into_iter().collect(),
            Area::Rect { rect } => layout::lines(layout::covered(&all, rect)),
        };
        if lines.is_empty() {
            drop(p);
            // No text objects there: perhaps letters that are part of a picture.
            return self.pick_drawn(id, page, area);
        }
        let first = lines[0]
            .runs
            .iter()
            .find(|r| !r.text.trim().is_empty())
            .map(|r| r.index)
            .unwrap_or(lines[0].runs[0].index);
        let object = p.objects().get(first as _).map_err(pdf_error)?;
        let t = object
            .as_text_object()
            .ok_or_else(|| anyhow!("The page changed while it was read."))?;
        let traits = traits_of(&t.font());
        let font = BlockFont {
            name: traits.name.clone(),
            family: if traits.family.is_empty() {
                fonts::base_name(&traits.name)
                    .split(['-', ','])
                    .next()
                    .unwrap_or("")
                    .to_string()
            } else {
                traits.family.clone()
            },
            bold: traits.bold,
            italic: traits.italic,
            serif: traits.serif,
            mono: traits.mono,
            color: t.fill_color().map(hex).unwrap_or_else(|_| "#000000".into()),
        };
        let rect = lines
            .iter()
            .skip(1)
            .fold(lines[0].bounds, |acc, l| acc.union(&l.bounds));
        let align = if lines.iter().all(|l| layout::is_figure(&l.text)) {
            Align::Right
        } else {
            Align::Left
        };
        Ok(Pick {
            block: Some(Block {
                page,
                version,
                lines: lines
                    .into_iter()
                    .map(|l| BlockLine {
                        objects: l.runs.iter().map(|r| r.index).collect(),
                        text: l.text,
                        rect: l.bounds,
                        baseline: l.baseline,
                        size: l.size,
                    })
                    .collect(),
                rect,
                font,
                align,
                drawn: None,
            }),
            why: None,
        })
    }

    /// Text that is part of a picture, under a click (the words of its field) or in a dragged box
    /// (line by line): read by Windows from the page drawn at 288 dpi, with the look of its ink.
    fn pick_drawn(&mut self, id: &str, page: u32, area: Area) -> Result<Pick> {
        let fonts = self.system_fonts().clone();
        let doc = self.doc(id)?;
        let version = doc.versions.get(page as usize).copied().unwrap_or(0);
        let p = doc.document.pages().get(page as _).map_err(pdf_error)?;
        let (pw, ph) = (p.width().value, p.height().value);
        let nothing = |why: &str| {
            Ok(Pick {
                block: None,
                why: Some(why.to_string()),
            })
        };
        let (part, spot) = match area {
            Area::Point { x, y } => (
                Rect::new(0.0, y - READ_BAND, pw, y + READ_BAND),
                Rect::around(x, y, 2.0),
            ),
            Area::Rect { rect } => (
                Rect::new(
                    rect.left - 4.0,
                    rect.bottom - 4.0,
                    rect.right + 4.0,
                    rect.top + 4.0,
                ),
                rect,
            ),
        };
        let part = Rect::new(
            part.left.max(0.0),
            part.bottom.max(0.0),
            part.right.min(pw),
            part.top.min(ph),
        );
        if part.width() < 1.0 || part.height() < 1.0 {
            return nothing(NO_TEXT);
        }
        let drawing = draw_part(&p, part, READ_SCALE)?;
        let lines = match ocr::read(&drawing.bgra, drawing.width, drawing.height) {
            Ok(l) => l,
            Err(e) => return nothing(&format!("{e:#}")),
        };
        let groups = match area {
            Area::Point { x, y } => {
                let (px, py) = drawing.to_px(x, y);
                raster::phrase_at(&lines, px, py).into_iter().collect()
            }
            Area::Rect { rect } => {
                let (l, t) = drawing.to_px(rect.left, rect.top);
                let (r, b) = drawing.to_px(rect.right, rect.bottom);
                raster::words_in(
                    &lines,
                    PxBox {
                        left: l.max(0.0) as usize,
                        top: t.max(0.0) as usize,
                        right: r.max(0.0).ceil() as usize,
                        bottom: b.max(0.0).ceil() as usize,
                    },
                )
            }
        };
        let pixels = Pixels {
            bgra: &drawing.bgra,
            width: drawing.width,
            height: drawing.height,
        };
        let read: Vec<(String, raster::Letters)> = groups
            .iter()
            .filter_map(|g| {
                let text = raster::text_of(g);
                let letters = raster::letters(&pixels, raster::around(g))?;
                (!text.trim().is_empty()).then_some((text, letters))
            })
            .collect();
        let Some((text, widest)) = read
            .iter()
            .max_by_key(|(_, l)| l.mask.width)
            .map(|(t, l)| (t.clone(), l.clone()))
        else {
            return nothing(if picture_at(&p, spot) {
                "Nook could not read any text in this part of the picture. Drag a box around the words."
            } else {
                NO_TEXT
            });
        };

        // The face, from the widest line; each line's size from how far its letters rise.
        let rise = widest.baseline - widest.bounds.top;
        let matched = fontmatch::closest(&fonts, &text, &widest.mask, rise);
        // A line is hundreds of pixels long and some tens tall: when the face is about as wide
        // as the letters, their width says their size better than their height. Otherwise the
        // new text is widened as they are. (Capitals rise about 0.72 of the size.)
        let (per_rise, stretch) = match &matched {
            Some(m) if m.stretch.ln().abs() < 0.06 => {
                (m.size_px * m.stretch / rise.max(1) as f32, 1.0)
            }
            Some(m) => (m.size_px / rise.max(1) as f32, m.stretch.clamp(0.8, 1.25)),
            None => (1.0 / 0.72, 1.0),
        };
        let lines: Vec<BlockLine> = read
            .iter()
            .map(|(text, l)| BlockLine {
                objects: Vec::new(),
                text: text.clone(),
                rect: drawing.rect_pt(l.bounds),
                baseline: drawing.to_pt(0.0, l.baseline as f32).1,
                size: (l.baseline - l.bounds.top) as f32 * per_rise / drawing.k,
            })
            .collect();
        let face = matched
            .as_ref()
            .map(|m| m.face)
            .or_else(|| fontmatch::face_of("arial"));
        let (key, family, bold, italic, kind) = face
            .map_or(("arial", "Arial", false, false, Kind::Sans), |f| {
                (f.key, f.family, f.bold, f.italic, f.kind)
            });
        let rect = lines
            .iter()
            .skip(1)
            .fold(lines[0].rect, |acc, l| acc.union(&l.rect));
        let align = if lines.iter().all(|l| layout::is_figure(&l.text)) {
            Align::Right
        } else {
            Align::Left
        };
        Ok(Pick {
            block: Some(Block {
                page,
                version,
                lines,
                rect,
                font: BlockFont {
                    name: format!(
                        "{family}{}{}",
                        if bold { " Bold" } else { "" },
                        if italic { " Italic" } else { "" }
                    ),
                    family: family.into(),
                    bold,
                    italic,
                    serif: kind == Kind::Serif,
                    mono: kind == Kind::Mono,
                    color: rgb_hex(widest.ink),
                },
                align,
                drawn: Some(Drawn {
                    paper: rgb_hex(widest.paper),
                    face: key.into(),
                    stretch,
                    from_page: false,
                }),
            }),
            why: None,
        })
    }

    fn system_fonts(&mut self) -> &SystemFonts {
        self.fonts.get_or_insert_with(SystemFonts::installed)
    }

    /// Puts the new text in, and makes sure nothing else on the page changed with it. PDFium
    /// rewrites the drawing of a page it edits, and some of what pages hold it cannot write back
    /// (colours in an ICC profile, gradients): they come out black. So the page is drawn before
    /// and after, and when more than a few pixels changed outside the edited lines the edit is
    /// taken back and made again another way: with every colour set anew as PDFium reads it (as
    /// RGB, which it can write), and failing that without rewriting the page's drawing at all,
    /// the old words covered and the new ones set over them. An edit that fails half made is
    /// taken back too.
    fn replace(
        &mut self,
        id: &str,
        block: Block,
        texts: Vec<String>,
        align: Align,
    ) -> Result<Replaced> {
        let before = self.page_drawing(id, block.page)?;
        let allowed = [edit_band(&block, &texts)];
        let tolerance = 60 + before.width * before.height / 10_000;
        let (was_dirty, snapshot) = {
            let doc = self.doc(id)?;
            (doc.dirty, doc.document.save_to_bytes().map_err(pdf_error)?)
        };
        for way in [Way::InPlace, Way::Recoloured, Way::Over] {
            let tried = match (block.drawn.is_some(), way) {
                (true, _) => self.replace_drawn(
                    id,
                    block.clone(),
                    texts.clone(),
                    align,
                    way,
                    snapshot.clone(),
                ),
                (false, Way::Over) => {
                    self.replace_over(id, &block, &texts, align, &before, snapshot.clone())
                }
                (false, _) => self.replace_text(
                    id,
                    block.clone(),
                    texts.clone(),
                    align,
                    way,
                    snapshot.clone(),
                ),
            };
            let done = match tried {
                Ok(done) => done,
                Err(e) => {
                    self.reload(id, snapshot)?;
                    return Err(e);
                }
            };
            let after = self.page_drawing(id, block.page)?;
            let changed = changed_outside(&before, &after, &allowed);
            if changed <= tolerance {
                return Ok(done);
            }
            tracing::warn!(
                "Changing text on page {} altered {changed} pixels elsewhere on it ({way:?}); taking it back",
                block.page + 1
            );
            self.take_back(id, block.page, was_dirty)?;
        }
        bail!("Nook could not change that text without spoiling the rest of the page, so the page is as it was.")
    }

    /// The page drawn small (1.5 pixels a point), to hold an edit's result against.
    fn page_drawing(&mut self, id: &str, page: u32) -> Result<Drawing> {
        let doc = self.doc(id)?;
        let p = doc.document.pages().get(page as _).map_err(pdf_error)?;
        let (pw, ph) = (p.width().value, p.height().value);
        draw_part(&p, Rect::new(0.0, 0.0, pw, ph), 1.5)
    }

    /// The document as it was (`bytes`), its fonts to be loaded again.
    fn reload(&mut self, id: &str, bytes: Vec<u8>) -> Result<()> {
        let pdfium = self.pdfium;
        let doc = self.doc(id)?;
        doc.document = pdfium
            .load_pdf_from_byte_vec(bytes, None)
            .map_err(pdf_error)?;
        doc.loaded.clear();
        doc.known = None;
        Ok(())
    }

    /// Takes back the edit just made to `page`, which spoiled it: the document as it was, and its
    /// count of edits, the page's version and whether it was saved, so the same pick can be put
    /// in another way.
    fn take_back(&mut self, id: &str, page: u32, was_dirty: bool) -> Result<()> {
        let bytes = self
            .doc(id)?
            .undo
            .pop()
            .ok_or_else(|| anyhow!("The edit could not be taken back."))?;
        self.reload(id, bytes)?;
        let doc = self.doc(id)?;
        doc.edits = doc.edits.saturating_sub(1);
        doc.dirty = was_dirty;
        if let Some(v) = doc.versions.get_mut(page as usize) {
            *v = v.saturating_sub(1);
        }
        Ok(())
    }

    /// The edit in the page's own text objects (see [`Engine::replace`]); `snapshot`, the
    /// document before it, is kept for Undo.
    fn replace_text(
        &mut self,
        id: &str,
        block: Block,
        texts: Vec<String>,
        align: Align,
        way: Way,
        snapshot: Vec<u8>,
    ) -> Result<Replaced> {
        let candidates_for = {
            let fonts = self.system_fonts().clone();
            move |t: &FontTraits| fonts.candidates(t)
        };
        let doc = self.doc(id)?;
        let page_index = block.page as usize;
        if doc.versions.get(page_index).copied() != Some(block.version) {
            bail!("The page changed since that text was picked. Select it again.");
        }
        // Read before the page is opened for the edit, so no second copy of it is open meanwhile.
        let known = doc.known().clone();
        let mut page = doc
            .document
            .pages()
            .get(block.page as _)
            .map_err(pdf_error)?;
        page.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
        let before = runs(&page);
        let page_width = page.width().value;
        let mut remove: Vec<usize> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        // For each object added at the end of the page, the original object it belongs before,
        // so the page reads in order again once they are moved there.
        let mut anchors: Vec<usize> = Vec::new();
        // The last line: the object holding its text, its baseline, and its original object.
        let mut last: Option<(usize, f32, usize)> = None;
        let mut note = |n: String| {
            if !notes.contains(&n) {
                notes.push(n);
            }
        };

        for (i, line) in block.lines.iter().enumerate() {
            let text = texts
                .get(i)
                .map(|t| t.trim_end().to_string())
                .unwrap_or_default();
            if text.trim().is_empty() {
                remove.extend(&line.objects);
                continue;
            }
            // The line's first object in the style most of its letters are in carries the text.
            let own: Vec<Run> = before
                .iter()
                .filter(|r| line.objects.contains(&r.index))
                .cloned()
                .collect();
            let main = layout::lines(own.clone())
                .first()
                .and_then(|l| layout::main_style(l).map(str::to_string));
            let mut own = own;
            own.sort_by(|a, b| {
                a.bounds
                    .left
                    .max(a.origin_x)
                    .total_cmp(&b.bounds.left.max(b.origin_x))
            });
            let keep = own
                .iter()
                .find(|r| !r.text.trim().is_empty() && main.as_deref().is_none_or(|m| r.style == m))
                .map(|r| r.index)
                .or_else(|| line.objects.first().copied())
                .ok_or_else(|| anyhow!("The picked line is empty."))?;
            let old = before
                .iter()
                .filter(|r| line.objects.contains(&r.index) && r.bounds.area() > 0.0)
                .fold(None::<Rect>, |acc, r| {
                    Some(acc.map_or(r.bounds, |a| a.union(&r.bounds)))
                })
                .unwrap_or(line.rect);
            let (index, said) = set_line(doc, &mut page, keep, &text, &known, &candidates_for)?;
            if let Some(n) = said {
                note(n);
            }
            if index != keep {
                remove.push(keep);
                anchors.push(keep);
            }
            remove.extend(line.objects.iter().copied().filter(|&o| o != keep));
            // Keep the chosen edge, and move what follows on the line by the change in width.
            let now = object_rect(&page, index)?;
            let dx = match align {
                Align::Left => 0.0,
                Align::Right => old.right - now.right,
                Align::Center => (old.left + old.right - now.left - now.right) / 2.0,
            };
            if dx.abs() > 0.01 {
                translate(&mut page, index, dx)?;
            }
            if now.right + dx > page_width + 1.0 || now.left + dx < -1.0 {
                note("The changed text runs past the edge of the page: make it shorter, or split it into two lines.".into());
            }
            if align == Align::Left {
                let grow = now.right - old.right;
                if grow.abs() > 0.01 {
                    for r in before.iter().filter(|r| {
                        !line.objects.contains(&r.index)
                            && (r.baseline - line.baseline).abs() <= line.size * 0.3
                            && r.bounds.left.max(r.origin_x) >= old.right - 0.5
                    }) {
                        translate(&mut page, r.index, grow)?;
                    }
                }
            }
            last = Some((index, line.baseline, keep));
        }

        // Lines beyond those picked go below the last, a line apart, in its font.
        if texts.len() > block.lines.len() {
            let (template, baseline, last_keep) =
                last.ok_or_else(|| anyhow!("There is no line to follow."))?;
            let step = match block.lines.len() {
                0 | 1 => block.lines.first().map_or(12.0, |l| l.size * 1.2),
                n => block.lines[n - 2].baseline - block.lines[n - 1].baseline,
            };
            let mut y = baseline;
            for text in texts[block.lines.len()..]
                .iter()
                .map(|t| t.trim_end())
                .filter(|t| !t.trim().is_empty())
            {
                y -= step;
                let (_, said) = add_line_like(doc, &mut page, template, text, y, &candidates_for)?;
                anchors.push(last_keep + 1);
                if let Some(n) = said {
                    note(n);
                }
            }
        }

        remove.sort_unstable();
        remove.dedup();
        for &index in remove.iter().rev() {
            page.objects_mut()
                .remove_object_at_index(index as _)
                .map_err(pdf_error)?;
        }
        // The added objects, from the end of the page to where the text they replace was.
        let mut added = Vec::new();
        for _ in 0..anchors.len() {
            let tail = page.objects().len() - 1;
            added.push(
                page.objects_mut()
                    .remove_object_at_index(tail)
                    .map_err(pdf_error)?,
            );
        }
        added.reverse();
        let mut placed: Vec<(usize, usize, PdfPageObject)> = anchors
            .iter()
            .enumerate()
            .zip(added)
            .map(|((seq, &a), object)| (a - remove.iter().filter(|&&r| r < a).count(), seq, object))
            .collect();
        placed.sort_by_key(|(at, seq, _)| (*at, *seq));
        for (k, (at, _, object)) in placed.into_iter().enumerate() {
            page.objects_mut()
                .insert_object_at_index(at + k, object)
                .map_err(pdf_error)?;
        }
        if way == Way::Recoloured {
            recolour(&page);
        }
        page.regenerate_content().map_err(pdf_error)?;
        drop(page);

        doc.undo.push(snapshot);
        if doc.undo.len() > UNDO_DEPTH {
            doc.undo.remove(0);
        }
        doc.edits += 1;
        doc.dirty = true;
        if let Some(v) = doc.versions.get_mut(page_index) {
            *v += 1;
        }
        Ok(Replaced {
            doc: doc.info(),
            note: (!notes.is_empty()).then(|| notes.join(" ")),
        })
    }

    /// New text for text that is part of a picture: the old letters are covered with the paper's
    /// colour, and each line is set over them as text in the matched face, at their size, colour
    /// and width, kept to the chosen edge. A scan's invisible text layer under them goes too,
    /// unless the page's drawing cannot be rewritten (`Way::Over`), when it stays.
    fn replace_drawn(
        &mut self,
        id: &str,
        block: Block,
        texts: Vec<String>,
        align: Align,
        way: Way,
        snapshot: Vec<u8>,
    ) -> Result<Replaced> {
        let drawn = block
            .drawn
            .clone()
            .ok_or_else(|| anyhow!("That text is not part of a picture."))?;
        if block.lines.is_empty() {
            bail!("The picked text is empty.");
        }
        let fonts = self.system_fonts().clone();
        let doc = self.doc(id)?;
        let page_index = block.page as usize;
        if doc.versions.get(page_index).copied() != Some(block.version) {
            bail!("The page changed since that text was picked. Select it again.");
        }

        // The face the letters matched, then others like it.
        let traits = FontTraits {
            name: block.font.name.clone(),
            family: block.font.family.clone(),
            bold: block.font.bold,
            italic: block.font.italic,
            serif: block.font.serif,
            mono: block.font.mono,
        };
        let mut faces: Vec<(PathBuf, String)> = fonts.file(&drawn.face).into_iter().collect();
        for c in fonts.candidates(&traits) {
            if !faces.iter().any(|(p, _)| *p == c.0) {
                faces.push(c);
            }
        }
        let wanted = faces.first().map(|(_, n)| n.clone());
        let (paper, ink) = (color_of(&drawn.paper), color_of(&block.font.color));
        // The page's own letters, read before the page is opened for the edit, and the face that
        // lays them out.
        let lettering = if drawn.from_page {
            let cuts = page_cuts(doc, block.page)?;
            let layout = faces
                .first()
                .and_then(|(p, _)| std::fs::read(p).ok())
                .ok_or_else(|| anyhow!("No font on this computer to lay the letters out with."))?;
            Some((cuts, layout))
        } else {
            None
        };
        let mut missing: Vec<char> = Vec::new();
        let mut page = doc
            .document
            .pages()
            .get(block.page as _)
            .map_err(pdf_error)?;
        page.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
        let page_width = page.width().value;
        let mut notes: Vec<String> = Vec::new();
        let mut note = |n: String| {
            if !notes.contains(&n) {
                notes.push(n);
            }
        };

        // Over each old line, a little past its soft edges.
        let covers: Vec<Rect> = block
            .lines
            .iter()
            .map(|l| {
                let pad = (l.size * 0.08).clamp(0.4, 1.5);
                Rect::new(
                    l.rect.left - pad,
                    l.rect.bottom - pad,
                    l.rect.right + pad,
                    l.rect.top + pad,
                )
            })
            .collect();
        let hidden: Vec<usize> = page
            .objects()
            .iter()
            .enumerate()
            .filter_map(|(i, o)| {
                let t = o.as_text_object()?;
                if !matches!(t.render_mode(), PdfPageTextRenderMode::Invisible) {
                    return None;
                }
                let b = o.bounds().ok()?.to_rect();
                let (x, y) = (
                    (b.left().value + b.right().value) / 2.0,
                    (b.bottom().value + b.top().value) / 2.0,
                );
                covers.iter().any(|c| c.contains(x, y)).then_some(i)
            })
            .collect();

        let mut set = |doc: &mut Doc,
                       page: &mut PdfPage,
                       text: &str,
                       like: &BlockLine,
                       baseline: f32|
         -> Result<()> {
            if let Some((cuts, layout)) = &lettering {
                let font = ab_glyph::FontRef::try_from_slice_and_index(layout, 0)
                    .map_err(|_| anyhow!("The font to lay the letters out with did not load."))?;
                let made = compose(
                    &Lettering {
                        font: &font,
                        size: like.size,
                        stretch: drawn.stretch,
                        cuts,
                        ink: [ink.red(), ink.green(), ink.blue()],
                    },
                    text,
                )
                .ok_or_else(|| anyhow!("There is nothing to draw in that text."))?;
                let old = like.rect;
                let origin = match align {
                    Align::Left => old.left - made.ink_left,
                    Align::Right => old.right - made.ink_right,
                    Align::Center => (old.left + old.right - made.ink_left - made.ink_right) / 2.0,
                };
                let mut picture = PdfPageImageObject::new_with_size(
                    &doc.document,
                    &image::DynamicImage::ImageRgba8(made.image),
                    PdfPoints::new(made.width),
                    PdfPoints::new(made.height),
                )
                .map_err(pdf_error)?;
                picture
                    .translate(
                        PdfPoints::new(origin + made.left),
                        PdfPoints::new(baseline + made.bottom),
                    )
                    .map_err(pdf_error)?;
                page.objects_mut()
                    .add_image_object(picture)
                    .map_err(pdf_error)?;
                // The words beneath, invisible, so the page still finds and copies them.
                let under = Placed {
                    size: like.size,
                    stretch: drawn.stretch,
                    x: origin,
                    baseline,
                    color: ink,
                };
                if let Ok((index, _)) = add_text(doc, page, text, &faces, under) {
                    let mut object = page.objects().get(index as _).map_err(pdf_error)?;
                    if let Some(t) = object.as_text_object_mut() {
                        t.set_render_mode(PdfPageTextRenderMode::Invisible)
                            .map_err(pdf_error)?;
                    }
                }
                missing.extend(made.missing);
                if origin + made.ink_right > page_width + 1.0 || origin + made.ink_left < -1.0 {
                    note("The changed text runs past the edge of the page: make it shorter, or split it into two lines.".into());
                }
                return Ok(());
            }
            let (index, used) = add_text(
                doc,
                page,
                text,
                &faces,
                Placed {
                    size: like.size,
                    stretch: drawn.stretch,
                    x: like.rect.left,
                    baseline,
                    color: ink,
                },
            )?;
            if Some(&used) != wanted.as_ref() {
                note(format!(
                    "{} is not on this computer, so the text is set in {used}.",
                    block.font.name
                ));
            }
            let now = object_rect(page, index)?;
            let old = like.rect;
            let dx = match align {
                Align::Left => old.left - now.left,
                Align::Right => old.right - now.right,
                Align::Center => (old.left + old.right - now.left - now.right) / 2.0,
            };
            if dx.abs() > 0.01 {
                translate(page, index, dx)?;
            }
            if now.right + dx > page_width + 1.0 || now.left + dx < -1.0 {
                note("The changed text runs past the edge of the page: make it shorter, or split it into two lines.".into());
            }
            Ok(())
        };
        for (i, (line, cover)) in block.lines.iter().zip(&covers).enumerate() {
            add_cover(doc, &mut page, *cover, paper)?;
            let text = texts
                .get(i)
                .map(|t| t.trim_end().to_string())
                .unwrap_or_default();
            if !text.trim().is_empty() {
                set(doc, &mut page, &text, line, line.baseline)?;
            }
        }
        // Lines beyond those picked go below the last, a line apart.
        let n = block.lines.len();
        let last = &block.lines[n - 1];
        let step = match n {
            1 => last.size * 1.2,
            _ => block.lines[n - 2].baseline - last.baseline,
        };
        let mut y = last.baseline;
        for text in texts
            .iter()
            .skip(n)
            .map(|t| t.trim_end())
            .filter(|t| !t.trim().is_empty())
        {
            y -= step;
            set(doc, &mut page, text, last, y)?;
        }
        if lettering.is_some() {
            missing.sort_unstable();
            missing.dedup();
            let list: Vec<String> = missing.iter().map(char::to_string).collect();
            note(match list.len() {
                0 => "Set in letters cut from this page.".to_string(),
                1 => format!(
                    "Set in letters cut from this page. It has no {}, so that one is drawn in {}.",
                    list[0], block.font.name
                ),
                _ => format!(
                    "Set in letters cut from this page. It has no {}, so those are drawn in {}.",
                    list.join(", "),
                    block.font.name
                ),
            });
        }
        if way != Way::Over {
            for &i in hidden.iter().rev() {
                page.objects_mut()
                    .remove_object_at_index(i as _)
                    .map_err(pdf_error)?;
            }
        }
        if way == Way::Recoloured {
            recolour(&page);
        }
        page.regenerate_content().map_err(pdf_error)?;
        drop(page);

        doc.undo.push(snapshot);
        if doc.undo.len() > UNDO_DEPTH {
            doc.undo.remove(0);
        }
        doc.edits += 1;
        doc.dirty = true;
        if let Some(v) = doc.versions.get_mut(page_index) {
            *v += 1;
        }
        Ok(Replaced {
            doc: doc.info(),
            note: (!notes.is_empty()).then(|| notes.join(" ")),
        })
    }

    /// The edit made without rewriting the page's own drawing, which would spoil it: each old
    /// line is covered with the colour around it (read from `before`, the page as it was) and the
    /// new text set over it, in the line's face from Windows at its size, colour and place, kept
    /// to the chosen edge. The old words stay in the file, beneath.
    fn replace_over(
        &mut self,
        id: &str,
        block: &Block,
        texts: &[String],
        align: Align,
        before: &Drawing,
        snapshot: Vec<u8>,
    ) -> Result<Replaced> {
        let candidates_for = {
            let fonts = self.system_fonts().clone();
            move |t: &FontTraits| fonts.candidates(t)
        };
        let doc = self.doc(id)?;
        let page_index = block.page as usize;
        if doc.versions.get(page_index).copied() != Some(block.version) {
            bail!("The page changed since that text was picked. Select it again.");
        }
        let mut page = doc
            .document
            .pages()
            .get(block.page as _)
            .map_err(pdf_error)?;
        page.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
        let page_width = page.width().value;
        let mut notes = vec![
            "This page's drawing does not survive being rewritten, so the old words are covered and the new ones set over them; the old ones are still in the file, beneath."
                .to_string(),
        ];
        let mut note = |n: String| {
            if !notes.contains(&n) {
                notes.push(n);
            }
        };
        let mut put = |doc: &mut Doc,
                       page: &mut PdfPage,
                       template: usize,
                       text: &str,
                       old: Rect,
                       baseline: f32|
         -> Result<()> {
            let (index, _) = add_line_like(doc, page, template, text, baseline, &candidates_for)?;
            let now = object_rect(page, index)?;
            let dx = match align {
                Align::Left => old.left - now.left,
                Align::Right => old.right - now.right,
                Align::Center => (old.left + old.right - now.left - now.right) / 2.0,
            };
            if dx.abs() > 0.01 {
                translate(page, index, dx)?;
            }
            if now.right + dx > page_width + 1.0 || now.left + dx < -1.0 {
                note("The changed text runs past the edge of the page: make it shorter, or split it into two lines.".into());
            }
            Ok(())
        };
        for (i, line) in block.lines.iter().enumerate() {
            let pad = (line.size * 0.08).clamp(0.4, 1.5);
            let cover = Rect::new(
                line.rect.left - pad,
                line.rect.bottom - pad,
                line.rect.right + pad,
                line.rect.top + pad,
            );
            add_cover(doc, &mut page, cover, paper_around(before, cover))?;
            let text = texts
                .get(i)
                .map(|t| t.trim_end().to_string())
                .unwrap_or_default();
            if text.trim().is_empty() {
                continue;
            }
            let template = *line
                .objects
                .first()
                .ok_or_else(|| anyhow!("The picked line is empty."))?;
            put(doc, &mut page, template, &text, line.rect, line.baseline)?;
        }
        // Lines beyond those picked go below the last, a line apart.
        let n = block.lines.len();
        if let (Some(last), true) = (block.lines.last(), texts.len() > n) {
            let template = *last
                .objects
                .first()
                .ok_or_else(|| anyhow!("The picked line is empty."))?;
            let step = match n {
                1 => last.size * 1.2,
                _ => block.lines[n - 2].baseline - last.baseline,
            };
            let mut y = last.baseline;
            for text in texts
                .iter()
                .skip(n)
                .map(|t| t.trim_end())
                .filter(|t| !t.trim().is_empty())
            {
                y -= step;
                put(doc, &mut page, template, text, last.rect, y)?;
            }
        }
        page.regenerate_content().map_err(pdf_error)?;
        drop(page);

        doc.undo.push(snapshot);
        if doc.undo.len() > UNDO_DEPTH {
            doc.undo.remove(0);
        }
        doc.edits += 1;
        doc.dirty = true;
        if let Some(v) = doc.versions.get_mut(page_index) {
            *v += 1;
        }
        Ok(Replaced {
            doc: doc.info(),
            note: Some(notes.join(" ")),
        })
    }

    fn undo(&mut self, id: &str) -> Result<PdfDoc> {
        let pdfium = self.pdfium;
        let doc = self.doc(id)?;
        let Some(bytes) = doc.undo.pop() else {
            bail!("There is nothing to undo.");
        };
        doc.document = pdfium
            .load_pdf_from_byte_vec(bytes, None)
            .map_err(pdf_error)?;
        // Fonts loaded into the document went with it.
        doc.loaded.clear();
        doc.known = None;
        doc.edits = doc.edits.saturating_sub(1);
        doc.dirty = true;
        for v in doc.versions.iter_mut() {
            *v += 1;
        }
        Ok(doc.info())
    }

    fn text_lines(&mut self, path: &Path) -> Result<Vec<PageText>> {
        let document = self
            .pdfium
            .load_pdf_from_file(path, None)
            .map_err(pdf_error)?;
        let mut pages = Vec::new();
        for page in document.pages().iter() {
            let mut lines = Vec::new();
            {
                let text = page.text().map_err(pdf_error)?;
                let mut line = LineSoFar::default();
                for ch in text.chars().iter() {
                    let Some(c) = ch.unicode_char() else { continue };
                    if c == '\n' || c == '\r' {
                        line.end(&mut lines);
                        continue;
                    }
                    line.text.push(c);
                    if c.is_whitespace() {
                        continue;
                    }
                    line.chars += 1;
                    line.size = line.size.max(ch.scaled_font_size().value);
                    let weight = match ch.font_weight() {
                        Some(PdfFontWeight::Weight600)
                        | Some(PdfFontWeight::Weight700Bold)
                        | Some(PdfFontWeight::Weight800)
                        | Some(PdfFontWeight::Weight900) => true,
                        Some(PdfFontWeight::Custom(w)) => w >= 600,
                        _ => {
                            let name = ch.font_name().to_lowercase();
                            name.contains("bold")
                                || name.contains("black")
                                || name.contains("heavy")
                        }
                    };
                    line.bold += weight as usize;
                    if let Ok(b) = ch.loose_bounds() {
                        line.left = line.left.min(b.left().value);
                        line.right = line.right.max(b.right().value);
                        line.top = line.top.max(b.top().value);
                        line.bottom = line.bottom.min(b.bottom().value);
                    }
                }
                line.end(&mut lines);
            }
            if lines.is_empty() {
                lines = read_page(&page);
            }
            pages.push(PageText { lines });
        }
        Ok(pages)
    }

    fn page_pictures(
        &mut self,
        path: &Path,
        to: &str,
        dpi: f32,
        one: &Path,
        dir: &Path,
    ) -> Result<Vec<PathBuf>> {
        let document = self
            .pdfium
            .load_pdf_from_file(path, None)
            .map_err(pdf_error)?;
        let count = document.pages().len() as usize;
        let digits = count.to_string().len();
        let name = one
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "page".into());
        if count > 1 {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Could not create {}", dir.display()))?;
        }
        let mut written = Vec::new();
        for (i, page) in document.pages().iter().enumerate() {
            let width = ((page.width().value * dpi / 72.0).round() as i32).clamp(16, 8000);
            let config = PdfRenderConfig::new()
                .set_target_width(width)
                .render_form_data(true)
                .render_annotations(true);
            let picture = page
                .render_with_config(&config)
                .map_err(pdf_error)?
                .as_image()
                .map_err(pdf_error)?;
            let target = if count == 1 {
                one.to_path_buf()
            } else {
                dir.join(format!("{name} {:0digits$}.{to}", i + 1))
            };
            images::save(&picture, to, &target)?;
            written.push(target);
        }
        Ok(written)
    }

    fn pictures_pdf(&mut self, pictures: &[PathBuf], out: &Path, work: &Path) -> Result<()> {
        let mut document = self.pdfium.create_new_pdf().map_err(pdf_error)?;
        for (i, file) in pictures.iter().enumerate() {
            let (picture, turned) = images::open_upright(file)?;
            let (w, h) = (picture.width() as f32, picture.height() as f32);
            let (pw, ph) = if w > h {
                (842.0, 595.0)
            } else {
                (595.0, 842.0)
            };
            let margin = 24.0;
            let s = ((pw - 2.0 * margin) / w).min((ph - 2.0 * margin) / h);
            let (iw, ih) = (w * s, h * s);
            let (x, y) = ((pw - iw) / 2.0, (ph - ih) / 2.0);
            let mut page = document
                .pages_mut()
                .create_page_at_end(PdfPagePaperSize::new_custom(
                    PdfPoints::new(pw),
                    PdfPoints::new(ph),
                ))
                .map_err(pdf_error)?;
            let see_through =
                picture.color().has_alpha() && picture.to_rgba8().pixels().any(|p| p.0[3] < 255);
            let mut object = if see_through {
                PdfPageImageObject::new_with_size(
                    &document,
                    &picture,
                    PdfPoints::new(iw),
                    PdfPoints::new(ih),
                )
                .map_err(pdf_error)?
            } else {
                // A JPEG goes in as it is, compressed; any other picture is made one first.
                let is_jpeg = crate::convert::formats::of_path(file).is_some_and(|f| f.id == "jpg");
                let jpeg = if is_jpeg && !turned {
                    file.clone()
                } else {
                    let made = work.join(format!("picture-{i}.jpg"));
                    images::save(&picture, "jpg", &made)?;
                    made
                };
                let mut o =
                    PdfPageImageObject::new_from_jpeg_file(&document, &jpeg).map_err(pdf_error)?;
                o.apply_matrix(PdfMatrix::new(iw, 0.0, 0.0, ih, 0.0, 0.0))
                    .map_err(pdf_error)?;
                o
            };
            object
                .translate(PdfPoints::new(x), PdfPoints::new(y))
                .map_err(pdf_error)?;
            page.objects_mut()
                .add_image_object(object)
                .map_err(pdf_error)?;
        }
        document.save_to_file(out).map_err(pdf_error)?;
        Ok(())
    }

    fn save(&mut self, id: &str, to: Option<PathBuf>) -> Result<PdfDoc> {
        let doc = self.doc(id)?;
        let target = match to {
            Some(t) => t,
            None => edited_path(&doc.path),
        };
        doc.document.save_to_file(&target).map_err(pdf_error)?;
        doc.saved_to = Some(target);
        doc.dirty = false;
        Ok(doc.info())
    }
}

/// "<name> (edited).pdf" beside the original, or "(edited 2)" and on when that exists.
pub fn edited_path(original: &Path) -> PathBuf {
    let stem = original
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".into());
    let dir = original.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut n = 1;
    loop {
        let name = if n == 1 {
            format!("{stem} (edited).pdf")
        } else {
            format!("{stem} (edited {n}).pdf")
        };
        let p = dir.join(name);
        if !p.exists() {
            return p;
        }
        n += 1;
    }
}

/// A part of a page drawn for reading: BGRA, rows from the top, `k` pixels a point; the page's
/// point (x, y) is at pixel (x·k − x0, (page height − y)·k − y0).
struct Drawing {
    bgra: Vec<u8>,
    width: usize,
    height: usize,
    x0: f32,
    y0: f32,
    k: f32,
    page_height: f32,
}

impl Drawing {
    fn to_px(&self, x: f32, y: f32) -> (f32, f32) {
        (
            x * self.k - self.x0,
            (self.page_height - y) * self.k - self.y0,
        )
    }

    fn to_pt(&self, x: f32, y: f32) -> (f32, f32) {
        (
            (x + self.x0) / self.k,
            self.page_height - (y + self.y0) / self.k,
        )
    }

    fn rect_pt(&self, b: PxBox) -> Rect {
        let (left, top) = self.to_pt(b.left as f32, b.top as f32);
        let (right, bottom) = self.to_pt(b.right as f32, b.bottom as f32);
        Rect::new(left, bottom, right, top)
    }
}

/// `part` of the page (points) drawn at `scale` pixels a point, less when that would be wider or
/// taller than 9,000 pixels; forms filled in are drawn too.
fn draw_part(page: &PdfPage, part: Rect, scale: f32) -> Result<Drawing> {
    let (pw, ph) = (page.width().value, page.height().value);
    let scale = scale.min(9000.0 / part.width().max(part.height()).max(1.0));
    let page_px = (pw * scale).round().max(1.0);
    let k = page_px / pw;
    let x0 = (part.left * k).floor();
    let y0 = ((ph - part.top) * k).floor();
    let width = ((part.right * k).ceil() - x0).max(1.0) as usize;
    let height = (((ph - part.bottom) * k).ceil() - y0).max(1.0) as usize;
    let mut bitmap =
        PdfBitmap::empty(width as i32, height as i32, PdfBitmapFormat::BGRA).map_err(pdf_error)?;
    let config = PdfRenderConfig::new()
        .set_target_width(page_px as i32)
        .set_origin(-(x0 as i32), -(y0 as i32))
        // BGRA as it says, for Windows' text recognition (pdfium-render asks for RGBA).
        .set_reverse_byte_order(false)
        .render_form_data(true)
        .render_annotations(true);
    page.render_into_bitmap_with_config(&mut bitmap, &config)
        .map_err(pdf_error)?;
    Ok(Drawing {
        bgra: bitmap.as_raw_bytes(),
        width,
        height,
        x0,
        y0,
        k,
        page_height: ph,
    })
}

/// A line of a page's text as it is read out, character by character.
struct LineSoFar {
    text: String,
    chars: usize,
    bold: usize,
    size: f32,
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
}

impl Default for LineSoFar {
    fn default() -> Self {
        LineSoFar {
            text: String::new(),
            chars: 0,
            bold: 0,
            size: 0.0,
            left: f32::MAX,
            right: f32::MIN,
            top: f32::MIN,
            bottom: f32::MAX,
        }
    }
}

impl LineSoFar {
    /// The line done: kept when it has text (bold when most of its letters are).
    fn end(&mut self, lines: &mut Vec<TextLine>) {
        let done = std::mem::take(self);
        if done.chars == 0 {
            return;
        }
        let text = done.text.trim().to_string();
        let (top, bottom) = if done.top >= done.bottom {
            (done.top, done.bottom)
        } else {
            (done.size, 0.0)
        };
        lines.push(TextLine {
            text,
            size: done.size,
            bold: done.bold * 5 > done.chars * 3,
            left: if done.left == f32::MAX {
                0.0
            } else {
                done.left
            },
            right: done.right.max(done.left.min(0.0)),
            top,
            bottom,
        });
    }
}

/// A page with no text (a scan) read by Windows' text recognition: its lines, their size from
/// their height. None when Windows cannot read here.
fn read_page(page: &PdfPage) -> Vec<TextLine> {
    let (pw, ph) = (page.width().value, page.height().value);
    let Ok(drawing) = draw_part(page, Rect::new(0.0, 0.0, pw, ph), 3.0) else {
        return Vec::new();
    };
    let Ok(lines) = ocr::read(&drawing.bgra, drawing.width, drawing.height) else {
        return Vec::new();
    };
    lines
        .iter()
        .map(|words| {
            let b = raster::around(words);
            let (left, top) = drawing.to_pt(b.left as f32, b.top as f32);
            let (right, bottom) = drawing.to_pt(b.right as f32, b.bottom as f32);
            TextLine {
                text: raster::text_of(words),
                size: (top - bottom) / 1.2,
                bold: false,
                left,
                right,
                top,
                bottom,
            }
        })
        .collect()
}

/// Letters cut from a page, drawn at `k` pixels a point.
struct PageCuts {
    k: f32,
    cuts: Vec<(char, raster::Cut)>,
}

/// The letters a page has to give: the whole page drawn at 288 dpi and read by Windows, each
/// word it reads cut into its letters. Read once and kept with the document.
fn page_cuts(doc: &mut Doc, page: u32) -> Result<Arc<PageCuts>> {
    if let Some(c) = doc.cuts.get(&page) {
        return Ok(c.clone());
    }
    let drawing = {
        let p = doc.document.pages().get(page as _).map_err(pdf_error)?;
        let (pw, ph) = (p.width().value, p.height().value);
        draw_part(&p, Rect::new(0.0, 0.0, pw, ph), READ_SCALE)?
    };
    let lines = ocr::read(&drawing.bgra, drawing.width, drawing.height)?;
    let pixels = Pixels {
        bgra: &drawing.bgra,
        width: drawing.width,
        height: drawing.height,
    };
    let mut cuts = Vec::new();
    for line in &lines {
        for phrase in raster::phrases(line) {
            if let Some(l) = raster::letters(&pixels, raster::around(&phrase)) {
                cuts.extend(raster::cut_letters(&pixels, &l, &phrase));
            }
        }
    }
    let got = Arc::new(PageCuts { k: drawing.k, cuts });
    doc.cuts.insert(page, got.clone());
    Ok(got)
}

/// What a line of the page's own letters is made with: the matched face, which lays it out (its
/// advances and kerning, `stretch` times as wide) at `size` points and draws the letters the page
/// does not have; the page's letters; and the ink's colour.
struct Lettering<'a> {
    font: &'a ab_glyph::FontRef<'a>,
    size: f32,
    stretch: f32,
    cuts: &'a PageCuts,
    ink: [u8; 3],
}

/// A line made: its picture, and in points from the text's origin on the baseline, the picture's
/// left and bottom edges, its size, and where its ink starts and ends; and the letters the page
/// did not have.
struct Composed {
    image: image::RgbaImage,
    left: f32,
    bottom: f32,
    width: f32,
    height: f32,
    ink_left: f32,
    ink_right: f32,
    missing: Vec<char>,
}

/// `text` out of the page's letters: each where the face would put its own, centred on it and
/// standing on the baseline. Of the page's copies of a letter, those in the same ink, within an
/// eighth of the face's height (resized when off by more than a twentieth) and with the face's
/// shape (nearly half their ink in common, placed where it would go), the most like it. A letter
/// the page has no such copy of is the face's own.
fn compose(l: &Lettering, text: &str) -> Option<Composed> {
    use ab_glyph::{Font, PxScale, ScaleFont};
    enum Mark<'c> {
        Cut {
            cut: &'c raster::Cut,
            s: f32,
            left: i32,
            top: i32,
        },
        Glyph(ab_glyph::OutlinedGlyph),
    }
    let k = l.cuts.k;
    let em = l.size * k * fontmatch::em_ratio(l.font);
    let scale = PxScale {
        x: em * l.stretch,
        y: em,
    };
    let scaled = l.font.as_scaled(scale);
    let far = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).any(|(&x, y)| x.abs_diff(y) > 60);
    let mut marks: Vec<Mark> = Vec::new();
    let mut missing = Vec::new();
    let (mut caret, mut prev) = (0.0f32, None);
    for c in text.chars() {
        let id = scaled.glyph_id(c);
        if let Some(p) = prev {
            caret += scaled.kern(p, id);
        }
        prev = Some(id);
        let glyph = id.with_scale_and_position(scale, ab_glyph::point(caret, 0.0));
        caret += scaled.h_advance(id);
        if c.is_whitespace() {
            continue;
        }
        let Some(outline) = l.font.outline_glyph(glyph) else {
            continue;
        };
        let b = outline.px_bounds();
        let want = b.max.y - b.min.y;
        let middle = (b.min.x + b.max.x) / 2.0;
        // The face's own letter, to hold the page's copies against: one in another face (a
        // label's, a stamp's) does not look like it.
        let (gx, gy) = (b.min.x.floor() as i32, b.min.y.floor() as i32);
        let (gw, gh) = (
            (b.max.x.ceil() as i32 - gx).max(1) as usize,
            (b.max.y.ceil() as i32 - gy).max(1) as usize,
        );
        let mut face = vec![false; gw * gh];
        outline.draw(|x, y, cover| {
            let (x, y) = (x as usize, y as usize);
            if x < gw && y < gh && cover >= 0.5 {
                face[y * gw + x] = true;
            }
        });
        let like = |cut: &raster::Cut, s: f32| {
            let (left, top) = (middle - cut.width as f32 * s / 2.0, -(cut.above as f32) * s);
            let (x0, y0) = (
                left.min(gx as f32).floor() as i32,
                top.min(gy as f32).floor() as i32,
            );
            let x1 = (left + cut.width as f32 * s)
                .max((gx + gw as i32) as f32)
                .ceil() as i32;
            let y1 = (top + cut.height as f32 * s)
                .max((gy + gh as i32) as f32)
                .ceil() as i32;
            let (mut both, mut either) = (0u32, 0u32);
            for y in y0..y1 {
                for x in x0..x1 {
                    let (fx, fy) = (x - gx, y - gy);
                    let f = fx >= 0
                        && fy >= 0
                        && (fx as usize) < gw
                        && (fy as usize) < gh
                        && face[fy as usize * gw + fx as usize];
                    let p = sample(
                        cut,
                        (x as f32 + 0.5 - left) / s - 0.5,
                        (y as f32 + 0.5 - top) / s - 0.5,
                    ) >= 0.5;
                    both += (f && p) as u32;
                    either += (f || p) as u32;
                }
            }
            both as f32 / either.max(1) as f32
        };
        let best = l
            .cuts
            .cuts
            .iter()
            .filter(|(ch, cut)| *ch == c && !far(cut.ink, l.ink))
            .filter_map(|(_, cut)| {
                // Heights to an eighth, and a pixel and a half either way for small marks (a
                // hyphen is three pixels tall).
                let off = (want - cut.ink_height as f32).abs();
                if off > (want * 0.12).max(1.5) {
                    return None;
                }
                let r = want / cut.ink_height.max(1) as f32;
                let s = if off <= (want * 0.05).max(1.5) {
                    1.0
                } else {
                    r
                };
                let shape = like(cut, s);
                (shape >= 0.45).then_some((shape - (r - 1.0).abs(), s, cut))
            })
            .max_by(|a, b| a.0.total_cmp(&b.0));
        match best {
            Some((_, s, cut)) => {
                marks.push(Mark::Cut {
                    cut,
                    s,
                    left: (middle - cut.width as f32 * s / 2.0).round() as i32,
                    top: (-(cut.above as f32) * s).round() as i32,
                });
            }
            None => {
                missing.push(c);
                marks.push(Mark::Glyph(outline));
            }
        }
    }
    let size_of = |cut: &raster::Cut, s: f32| {
        (
            (cut.width as f32 * s).round() as i32,
            (cut.height as f32 * s).round() as i32,
        )
    };
    let boxes = marks.iter().map(|m| match m {
        Mark::Cut { cut, s, left, top } => {
            let (w, h) = size_of(cut, *s);
            (*left, *top, left + w, top + h)
        }
        Mark::Glyph(o) => {
            let b = o.px_bounds();
            (
                b.min.x.floor() as i32,
                b.min.y.floor() as i32,
                b.max.x.ceil() as i32,
                b.max.y.ceil() as i32,
            )
        }
    });
    let (x0, y0, x1, y1) = boxes.fold(None, |acc: Option<(i32, i32, i32, i32)>, b| {
        Some(acc.map_or(b, |a| {
            (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
        }))
    })?;
    let (x0, y0, x1, y1) = (x0 - 1, y0 - 1, x1 + 1, y1 + 1);
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut alpha = vec![0f32; w * h];
    let mut put = |x: i32, y: i32, a: f32| {
        let (x, y) = (x - x0, y - y0);
        if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
            let i = y as usize * w + x as usize;
            alpha[i] = alpha[i].max(a);
        }
    };
    for m in &marks {
        match m {
            Mark::Cut { cut, s, left, top } => {
                let (cw, ch) = size_of(cut, *s);
                for ty in 0..ch {
                    for tx in 0..cw {
                        let a = if *s == 1.0 {
                            cut.alpha[ty as usize * cut.width + tx as usize] as f32 / 255.0
                        } else {
                            sample(
                                cut,
                                (tx as f32 + 0.5) / s - 0.5,
                                (ty as f32 + 0.5) / s - 0.5,
                            )
                        };
                        put(left + tx, top + ty, a);
                    }
                }
            }
            Mark::Glyph(o) => {
                let b = o.px_bounds();
                o.draw(|gx, gy, c| put(b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32, c));
            }
        }
    }
    let inked: Vec<usize> = (0..w)
        .filter(|&x| (0..h).any(|y| alpha[y * w + x] > 0.1))
        .collect();
    let (first, last) = (*inked.first()?, *inked.last()?);
    let [r, g, b] = l.ink;
    let image = image::RgbaImage::from_fn(w as u32, h as u32, |x, y| {
        let a = alpha[y as usize * w + x as usize];
        image::Rgba([r, g, b, (a * 255.0).round() as u8])
    });
    Some(Composed {
        image,
        left: x0 as f32 / k,
        bottom: -(y1 as f32) / k,
        width: w as f32 / k,
        height: h as f32 / k,
        ink_left: (x0 + first as i32) as f32 / k,
        ink_right: (x0 + last as i32 + 1) as f32 / k,
        missing,
    })
}

/// A cut letter's ink at a point between its pixels.
fn sample(cut: &raster::Cut, x: f32, y: f32) -> f32 {
    let at = |xi: i32, yi: i32| {
        if xi < 0 || yi < 0 || xi >= cut.width as i32 || yi >= cut.height as i32 {
            0.0
        } else {
            cut.alpha[yi as usize * cut.width + xi as usize] as f32 / 255.0
        }
    };
    let (fx, fy) = (x.floor(), y.floor());
    let (dx, dy) = (x - fx, y - fy);
    let (xi, yi) = (fx as i32, fy as i32);
    at(xi, yi) * (1.0 - dx) * (1.0 - dy)
        + at(xi + 1, yi) * dx * (1.0 - dy)
        + at(xi, yi + 1) * (1.0 - dx) * dy
        + at(xi + 1, yi + 1) * dx * dy
}

/// How an edit is made (see [`Engine::replace`]): in the page's own objects; so, with every
/// colour on the page set anew as RGB; or over the page's drawing, which is left as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Way {
    InPlace,
    Recoloured,
    Over,
}

/// Where an edit may change the page: right across it, from a little above the picked lines to
/// a little below the last new one (the rest of a line moves with a longer or shorter word).
fn edit_band(block: &Block, texts: &[String]) -> Rect {
    let size = block.lines.iter().map(|l| l.size).fold(1.0, f32::max);
    let top = block
        .lines
        .iter()
        .map(|l| l.rect.top)
        .fold(f32::MIN, f32::max);
    let mut bottom = block
        .lines
        .iter()
        .map(|l| l.rect.bottom)
        .fold(f32::MAX, f32::min);
    let n = block.lines.len();
    if let (Some(last), true) = (block.lines.last(), texts.len() > n) {
        let step = match n {
            1 => last.size * 1.2,
            _ => block.lines[n - 2].baseline - last.baseline,
        };
        bottom = bottom.min(last.baseline - step * (texts.len() - n) as f32 - size * 0.3);
    }
    Rect::new(-1e5, bottom - size * 0.6, 1e5, top + size * 0.6)
}

/// Sets every path's and text's colours again as PDFium reads them (as RGB), so a page it
/// rewrites keeps colours it cannot write in their own terms (an ICC profile's) instead of
/// drawing them black.
fn recolour(page: &PdfPage) {
    for mut o in page.objects().iter() {
        if !matches!(
            o.object_type(),
            PdfPageObjectType::Path | PdfPageObjectType::Text
        ) {
            continue;
        }
        if let Ok(c) = o.fill_color() {
            let _ = o.set_fill_color(c);
        }
        if let Ok(c) = o.stroke_color() {
            let _ = o.set_stroke_color(c);
        }
    }
}

/// The page's commonest colour just around `r` (points), in a drawing of it.
fn paper_around(d: &Drawing, r: Rect) -> PdfColor {
    let (l, t) = d.to_px(r.left, r.top);
    let (rt, b) = d.to_px(r.right, r.bottom);
    let (l, t, rt, b) = (
        l.floor() as i64 - 2,
        t.floor() as i64 - 2,
        rt.ceil() as i64 + 2,
        b.ceil() as i64 + 2,
    );
    let mut seen: HashMap<[u8; 3], usize> = HashMap::new();
    let mut at = |x: i64, y: i64| {
        if x >= 0 && y >= 0 && (x as usize) < d.width && (y as usize) < d.height {
            let i = (y as usize * d.width + x as usize) * 4;
            *seen
                .entry([d.bgra[i + 2], d.bgra[i + 1], d.bgra[i]])
                .or_default() += 1;
        }
    };
    for x in l..=rt {
        at(x, t);
        at(x, b);
    }
    for y in t..=b {
        at(l, y);
        at(rt, y);
    }
    seen.into_iter()
        .max_by_key(|&(_, n)| n)
        .map_or(PdfColor::WHITE, |([r, g, b], _)| {
            PdfColor::new(r, g, b, 255)
        })
}

/// How many pixels differ between two drawings of the same page, by more than a faint shade in
/// some channel, outside `allowed` (points).
fn changed_outside(before: &Drawing, after: &Drawing, allowed: &[Rect]) -> usize {
    if (before.width, before.height) != (after.width, after.height) {
        return before.width * before.height;
    }
    let boxes: Vec<(f32, f32, f32, f32)> = allowed
        .iter()
        .map(|r| {
            let (l, t) = before.to_px(r.left, r.top);
            let (rt, b) = before.to_px(r.right, r.bottom);
            (l, t, rt, b)
        })
        .collect();
    let mut changed = 0;
    for y in 0..before.height {
        for x in 0..before.width {
            let i = (y * before.width + x) * 4;
            let differs = (0..3).any(|c| before.bgra[i + c].abs_diff(after.bgra[i + c]) > 40);
            if differs {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                if !boxes
                    .iter()
                    .any(|&(l, t, r, b)| fx >= l && fx < r && fy >= t && fy < b)
                {
                    changed += 1;
                }
            }
        }
    }
    changed
}

/// Whether a picture on the page reaches into `spot`.
fn picture_at(page: &PdfPage, spot: Rect) -> bool {
    page.objects().iter().any(|o| {
        o.object_type() == PdfPageObjectType::Image
            && o.bounds().is_ok_and(|b| {
                let b = b.to_rect();
                spot.intersection(&Rect::new(
                    b.left().value,
                    b.bottom().value,
                    b.right().value,
                    b.top().value,
                )) > 0.0
            })
    })
}

fn rgb_hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// "#1f4e79" as a colour; black when it is not one.
fn color_of(hex: &str) -> PdfColor {
    let h = hex.trim_start_matches('#');
    let part = |i: usize| h.get(i..i + 2).and_then(|s| u8::from_str_radix(s, 16).ok());
    match (h.len(), part(0), part(2), part(4)) {
        (6, Some(r), Some(g), Some(b)) => PdfColor::new(r, g, b, 255),
        _ => PdfColor::BLACK,
    }
}

/// A rectangle of `color` over `r`, no outline: it hides what is under it.
fn add_cover(doc: &Doc, page: &mut PdfPage, r: Rect, color: PdfColor) -> Result<()> {
    let path = PdfPagePathObject::new_rect(
        &doc.document,
        PdfRect::new_from_values(r.bottom, r.left, r.top, r.right),
        None,
        None,
        Some(color),
    )
    .map_err(pdf_error)?;
    page.objects_mut()
        .add_path_object(path)
        .map_err(pdf_error)?;
    Ok(())
}

/// Where and how a new line of text is set: `size` points, `stretch` times as wide, its origin
/// at (`x`, `baseline`), in `color`.
struct Placed {
    size: f32,
    stretch: f32,
    x: f32,
    baseline: f32,
    color: PdfColor,
}

/// Adds `text` in the first of `faces` that writes it. Returns the new object's index (the last
/// on the page) and the face's name.
fn add_text(
    doc: &mut Doc,
    page: &mut PdfPage,
    text: &str,
    faces: &[(PathBuf, String)],
    at: Placed,
) -> Result<(usize, String)> {
    for (path, used) in faces {
        let Some(token) = font_token(doc, path) else {
            continue;
        };
        let mut object =
            PdfPageTextObject::new(&doc.document, text, token, PdfPoints::new(at.size))
                .map_err(pdf_error)?;
        object
            .apply_matrix(PdfMatrix::new(at.stretch, 0.0, 0.0, 1.0, at.x, at.baseline))
            .map_err(pdf_error)?;
        object.set_fill_color(at.color).map_err(pdf_error)?;
        let added = page
            .objects_mut()
            .add_text_object(object)
            .map_err(pdf_error)?;
        let index = page.objects().len() - 1;
        if added
            .as_text_object()
            .is_some_and(|t| came_through(text, &t.text()))
        {
            return Ok((index, used.clone()));
        }
        page.objects_mut()
            .remove_object_at_index(index as _)
            .map_err(pdf_error)?;
    }
    bail!("No font on this computer writes that text.")
}

/// The installed font at `path` in this document, loaded on first use.
fn font_token(doc: &mut Doc, path: &Path) -> Option<PdfFontToken> {
    if let Some(t) = doc.loaded.get(path) {
        return Some(*t);
    }
    let bytes = std::fs::read(path).ok()?;
    match doc
        .document
        .fonts_mut()
        .load_true_type_from_bytes(&bytes, true)
    {
        Ok(t) => {
            doc.loaded.insert(path.to_path_buf(), t);
            Some(t)
        }
        Err(e) => {
            tracing::info!("PDFium could not load {}: {e}", path.display());
            None
        }
    }
}

fn object_rect(page: &PdfPage, index: usize) -> Result<Rect> {
    let b = page
        .objects()
        .get(index as _)
        .map_err(pdf_error)?
        .bounds()
        .map_err(pdf_error)?
        .to_rect();
    Ok(Rect::new(
        b.left().value,
        b.bottom().value,
        b.right().value,
        b.top().value,
    ))
}

fn translate(page: &mut PdfPage, index: usize, dx: f32) -> Result<()> {
    let mut object = page.objects().get(index as _).map_err(pdf_error)?;
    object
        .translate(PdfPoints::new(dx), PdfPoints::new(0.0))
        .map_err(pdf_error)?;
    Ok(())
}

/// Puts `text` in the object at `keep`, in its own font when that font can write it, else in a
/// new object with the same font from Windows. Returns the object that holds the text now (a new
/// one is added at the end of the page's objects) and a note when Windows' font was used.
fn set_line(
    doc: &mut Doc,
    page: &mut PdfPage,
    keep: usize,
    text: &str,
    known: &HashMap<String, HashSet<char>>,
    candidates_for: &dyn Fn(&FontTraits) -> Vec<(PathBuf, String)>,
) -> Result<(usize, Option<String>)> {
    let (traits, subset) = {
        let object = page.objects().get(keep as _).map_err(pdf_error)?;
        let t = object
            .as_text_object()
            .ok_or_else(|| anyhow!("The page changed while it was edited."))?;
        let font = t.font();
        (
            traits_of(&font),
            font.is_embedded().unwrap_or(false) && fonts::is_subset(&font.name()),
        )
    };
    let known_ok = !subset
        || known
            .get(&traits.name)
            .is_some_and(|seen| text.chars().all(|c| c.is_whitespace() || seen.contains(&c)));
    if known_ok {
        let mut object = page.objects().get(keep as _).map_err(pdf_error)?;
        let t = object
            .as_text_object_mut()
            .ok_or_else(|| anyhow!("The page changed while it was edited."))?;
        t.set_text(text).map_err(pdf_error)?;
        if came_through(text, &t.text()) {
            return Ok((keep, None));
        }
    }
    // The embedded font cannot write it: the same face from Windows, in a new object in the old
    // one's place.
    add_line_like(doc, page, keep, text, f32::NAN, candidates_for)
}

/// Adds a text object like the one at `template` (font size, matrix, colour, render mode) with
/// `text`, at `baseline` (NaN: the template's own), in the first installed font that writes it.
fn add_line_like(
    doc: &mut Doc,
    page: &mut PdfPage,
    template: usize,
    text: &str,
    baseline: f32,
    candidates_for: &dyn Fn(&FontTraits) -> Vec<(PathBuf, String)>,
) -> Result<(usize, Option<String>)> {
    let (traits, matrix, size, color, mode) = {
        let object = page.objects().get(template as _).map_err(pdf_error)?;
        let t = object
            .as_text_object()
            .ok_or_else(|| anyhow!("The page changed while it was edited."))?;
        let font = t.font();
        let traits = traits_of(&font);
        (
            traits,
            t.matrix().map_err(pdf_error)?,
            t.unscaled_font_size(),
            t.fill_color().unwrap_or(PdfColor::BLACK),
            t.render_mode(),
        )
    };
    let matrix = if baseline.is_nan() {
        matrix
    } else {
        PdfMatrix::new(
            matrix.a(),
            matrix.b(),
            matrix.c(),
            matrix.d(),
            matrix.e(),
            baseline,
        )
    };
    // The template's own font first when it is not a subset and writes the text; then Windows'.
    let family = if traits.family.is_empty() {
        fonts::base_name(&traits.name).to_string()
    } else {
        traits.family.clone()
    };
    for (path, used) in candidates_for(&traits) {
        let Some(token) = font_token(doc, &path) else {
            continue;
        };
        let mut object =
            PdfPageTextObject::new(&doc.document, text, token, size).map_err(pdf_error)?;
        // A new object stands at the origin: its matrix becomes the template's.
        object.apply_matrix(matrix).map_err(pdf_error)?;
        object.set_fill_color(color).map_err(pdf_error)?;
        let _ = object.set_render_mode(mode);
        let added = page
            .objects_mut()
            .add_text_object(object)
            .map_err(pdf_error)?;
        let index = page.objects().len() - 1;
        let ok = added
            .as_text_object()
            .is_some_and(|t| came_through(text, &t.text()));
        if ok {
            let note = if baseline.is_nan() {
                format!("The PDF carries only the letters it used from {family}, so the changed text is set in {used} from Windows.")
            } else {
                format!("New lines are set in {used} from Windows.")
            };
            return Ok((index, Some(note)));
        }
        // This face cannot write it either: take it away again and try the next.
        page.objects_mut()
            .remove_object_at_index(index as _)
            .map_err(pdf_error)?;
    }
    let missing: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<HashSet<_>>()
        .into_iter()
        .take(5)
        .collect();
    bail!("No font on this computer writes that text in {family} (some of: {missing}).")
}

/// With the real PDFium: `NOOK_TEST_PDFIUM` names `pdfium.dll`, `NOOK_TEST_PDF` a PDF printed by
/// Edge from an invoice's HTML (three pages, subset fonts), `NOOK_TEST_OUT` a folder for the result:
/// `cargo test -p nook-core edits_a_real_pdf -- --ignored --nocapture`.
#[cfg(test)]
mod live {
    use super::*;

    fn env(name: &str) -> PathBuf {
        PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("set {name}")))
    }

    /// The first line on `page` whose text contains `needle`, as a click at its middle picks it.
    async fn click(editor: &PdfEditor, doc: &PdfDoc, page: u32, needle: &str) -> Block {
        let id = doc.id.clone();
        let needle = needle.to_string();
        let point = editor
            .call(move |e| {
                let d = e.doc(&id)?;
                let p = d.document.pages().get(page as _).map_err(pdf_error)?;
                let all = runs(&p);
                for l in layout::lines(all.clone()) {
                    if let Some(at) = l.text.find(&needle) {
                        // the run holding the needle's first letter
                        let mut seen = 0;
                        for r in &l.runs {
                            seen += r.text.len();
                            if seen > at && !r.text.trim().is_empty() {
                                let b = r.bounds;
                                return Ok(((b.left + b.right) / 2.0, (b.bottom + b.top) / 2.0));
                            }
                        }
                    }
                }
                bail!("no line with {needle}")
            })
            .await
            .unwrap();
        let pick = editor
            .pick(
                &doc.id,
                page,
                Area::Point {
                    x: point.0,
                    y: point.1,
                },
            )
            .await
            .unwrap();
        pick.block
            .unwrap_or_else(|| panic!("nothing picked: {:?}", pick.why))
    }

    /// Dots per inch of the made-up scan.
    const DPI: f32 = 200.0;

    /// `text` in the installed face `key` at `pt` points, stamped on a 200 dpi `img`: its origin
    /// `x` points in, its baseline `from_top` points down.
    fn stamp(
        img: &mut image::RgbImage,
        key: &str,
        text: &str,
        pt: f32,
        x: f32,
        from_top: f32,
        ink: [u8; 3],
    ) {
        use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
        let (path, _) = SystemFonts::installed()
            .file(key)
            .unwrap_or_else(|| panic!("{key} is installed"));
        let bytes = std::fs::read(path).unwrap();
        let font = FontRef::try_from_slice_and_index(&bytes, 0).unwrap();
        let k = DPI / 72.0;
        let scale = PxScale::from(pt * k * crate::pdf::fontmatch::em_ratio(&font));
        let scaled = font.as_scaled(scale);
        let (mut caret, base) = (x * k, from_top * k);
        let mut prev = None;
        for c in text.chars() {
            let id = scaled.glyph_id(c);
            if let Some(p) = prev {
                caret += scaled.kern(p, id);
            }
            if let Some(o) =
                font.outline_glyph(id.with_scale_and_position(scale, ab_glyph::point(caret, base)))
            {
                let b = o.px_bounds();
                o.draw(|gx, gy, a| {
                    let (px, py) = (b.min.x as u32 + gx, b.min.y as u32 + gy);
                    if px < img.width() && py < img.height() {
                        let p = img.get_pixel_mut(px, py);
                        for (c, i) in p.0.iter_mut().zip(ink) {
                            *c = (*c as f32 * (1.0 - a) + i as f32 * a).round() as u8;
                        }
                    }
                });
            }
            caret += scaled.h_advance(id);
            prev = Some(id);
        }
    }

    /// Whether "#rrggbb" is within `by` of `rgb` in each channel.
    fn near(hex: &str, rgb: [u8; 3], by: u8) -> bool {
        let c = color_of(hex);
        [c.red(), c.green(), c.blue()]
            .iter()
            .zip(rgb)
            .all(|(&a, b)| a.abs_diff(b) <= by)
    }

    /// A form scanned at 200 dpi (fields filled in, a rule under the date, a line in Times New
    /// Roman in blue, and the invisible text layer a scan's text recognition leaves over the date):
    /// the date is clicked and changed in the page's own letters, the country too (two letters
    /// the page does not have), and the city dragged across and set in its matched face.
    /// `NOOK_TEST_PDFIUM` and `NOOK_TEST_OUT` as above:
    /// `cargo test -p nook-core edits_text_in_a_picture -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs pdfium.dll"]
    async fn edits_text_in_a_picture() {
        let (dll, out) = (env("NOOK_TEST_PDFIUM"), env("NOOK_TEST_OUT"));
        std::fs::create_dir_all(&out).unwrap();
        let pdf = out.join("scanned-form.pdf");
        let paper = [251, 248, 240];
        let (w_pt, h_pt, page_h) = (612.0f32, 200.0f32, 792.0f32);
        let k = DPI / 72.0;
        let mut img =
            image::RgbImage::from_pixel((w_pt * k) as u32, (h_pt * k) as u32, image::Rgb(paper));
        let grey = [80, 80, 80];
        stamp(
            &mut img,
            "arial",
            "Date of birth (MM-DD-YYYY)",
            8.0,
            40.0,
            40.0,
            grey,
        );
        stamp(
            &mut img,
            "arial",
            "6-23-1990",
            11.0,
            40.0,
            62.0,
            [10, 10, 10],
        );
        for x in (36.0 * k) as u32..(250.0 * k) as u32 {
            for y in (63.8 * k) as u32..(63.8 * k) as u32 + 2 {
                img.put_pixel(x, y, image::Rgb([60, 60, 60]));
            }
        }
        stamp(&mut img, "arial", "Country", 8.0, 320.0, 40.0, grey);
        stamp(&mut img, "arial", "Turkey", 11.0, 320.0, 62.0, [10, 10, 10]);
        stamp(
            &mut img,
            "timesnewroman",
            "Istanbul, Kadikoy",
            12.0,
            40.0,
            120.0,
            [31, 58, 110],
        );

        let editor = PdfEditor::new(move || Some(dll.clone()));
        let path = pdf.clone();
        editor
            .call(move |e| {
                let mut document = e.pdfium.create_new_pdf().map_err(pdf_error)?;
                let mut page = document
                    .pages_mut()
                    .create_page_at_end(PdfPagePaperSize::new_custom(
                        PdfPoints::new(612.0),
                        PdfPoints::new(page_h),
                    ))
                    .map_err(pdf_error)?;
                let mut picture = PdfPageImageObject::new_with_size(
                    &document,
                    &image::DynamicImage::ImageRgb8(img),
                    PdfPoints::new(w_pt),
                    PdfPoints::new(h_pt),
                )
                .map_err(pdf_error)?;
                picture
                    .translate(PdfPoints::new(0.0), PdfPoints::new(page_h - h_pt))
                    .map_err(pdf_error)?;
                page.objects_mut()
                    .add_image_object(picture)
                    .map_err(pdf_error)?;
                let helvetica = document.fonts_mut().helvetica();
                let mut layer =
                    PdfPageTextObject::new(&document, "6-23-1990", helvetica, PdfPoints::new(11.0))
                        .map_err(pdf_error)?;
                layer
                    .translate(PdfPoints::new(40.0), PdfPoints::new(page_h - 62.0))
                    .map_err(pdf_error)?;
                layer
                    .set_render_mode(PdfPageTextRenderMode::Invisible)
                    .map_err(pdf_error)?;
                page.objects_mut()
                    .add_text_object(layer)
                    .map_err(pdf_error)?;
                drop(page);
                document.save_to_file(&path).map_err(pdf_error)?;
                Ok(())
            })
            .await
            .unwrap();

        let doc = editor.open(&pdf).await.unwrap();
        // A click on the date: its field, read from the picture.
        let started = std::time::Instant::now();
        let pick = editor
            .pick(
                &doc.id,
                0,
                Area::Point {
                    x: 60.0,
                    y: page_h - 58.0,
                },
            )
            .await
            .unwrap();
        println!("read in {} ms", started.elapsed().as_millis());
        let date = pick
            .block
            .unwrap_or_else(|| panic!("nothing picked: {:?}", pick.why));
        println!(
            "date: {:?}\n  {:?}\n  {:?}",
            date.lines, date.font, date.drawn
        );
        let drawn = date.drawn.clone().expect("text in a picture");
        assert_eq!(date.lines.len(), 1);
        assert_eq!(date.lines[0].text, "6-23-1990");
        assert_eq!(
            (date.font.family.as_str(), date.font.bold),
            ("Arial", false)
        );
        assert!(
            (date.lines[0].size - 11.0).abs() < 0.25,
            "{}",
            date.lines[0].size
        );
        assert!(
            (date.lines[0].baseline - (page_h - 62.0)).abs() < 0.6,
            "{}",
            date.lines[0].baseline
        );
        assert!(
            date.lines[0].rect.bottom > page_h - 63.8 + 0.5,
            "the rule under the date is not the date's"
        );
        assert!(
            near(&date.font.color, [10, 10, 10], 40),
            "{}",
            date.font.color
        );
        assert!(near(&drawn.paper, paper, 4), "{}", drawn.paper);
        assert_eq!(date.align, Align::Right, "a figure keeps its right edge");
        // In the page's own letters: every one of them is in the old date.
        let mut own = date.clone();
        own.drawn = Some(Drawn {
            from_page: true,
            ..drawn
        });
        let started = std::time::Instant::now();
        let done = editor
            .replace(&doc.id, own, vec!["9-13-1962".into()], date.align)
            .await
            .unwrap();
        println!("page letters in {} ms", started.elapsed().as_millis());
        println!("note: {:?}", done.note);
        assert_eq!(
            done.note.as_deref(),
            Some("Set in letters cut from this page.")
        );

        // The country, in the page's letters too: it has no m or n at that size and face.
        let doc = done.doc;
        let country = editor
            .pick(
                &doc.id,
                0,
                Area::Point {
                    x: 335.0,
                    y: page_h - 58.0,
                },
            )
            .await
            .unwrap()
            .block
            .expect("the country");
        assert_eq!(country.lines[0].text, "Turkey");
        let mut own = country.clone();
        own.drawn.as_mut().unwrap().from_page = true;
        let done = editor
            .replace(&doc.id, own, vec!["Turkmen".into()], Align::Left)
            .await
            .unwrap();
        println!("note: {:?}", done.note);
        assert!(
            done.note
                .as_deref()
                .unwrap()
                .contains("It has no m, n, so those are drawn in Arial."),
            "{:?}",
            done.note
        );

        // A box dragged over the city, in Times New Roman.
        let doc = done.doc;
        let pick = editor
            .pick(
                &doc.id,
                0,
                Area::Rect {
                    rect: Rect::new(34.0, page_h - 128.0, 200.0, page_h - 108.0),
                },
            )
            .await
            .unwrap();
        let city = pick
            .block
            .unwrap_or_else(|| panic!("nothing picked: {:?}", pick.why));
        println!(
            "city: {:?}\n  {:?}\n  {:?}",
            city.lines, city.font, city.drawn
        );
        assert_eq!(city.lines[0].text, "Istanbul, Kadikoy");
        assert_eq!(city.font.family, "Times New Roman");
        assert!(
            (city.lines[0].size - 12.0).abs() < 0.3,
            "{}",
            city.lines[0].size
        );
        assert!(
            near(&city.font.color, [31, 58, 110], 24),
            "{}",
            city.font.color
        );
        let done = editor
            .replace(
                &doc.id,
                city.clone(),
                vec!["Ankara, Cankaya".into()],
                Align::Left,
            )
            .await
            .unwrap();
        println!("note: {:?}", done.note);

        // The new date is part of the picture again: it reads, to change again.
        let doc = done.doc;
        let again = editor
            .pick(
                &doc.id,
                0,
                Area::Point {
                    x: 60.0,
                    y: page_h - 58.0,
                },
            )
            .await
            .unwrap()
            .block
            .expect("the new date");
        println!("again: {:?} {:?}", again.lines, again.font);
        assert!(again.drawn.is_some());
        assert_eq!(again.lines[0].text, "9-13-1962");
        assert_eq!(again.font.family, "Arial");

        let saved = editor
            .save(&doc.id, Some(out.join("scanned-form (edited).pdf")))
            .await
            .unwrap();
        for (name, id) in [("before", None), ("after", Some(saved.id.clone()))] {
            let id = match id {
                Some(id) => id,
                None => editor.open(&pdf).await.unwrap().id,
            };
            let png = editor.render(&id, 0, 1836).await.unwrap();
            std::fs::write(out.join(format!("scanned-{name}.png")), png).unwrap();
        }
        let id = saved.id.clone();
        let text = editor
            .call(move |e| {
                let d = e.doc(&id)?;
                let p = d.document.pages().get(0).map_err(pdf_error)?;
                let all = p.text().map_err(pdf_error)?.all();
                Ok(all)
            })
            .await
            .unwrap();
        println!("--- saved text ---\n{text}");
        for want in ["9-13-1962", "Turkmen", "Ankara, Cankaya"] {
            assert!(text.contains(want), "{want}");
        }
        assert!(
            !text.contains("6-23-1990"),
            "the scan's old text layer went with the date"
        );
    }

    /// A PDF made of `objects` (numbered from 1, the first the catalog), with its cross-reference
    /// table.
    fn pdf_of(objects: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend(o);
            out.extend(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
        let mut o = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        o.extend(data);
        o.extend(b"\nendstream");
        o
    }

    /// Pages drawn as design tools write them. The first has colours in an ICC profile (an icon,
    /// a big light "1" behind a heading), a see-through square and a shape clipped to a box; the
    /// second a gradient; both two lines of text. A line on each is changed, and nothing else on
    /// its page may change with it: on the first the colours are set anew as RGB and the words are
    /// truly replaced; the second's gradient PDFium cannot write at all, so its old words are
    /// covered and the new ones set over them, and the note says so.
    /// `NOOK_TEST_PDFIUM` and `NOOK_TEST_OUT` as above:
    /// `cargo test -p nook-core keeps_the_rest_of_the_page -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs pdfium.dll"]
    async fn keeps_the_rest_of_the_page() {
        let (dll, out) = (env("NOOK_TEST_PDFIUM"), env("NOOK_TEST_OUT"));
        std::fs::create_dir_all(&out).unwrap();
        let icc =
            std::fs::read(r"C:\Windows\System32\spool\drivers\color\sRGB Color Space Profile.icm")
                .expect("Windows' sRGB profile");
        let lines = "BT /F1 16 Tf 0 0 0 rg 72 500 Td (Digital Documents All In One Place) Tj ET
BT /F1 12 Tf 0.2 0.2 0.2 rg 72 470 Td (With the new experience you must) Tj ET";
        let first = format!(
            "q 0.5 0.5 0.5 RG 2 w 60 380 m 552 380 l S Q
q /CS0 cs 0.80 0.85 0.95 scn 80 600 100 120 re f Q
q /CS0 cs 0.85 0.85 0.85 scn BT /F2 220 Tf 300 440 Td (1) Tj ET Q
q /GS1 gs 1 0 0 rg 420 620 100 100 re f Q
q 60 200 80 60 re W n 0 0 1 rg 0 0 612 792 re f Q
{lines}"
        );
        let second = format!("q /Pattern cs /P1 scn 300 150 200 100 re f Q\n{lines}");
        let resources = "/Resources << /Font << /F1 7 0 R /F2 8 0 R >> /ColorSpace << /CS0 [/ICCBased 9 0 R] >> /ExtGState << /GS1 << /ca 0.4 >> >> /Pattern << /P1 10 0 R >> >>";
        let pdf = out.join("designed.pdf");
        std::fs::write(
            &pdf,
            pdf_of(&[
                b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
                b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_vec(),
                format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R {resources} >>").into_bytes(),
                format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R {resources} >>").into_bytes(),
                stream("", first.as_bytes()),
                stream("", second.as_bytes()),
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>".to_vec(),
                stream("/N 3", &icc),
                b"<< /PatternType 2 /Shading << /ShadingType 2 /ColorSpace /DeviceRGB /Coords [300 0 500 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [1 1 0] /C1 [0 0.6 0] /N 1 >> /Extend [true true] >> >>".to_vec(),
            ]),
        )
        .unwrap();

        let editor = PdfEditor::new(move || Some(dll.clone()));
        let original = editor.open(&pdf).await.unwrap();
        let mut doc = editor.open(&pdf).await.unwrap();
        let draw = |id: String, page: u32| {
            editor.call(move |e| {
                let d = e.doc(&id)?;
                let p = d.document.pages().get(page as _).map_err(pdf_error)?;
                draw_part(&p, Rect::new(0.0, 0.0, 612.0, 792.0), 1.5)
            })
        };
        let text_of = |id: String, page: u32| {
            editor.call(move |e| {
                let d = e.doc(&id)?;
                let p = d.document.pages().get(page as _).map_err(pdf_error)?;
                let all = p.text().map_err(pdf_error)?.all();
                Ok(all)
            })
        };
        for page in 0..2u32 {
            let started = std::time::Instant::now();
            let line = editor
                .pick(&doc.id, page, Area::Point { x: 100.0, y: 505.0 })
                .await
                .unwrap()
                .block
                .expect("the heading");
            assert_eq!(line.lines[0].text, "Digital Documents All In One Place");
            let done = editor
                .replace(
                    &doc.id,
                    line.clone(),
                    vec!["Digital Files - All In One Place".into()],
                    Align::Left,
                )
                .await
                .unwrap();
            println!(
                "page {}: {} ms, note: {:?}",
                page + 1,
                started.elapsed().as_millis(),
                done.note
            );
            let before = draw(original.id.clone(), page).await.unwrap();
            let after = draw(doc.id.clone(), page).await.unwrap();
            let band = Rect::new(0.0, 490.0, 612.0, 520.0);
            let changed = changed_outside(&before, &after, &[band]);
            println!("  {changed} pixels changed outside the edited line");
            assert!(
                changed < 50,
                "the rest of page {} changed: {changed} pixels",
                page + 1
            );
            let text = text_of(doc.id.clone(), page).await.unwrap();
            assert!(text.contains("Digital Files - All In One Place"), "{text}");
            if page == 0 {
                assert_eq!(done.note, None, "set anew in RGB, the page is truly edited");
                assert!(!text.contains("Digital Documents"), "{text}");
            } else {
                assert!(
                    done.note.as_deref().unwrap().contains("covered"),
                    "{:?}",
                    done.note
                );
            }
            assert_eq!(done.doc.edits, page + 1);
            assert!(done.doc.can_undo);
            doc = done.doc;
        }
        for (name, id) in [("before", original.id.clone()), ("after", doc.id.clone())] {
            for page in 0..2 {
                let png = editor.render(&id, page, 1224).await.unwrap();
                std::fs::write(out.join(format!("designed-{name}-{}.png", page + 1)), png).unwrap();
            }
        }
        let undone = editor.undo(&doc.id).await.unwrap();
        assert_eq!(undone.edits, 1);
    }

    #[tokio::test]
    #[ignore = "needs pdfium.dll and a PDF"]
    async fn edits_a_real_pdf() {
        let (dll, pdf, out) = (
            env("NOOK_TEST_PDFIUM"),
            env("NOOK_TEST_PDF"),
            env("NOOK_TEST_OUT"),
        );
        std::fs::create_dir_all(&out).unwrap();
        let editor = PdfEditor::new(move || Some(dll.clone()));
        assert!(editor.available());
        let doc = editor.open(&pdf).await.unwrap();
        println!("{} pages: {:?}", doc.pages.len(), doc.pages);

        // The bold name in its sentence: its own style, letters the subset lacks.
        let name = click(&editor, &doc, 0, "Northwind").await;
        println!(
            "picked {:?} in {:?}",
            name.lines.iter().map(|l| &l.text).collect::<Vec<_>>(),
            name.font
        );
        assert_eq!(name.lines[0].text, "Northwind Traders Ltd");
        assert!(name.font.bold);
        let done = editor
            .replace(
                &doc.id,
                name.clone(),
                vec!["Contoso Pharma GmbH".into()],
                name.align,
            )
            .await
            .unwrap();
        println!("note: {:?}", done.note);

        // A figure: kept to its right edge, a digit the embedded Consolas never had.
        let doc = done.doc;
        let total = click(&editor, &doc, 0, "5,200.00").await;
        println!(
            "picked {:?} align {:?} font {:?}",
            total.lines[0].text, total.align, total.font
        );
        assert_eq!(total.align, Align::Right);
        let done = editor
            .replace(
                &doc.id,
                total.clone(),
                vec!["€6,150.00".into()],
                total.align,
            )
            .await
            .unwrap();
        println!("note: {:?}", done.note);

        // Letters the subset has: in the embedded font itself.
        let doc = done.doc;
        let due = click(&editor, &doc, 0, "Due").await;
        println!("picked {:?}", due.lines[0].text);
        let done = editor
            .replace(
                &doc.id,
                due.clone(),
                vec![due.lines[0].text.replace("14 October", "13 October")],
                due.align,
            )
            .await
            .unwrap();
        println!("note: {:?}", done.note);

        // A dragged box over two lines of the second page, and a third line added.
        let doc = done.doc;
        let page = &doc.pages[1];
        let pick = editor
            .pick(
                &doc.id,
                1,
                Area::Rect {
                    rect: Rect::new(
                        40.0,
                        page.height - 200.0,
                        page.width - 40.0,
                        page.height - 90.0,
                    ),
                },
            )
            .await
            .unwrap();
        let block = pick.block.expect("text on page 2");
        println!(
            "page 2 lines: {:?}",
            block.lines.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
        let texts: Vec<String> = block.lines.iter().map(|l| l.text.to_uppercase()).collect();
        let done = editor
            .replace(&doc.id, block, texts, Align::Left)
            .await
            .unwrap();
        println!("note: {:?}", done.note);
        assert!(
            done.note.unwrap().contains("runs past the edge"),
            "the capitals are wider than the page"
        );

        use crate::busy::BusyWork;
        assert!(
            editor.busy_with().is_some(),
            "unsaved changes hold an automatic update back"
        );
        let saved = editor
            .save(&done.doc.id, Some(out.join("edited.pdf")))
            .await
            .unwrap();
        assert!(!saved.dirty);
        assert_eq!(editor.busy_with(), None);
        for (i, p) in saved.pages.iter().enumerate() {
            let png = editor
                .render(&saved.id, i as u32, (p.width * 2.0) as i32)
                .await
                .unwrap();
            std::fs::write(out.join(format!("page-{}.png", i + 1)), png).unwrap();
        }
        let undone = editor.undo(&saved.id).await.unwrap();
        assert_eq!(undone.edits, 3);

        // What the saved file says now.
        let again = editor.open(&out.join("edited.pdf")).await.unwrap();
        let text = editor
            .call(move |e| {
                let d = e.doc(&again.id)?;
                let mut all = String::new();
                for p in d.document.pages().iter() {
                    all.push_str(&p.text().map_err(pdf_error)?.all());
                    all.push('\n');
                }
                Ok(all)
            })
            .await
            .unwrap();
        println!("--- saved text ---\n{text}");
        for want in [
            "Contoso Pharma GmbH",
            "6,150.00",
            "13 October 2026",
            "PERCENT PER YEAR.",
        ] {
            assert!(text.contains(want), "{want}");
        }
        // The page reads in order again: the new name where the old one was.
        let bill = text.find("Bill to:").unwrap();
        assert!(
            text[bill..].starts_with("Bill to: Contoso Pharma GmbH, 42 Harbour Road"),
            "{}",
            &text[bill..bill + 60]
        );
        for gone in ["Northwind", "5,200.00", "14 October"] {
            assert!(!text.contains(gone), "{gone}");
        }
    }
}

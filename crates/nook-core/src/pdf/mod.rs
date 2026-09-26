//! The PDF editor flow: open a PDF, point at text, change it; the font, size, colour and place
//! stay as they were. New in the Rust Nook (the Kotlin app had no such flow).
//!
//! - [`PdfEditor`] (`editor.rs`): PDFium (Chrome's PDF engine, the `pdfium` engine component,
//!   downloaded on first use) on a thread of its own with the open documents: `open(path)`,
//!   `render(id, page, width)` (a PNG), `pick(id, page, area)` (the text in a dragged box or under
//!   a click, as lines), `replace(id, block, texts, align)`, `undo(id)`, `save(id, to)`,
//!   `close(id)`. Build one for the app with where `pdfium.dll` will be; nothing starts until it
//!   is used.
//! - [`PdfInstaller`] (`setup.rs`): the PDF engine's one download, with its progress on
//!   `topic::PDF`, as the translator shows its own downloads.
//! - [`layout`]: which text objects a selection covers and how they make lines (pure geometry).
//! - [`fonts`]: the installed Windows font a PDF's font is, for characters its embedded subset
//!   does not have.
//! - Text that is part of a picture (a scan, a form printed flat): [`ocr`] reads it with
//!   Windows' own text recognition, [`raster`] finds its ink, colours and baseline, and
//!   [`fontmatch`] the installed face it is set in; the editor covers the old letters with the
//!   paper's colour and sets the new text over them as text.
//!
//! The shapes are those of `ui/src/api/pdf.ts`.

pub mod editor;
pub mod fontmatch;
pub mod fonts;
pub mod layout;
pub mod ocr;
pub mod raster;
pub mod setup;

pub use editor::{
    Align, Area, Block, BlockFont, BlockLine, PdfDoc, PdfEditor, PdfPageInfo, Pick, Replaced,
};
pub use layout::Rect;
pub use setup::PdfInstaller;

//! The document converter Nooklet: files into the format asked for.
//!
//! - [`formats`]: what it reads and writes, by kind.
//! - [`routes`]: how one becomes another, step by step, each step done by an engine.
//! - [`tables`]: workbooks, CSV and JSON rows, converted by Nook itself.
//! - [`images`]: pictures, converted by Nook itself.
//! - [`pdftext`]: a PDF's text lines made into Markdown or plain text.

pub mod formats;
pub mod images;
pub mod pdftext;
pub mod routes;
pub mod run;
pub mod service;
pub mod system;
pub mod tables;
pub mod tools;

pub use service::{ConvertService, Job, Offer};

#[cfg(test)]
mod live;

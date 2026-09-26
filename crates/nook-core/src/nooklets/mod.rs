//! The Nooklets: Nook's ready-made jobs (translate speech, edit a PDF, convert documents), and
//! the finder that picks one for a request typed in a sentence.
//!
//! - [`catalog`]: the Nooklets, their example requests, and what a request sets for each.
//! - [`finder`]: the small fixed model that finds the Nooklet for a request.

pub mod catalog;
pub mod finder;

pub use finder::{Finder, FinderSetup, Found, Hit, LlamaEmbedder};

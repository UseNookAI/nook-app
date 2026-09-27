//! Reading a document's words, for the Nooklets that take documents (Summarize, Read aloud). A
//! text or Markdown file is read as it is; anything else goes through the document converter's
//! engines into Markdown ([`crate::convert::ConvertService`] is the [`Reader`]): a PDF through
//! PDFium (scans through Windows' text recognition), Word files, web pages and e-books through
//! Pandoc, old Office files through Word or LibreOffice first.

use std::path::Path;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::runtime::EngineComponent;

/// An engine a document needs downloaded before it can be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderNeed {
    pub component: EngineComponent,
    /// "the document engine (Pandoc)".
    pub what: String,
    pub bytes: u64,
}

/// What reads documents into words.
#[async_trait]
pub trait Reader: Send + Sync {
    /// The engines still to download to read `path`; Err with why Nook cannot read it.
    fn needs(&self, path: &Path) -> std::result::Result<Vec<ReaderNeed>, String>;

    /// The words of `path`, as Markdown where the document has headings, lists and tables.
    /// `work` is scratch space the caller removes.
    async fn read(&self, path: &Path, work: &Path, cancel: &CancellationToken) -> Result<String>;
}

/// The extensions read as they are, with no engine.
pub const PLAIN: &[&str] = &["txt", "text", "md", "markdown"];

/// Whether `path` is read as it is.
pub fn is_plain(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| PLAIN.contains(&e.as_str()))
}

/// A text file's words: UTF-8 (its BOM dropped), UTF-16 with a BOM, else the Windows code page
/// most text files from older programs are in.
pub fn read_plain(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("Could not read {}", path.display()))?;
    Ok(decode(&bytes))
}

pub fn decode(bytes: &[u8]) -> String {
    if let Some((encoding, bom)) = encoding_rs::Encoding::for_bom(bytes) {
        return encoding
            .decode_without_bom_handling(&bytes[bom..])
            .0
            .into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_comes_in_whatever_its_encoding() {
        assert_eq!(decode("Grüße".as_bytes()), "Grüße");
        assert_eq!(decode(b"\xEF\xBB\xBFhello"), "hello");
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("Hi é".encode_utf16().flat_map(|u| u.to_le_bytes()))
            .collect();
        assert_eq!(decode(&utf16), "Hi é");
        assert_eq!(decode(b"caf\xE9"), "café", "Windows-1252");
        assert!(is_plain(Path::new("C:/a/Notes.MD")));
        assert!(!is_plain(Path::new("C:/a/report.pdf")));
    }
}

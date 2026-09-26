//! The worker's way onto the web. Ports `ai.nook.agent.web`: [`Web`] (what the worker's web tools
//! need), [`WebAccess`] (the real one, over this computer's own connection), DuckDuckGo's HTML
//! results page, and [`page_text`] (a fetched page as the text the worker reads).
//!
//! Which addresses the worker may open (only ones it was shown), the searches per request and
//! reading a page in parts are the web tools' business (`worker/WebTools.java`, ported with the
//! worker loop); this module is the connection and the reading.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

mod duckduckgo;
pub mod page_text;
pub mod web_access;

pub use page_text::Page;
pub use web_access::WebAccess;

/// One search result: the page's title, its address and the search engine's snippet. (`Web.Result`
/// in the original, renamed so it does not shadow `Result`.)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

impl SearchResult {
    pub fn new(
        title: impl Into<String>,
        url: impl Into<String>,
        snippet: impl Into<String>,
    ) -> SearchResult {
        SearchResult {
            title: title.into(),
            url: url.into(),
            snippet: snippet.into(),
        }
    }
}

/// What the worker's web tools need from the internet: a search and a page read. [`WebAccess`]
/// does both over this computer's own connection; tests stand in for it.
///
/// Errors carry the sentence the worker is shown after `error: `, as the original's IOException
/// messages did.
#[async_trait]
pub trait Web: Send + Sync {
    async fn search(&self, query: &str) -> Result<Vec<SearchResult>>;

    async fn fetch(&self, url: &str) -> Result<Page>;
}

//! A web page as the worker reads it: the readable part of an HTML page as plain text, headings
//! marked with #, list items with -, code kept as it is between ``` fences, and each link numbered
//! [n] after its text with its address in [`Page::links`]. Scripts, styles, navigation, footers and
//! form controls are dropped, and when a page marks its main content (main, role=main, a single
//! article) only that is kept. Plain text, JSON and PDF come as they are.
//!
//! Ports `web/PageText.java`. The original parsed with jsoup and read PDFs with Tika; here the
//! page is html5ever's tree (through scraper) and a PDF goes through pdf-extract. Where jsoup's
//! behaviour decides the text (its whitespace rules, what `text()` and `hasText()` mean, a
//! `<base href>`), it is reproduced below.

use std::collections::HashMap;

use anyhow::{bail, Result};
use ego_tree::{NodeId, NodeRef};
use encoding_rs::Encoding;
use once_cell::sync::Lazy;
use regex::Regex;
use scraper::{Html, Node};
use serde::{Deserialize, Serialize};
use url::Url;

/// At most this many links are numbered on one page.
pub const MAX_LINKS: usize = 300;
/// Text beyond this is cut: the worker reads a page in parts, and nobody reads a hundred of them.
/// Counted in characters (the original counted UTF-16 units, the same for all but rare scripts).
pub const MAX_CHARS: usize = 400_000;

/// A page the worker read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    /// Where the page was read, after redirects.
    pub url: String,
    pub title: String,
    pub text: String,
    /// Link [n] in the text is `links[n - 1]`.
    pub links: Vec<String>,
    /// The page was longer than Nook reads.
    pub cut: bool,
}

/// A fetched body as a page, by its content type (sniffed when the server gives none).
///
/// Parsing is CPU work (a page can be five megabytes): call it from `spawn_blocking` in async code.
pub fn of(url: &str, content_type: Option<&str>, body: &[u8], cut: bool) -> Result<Page> {
    let kind = content_type.unwrap_or("").to_lowercase();
    let mut mime = kind.split(';').next().unwrap_or("").trim().to_string();
    let charset = charset(&kind);
    if mime.is_empty() || mime == "application/octet-stream" {
        mime = sniff(body).to_string();
    }
    if mime == "text/html" || mime == "application/xhtml+xml" {
        return Ok(from_document(parse_bytes(body, charset), url, cut));
    }
    if mime == "application/pdf" {
        if cut {
            bail!("the PDF is larger than Nook reads");
        }
        return pdf(url, body);
    }
    if mime.starts_with("text/")
        || mime.ends_with("/json")
        || mime.ends_with("+json")
        || mime.ends_with("/xml")
        || mime.ends_with("+xml")
        || mime.ends_with("javascript")
        || mime.ends_with("yaml")
        || mime.ends_with("toml")
    {
        let (text, _, _) = charset.unwrap_or(encoding_rs::UTF_8).decode(body);
        return Ok(plain(url, &text, cut));
    }
    bail!("that address is not a page Nook can read ({mime})")
}

/// A page from HTML text, as if read from `url`.
pub fn from_html(html: &str, url: &str) -> Page {
    from_document(Html::parse_document(html), url, false)
}

/// The address without its `#fragment`.
pub fn without_fragment(url: &str) -> &str {
    match url.find('#') {
        Some(hash) => &url[..hash],
        None => url,
    }
}

fn from_document(mut doc: Html, url: &str, cut: bool) -> Page {
    let mut title = document_title(&doc);
    let base = base_uri(&doc, url);

    let gone: Vec<NodeId> = doc
        .tree
        .root()
        .descendants()
        .filter(|n| never_text(*n))
        .map(|n| n.id())
        .collect();
    detach(&mut doc, &gone);

    let mut text = String::new();
    let mut links = Vec::new();
    let mut h1 = None;
    if let Some(body) = body(&doc) {
        let root = main_content(body);
        let root_id = root.id();
        let chrome: Vec<NodeId> = root
            .descendants()
            .skip(1)
            .filter(|n| is_chrome(*n))
            .map(|n| n.id())
            .collect();
        detach(&mut doc, &chrome);
        // A site's header is its logo and menu; an article's header holds its title, which stays.
        let headers: Vec<NodeId> = match doc.tree.get(root_id) {
            Some(root) => root
                .descendants()
                .skip(1)
                .filter(|n| {
                    element_name(*n) == Some("header")
                        && !n.descendants().any(|d| element_name(d) == Some("h1"))
                })
                .map(|n| n.id())
                .collect(),
            None => Vec::new(),
        };
        detach(&mut doc, &headers);

        if let Some(root) = doc.tree.get(root_id) {
            let mut r = Renderer::new(url, base);
            walk(root, &mut r);
            text = tidy(&r.out);
            links = r.links;
            h1 = root
                .descendants()
                .find(|n| element_name(*n) == Some("h1"))
                .map(|n| element_text(n));
        }
    }
    if title.is_empty() {
        title = match h1 {
            Some(h) => h.trim_matches(java_ws).to_string(),
            None => url.to_string(),
        };
    }
    let (text, longer) = cut_chars(text, MAX_CHARS);
    Page {
        url: url.to_string(),
        title,
        text,
        links,
        cut: cut || longer,
    }
}

/// Scripts, styles, embedded things, form controls and what the page hides.
fn never_text(n: NodeRef<Node>) -> bool {
    let Some(e) = n.value().as_element() else {
        return false;
    };
    matches!(
        e.name(),
        "script"
            | "style"
            | "noscript"
            | "template"
            | "svg"
            | "canvas"
            | "iframe"
            | "object"
            | "embed"
            | "dialog"
            | "button"
            | "input"
            | "select"
            | "textarea"
    ) || e.attr("hidden").is_some()
        || attr_is(e, "aria-hidden", "true")
}

/// A site's navigation, sidebars, footers and search.
fn is_chrome(n: NodeRef<Node>) -> bool {
    let Some(e) = n.value().as_element() else {
        return false;
    };
    matches!(e.name(), "nav" | "aside" | "footer")
        || [
            "navigation",
            "banner",
            "contentinfo",
            "complementary",
            "search",
        ]
        .iter()
        .any(|role| attr_is(e, "role", role))
}

/// jsoup's `[name=value]`: the value trimmed and compared without case.
fn attr_is(e: &scraper::node::Element, name: &str, value: &str) -> bool {
    e.attr(name)
        .is_some_and(|v| v.trim().eq_ignore_ascii_case(value))
}

fn element_name<'a>(n: NodeRef<'a, Node>) -> Option<&'a str> {
    n.value().as_element().map(|e| e.name())
}

fn detach(doc: &mut Html, ids: &[NodeId]) {
    for id in ids {
        if let Some(mut n) = doc.tree.get_mut(*id) {
            n.detach();
        }
    }
}

fn html_element(doc: &Html) -> Option<NodeRef<'_, Node>> {
    doc.tree
        .root()
        .children()
        .find(|n| element_name(*n) == Some("html"))
}

fn body(doc: &Html) -> Option<NodeRef<'_, Node>> {
    html_element(doc)?
        .children()
        .find(|n| matches!(element_name(*n), Some("body") | Some("frameset")))
}

/// The text of the head's first title, as jsoup's `Document.title()`.
fn document_title(doc: &Html) -> String {
    let head =
        html_element(doc).and_then(|h| h.children().find(|n| element_name(*n) == Some("head")));
    match head.and_then(|h| h.descendants().find(|n| element_name(*n) == Some("title"))) {
        Some(t) => normalise_ws(&element_text(t), false)
            .trim_matches(|c| c <= ' ')
            .to_string(),
        None => String::new(),
    }
}

/// Where relative links lead from: the page's first `<base href>`, else the page itself.
fn base_uri(doc: &Html, url: &str) -> Option<Url> {
    let page = Url::parse(url).ok();
    let base = doc
        .tree
        .root()
        .descendants()
        .filter_map(|n| {
            n.value()
                .as_element()
                .filter(|e| e.name() == "base")
                .and_then(|e| e.attr("href"))
        })
        .find(|href| !href.is_empty());
    match (base, &page) {
        (Some(href), Some(p)) => p.join(href).ok().or(page),
        (Some(href), None) => Url::parse(href).ok(),
        (None, _) => page,
    }
}

/// The page's main content when it marks one and it holds real text, else the whole body.
fn main_content(body: NodeRef<Node>) -> NodeRef<Node> {
    let marks: [fn(&scraper::node::Element) -> bool; 3] = [
        |e| e.name() == "main",
        |e| attr_is(e, "role", "main"),
        |e| e.name() == "article",
    ];
    for mark in marks {
        let found: Vec<NodeRef<Node>> = body
            .descendants()
            .filter(|n| n.value().as_element().is_some_and(mark))
            .take(2)
            .collect();
        if found.len() == 1 && element_text(found[0]).chars().count() >= 200 {
            return found[0];
        }
    }
    body
}

pub(crate) fn plain(url: &str, text: &str, cut: bool) -> Page {
    let t = text.replace("\r\n", "\n");
    let (t, longer) = cut_chars(t.trim_matches(java_ws).to_string(), MAX_CHARS);
    Page {
        url: url.to_string(),
        title: last_segment(url),
        text: t,
        links: Vec::new(),
        cut: cut || longer,
    }
}

fn pdf(url: &str, body: &[u8]) -> Result<Page> {
    // pdf-extract panics on some damaged or unusual files; a page must never take the worker down.
    match std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(body)) {
        Ok(Ok(text)) => Ok(plain(url, &text, false)),
        Ok(Err(e)) => bail!("could not read the PDF: {e}"),
        Err(_) => bail!(
            "could not read the PDF: it is damaged or uses something Nook's reader does not know"
        ),
    }
}

fn last_segment(url: &str) -> String {
    let mut u = without_fragment(url);
    if let Some(q) = u.find('?') {
        u = &u[..q];
    }
    let u = u.trim_end_matches('/');
    let last = match u.rfind('/') {
        Some(slash) => &u[slash + 1..],
        None => u,
    };
    if last.is_empty() || last.contains(':') {
        url.to_string()
    } else {
        last.to_string()
    }
}

/// The charset a Content-Type names, when it is one this reader knows.
fn charset(content_type: &str) -> Option<&'static Encoding> {
    let at = content_type.find("charset=")?;
    let name = content_type[at + 8..].replace('"', "");
    let name = name.split(';').next().unwrap_or("").trim();
    Encoding::for_label_no_replacement(name.as_bytes())
}

fn sniff(body: &[u8]) -> &'static str {
    let head = &body[..body.len().min(1024)];
    if head.starts_with(b"%PDF-") {
        return "application/pdf";
    }
    let lower = head.to_ascii_lowercase();
    if contains(&lower, b"<html") || contains(&lower, b"<!doctype html") {
        return "text/html";
    }
    if head.contains(&0) {
        "application/octet-stream"
    } else {
        "text/plain"
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// HTML bytes as a document, decoded as jsoup decodes them: a byte-order mark first, then the
/// charset the server named, then a `<meta>` charset in the page, else UTF-8.
fn parse_bytes(body: &[u8], declared: Option<&'static Encoding>) -> Html {
    let (text, _, _) = declared.unwrap_or(encoding_rs::UTF_8).decode(body);
    let doc = Html::parse_document(&text);
    if declared.is_some() || Encoding::for_bom(body).is_some() {
        return doc;
    }
    match meta_charset(&doc) {
        // A page that could be read as ASCII to find its meta is not UTF-16, whatever it says.
        Some(enc)
            if enc != encoding_rs::UTF_8
                && enc != encoding_rs::UTF_16LE
                && enc != encoding_rs::UTF_16BE =>
        {
            let (text, _, _) = enc.decode(body);
            Html::parse_document(&text)
        }
        _ => doc,
    }
}

static META_CHARSET: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?i)\bcharset=\s*["']?([^\s,;"']*)"#).expect("a valid pattern"));

fn meta_charset(doc: &Html) -> Option<&'static Encoding> {
    for n in doc.tree.root().descendants() {
        let Some(e) = n.value().as_element().filter(|e| e.name() == "meta") else {
            continue;
        };
        let equiv = attr_is(e, "http-equiv", "content-type");
        if !equiv && e.attr("charset").is_none() {
            continue;
        }
        let mut found = None;
        if e.attr("http-equiv").is_some() {
            found = e
                .attr("content")
                .and_then(|c| META_CHARSET.captures(c))
                .and_then(|c| c.get(1))
                .and_then(|m| Encoding::for_label_no_replacement(m.as_str().trim().as_bytes()));
        }
        if found.is_none() {
            if let Some(cs) = e.attr("charset") {
                found = Encoding::for_label_no_replacement(
                    cs.trim().replace(['"', '\''], "").as_bytes(),
                );
            }
        }
        if found.is_some() {
            return found;
        }
    }
    None
}

/// Trailing spaces off every line, at most one blank line in a row, except inside code fences.
pub(crate) fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut code = false;
    let mut blanks = 0;
    for line in s.split('\n') {
        if line.starts_with("```") {
            code = !code;
        }
        let mut l = if code {
            line
        } else {
            line.trim_end_matches(java_ws)
        };
        if !code && is_blank(l) {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
            l = "";
        } else {
            blanks = 0;
        }
        out.push_str(l);
        out.push('\n');
    }
    out.trim_matches(java_ws).to_string()
}

fn cut_chars(mut s: String, max: usize) -> (String, bool) {
    match s.char_indices().nth(max) {
        Some((at, _)) => {
            s.truncate(at);
            (s, true)
        }
        None => (s, false),
    }
}

// ------------------------------------------------------------------ jsoup's text rules

/// `Character.isWhitespace`, which Java's strip, isBlank and the renderer use.
fn java_ws(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{1C}'..='\u{1F}'
    ) || (c.is_whitespace() && !matches!(c, '\u{85}' | '\u{A0}' | '\u{2007}' | '\u{202F}'))
}

fn is_blank(s: &str) -> bool {
    s.chars().all(java_ws)
}

/// jsoup's `StringUtil.isActuallyWhitespace`: what its text normalisation collapses.
fn actually_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{0C}' | '\r' | '\u{A0}')
}

/// jsoup's `StringUtil.appendNormalisedWhitespace`: runs of whitespace as one space, zero-width
/// spaces and soft hyphens dropped, leading whitespace dropped when asked.
fn normalise_ws_into(out: &mut String, s: &str, strip_leading: bool) {
    let mut last_was_white = false;
    let mut reached_non_white = false;
    for c in s.chars() {
        if actually_ws(c) {
            if (strip_leading && !reached_non_white) || last_was_white {
                continue;
            }
            out.push(' ');
            last_was_white = true;
        } else if c != '\u{200B}' && c != '\u{AD}' {
            out.push(c);
            last_was_white = false;
            reached_non_white = true;
        }
    }
}

fn normalise_ws(s: &str, strip_leading: bool) -> String {
    let mut out = String::with_capacity(s.len());
    normalise_ws_into(&mut out, s, strip_leading);
    out
}

/// jsoup's block tags, between which `text()` puts a space.
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "html"
            | "head"
            | "body"
            | "frameset"
            | "script"
            | "noscript"
            | "style"
            | "meta"
            | "link"
            | "title"
            | "frame"
            | "noframes"
            | "section"
            | "nav"
            | "aside"
            | "hgroup"
            | "header"
            | "footer"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "pre"
            | "div"
            | "blockquote"
            | "hr"
            | "address"
            | "figure"
            | "figcaption"
            | "form"
            | "fieldset"
            | "ins"
            | "del"
            | "dl"
            | "dt"
            | "dd"
            | "li"
            | "table"
            | "caption"
            | "thead"
            | "tfoot"
            | "tbody"
            | "colgroup"
            | "col"
            | "tr"
            | "th"
            | "td"
            | "video"
            | "audio"
            | "canvas"
            | "details"
            | "menu"
            | "plaintext"
            | "template"
            | "article"
            | "main"
            | "svg"
            | "math"
            | "center"
            | "dir"
            | "applet"
            | "marquee"
            | "listing"
    )
}

/// Text inside these keeps its whitespace in jsoup's `text()`.
fn preserves_whitespace(n: NodeRef<Node>) -> bool {
    let mut cur = n.parent();
    for _ in 0..6 {
        let Some(p) = cur else { return false };
        if matches!(
            element_name(p),
            Some("pre" | "plaintext" | "title" | "textarea")
        ) {
            return true;
        }
        cur = p.parent();
    }
    false
}

/// An element's text as jsoup's `Element.text()` gives it: whitespace normalised, a space between
/// blocks, trimmed.
pub(crate) fn element_text(el: NodeRef<Node>) -> String {
    struct Text {
        out: String,
    }
    impl Text {
        fn last_is_space(&self) -> bool {
            self.out.ends_with(' ')
        }
    }
    impl<'a> Visitor<'a> for Text {
        fn head(&mut self, n: NodeRef<'a, Node>) -> Flow {
            match n.value() {
                Node::Text(t) => {
                    if preserves_whitespace(n) {
                        self.out.push_str(t);
                    } else {
                        let strip = self.last_is_space();
                        normalise_ws_into(&mut self.out, t, strip);
                    }
                }
                Node::Element(e)
                    if !self.out.is_empty()
                        && (is_block(e.name()) || e.name() == "br")
                        && !self.last_is_space() =>
                {
                    self.out.push(' ');
                }
                _ => {}
            }
            Flow::Continue
        }

        fn tail(&mut self, n: NodeRef<'a, Node>) {
            let Some(e) = n.value().as_element() else {
                return;
            };
            if !is_block(e.name()) || self.last_is_space() {
                return;
            }
            let next_is_inline = match n.next_sibling().map(|s| s.value()) {
                Some(Node::Text(_)) => true,
                Some(Node::Element(s)) => !is_block(s.name()),
                _ => false,
            };
            if next_is_inline {
                self.out.push(' ');
            }
        }
    }
    let mut t = Text { out: String::new() };
    walk(el, &mut t);
    t.out.trim_matches(|c| c <= ' ').to_string()
}

/// Everything a `<pre>` holds, as written, a `<br>` as a line break (jsoup's `wholeText()`).
fn whole_text(el: NodeRef<Node>) -> String {
    let mut out = String::new();
    for n in el.descendants() {
        match n.value() {
            Node::Text(t) => out.push_str(t),
            Node::Element(e) if e.name() == "br" => out.push('\n'),
            _ => {}
        }
    }
    out
}

/// jsoup's `hasText()`: some text in it that is not whitespace.
fn has_text(el: NodeRef<Node>) -> bool {
    el.descendants()
        .any(|n| matches!(n.value(), Node::Text(t) if !t.chars().all(actually_ws)))
}

// ------------------------------------------------------------------ walking the tree

enum Flow {
    Continue,
    /// Neither the children nor the element's tail.
    Skip,
}

trait Visitor<'a> {
    fn head(&mut self, n: NodeRef<'a, Node>) -> Flow;
    fn tail(&mut self, n: NodeRef<'a, Node>);
}

/// Visits a subtree in document order, heads on the way in and tails on the way out, without
/// recursion: a page can nest thousands of elements deep.
fn walk<'a>(root: NodeRef<'a, Node>, v: &mut impl Visitor<'a>) {
    let mut stack = vec![(root, false)];
    while let Some((n, closing)) = stack.pop() {
        if closing {
            v.tail(n);
            continue;
        }
        if let Flow::Continue = v.head(n) {
            stack.push((n, true));
            let children: Vec<NodeRef<'a, Node>> = n.children().collect();
            stack.extend(children.into_iter().rev().map(|c| (c, false)));
        }
    }
}

/// Walks the kept part of a page into text, numbering links as it meets them.
struct Renderer {
    out: String,
    links: Vec<String>,
    index: HashMap<String, usize>,
    /// This page's address without a fragment, as a link to it would be spelled.
    page: String,
    base: Option<Url>,
}

impl Renderer {
    fn new(url: &str, base: Option<Url>) -> Renderer {
        let page = without_fragment(url);
        let page = Url::parse(page)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| page.to_string());
        Renderer {
            out: String::new(),
            links: Vec::new(),
            index: HashMap::new(),
            page,
            base,
        }
    }

    fn last_byte(&self) -> Option<u8> {
        self.out.as_bytes().last().copied()
    }

    fn link(&mut self, a: NodeRef<Node>, e: &scraper::node::Element) {
        if !has_text(a) {
            return;
        }
        let href = self.abs_url(e.attr("href"));
        if !href.starts_with("http://") && !href.starts_with("https://") {
            return;
        }
        let url = without_fragment(&href).to_string();
        if url == self.page {
            return; // a jump within this page
        }
        let n = match self.index.get(&url) {
            Some(n) => *n,
            None => {
                if self.links.len() >= MAX_LINKS {
                    return;
                }
                self.links.push(url.clone());
                self.index.insert(url, self.links.len());
                self.links.len()
            }
        };
        self.trim_spaces();
        self.out.push_str(&format!(" [{n}]"));
    }

    /// jsoup's `absUrl`: the address resolved against the page, as it is when already absolute,
    /// empty when it cannot be made one.
    fn abs_url(&self, href: Option<&str>) -> String {
        let Some(href) = href else {
            return String::new();
        };
        let resolved = match &self.base {
            Some(base) => base.join(href),
            None => Url::parse(href),
        };
        match resolved {
            Ok(u) => u.to_string(),
            Err(_) if has_scheme(href) => href.to_string(),
            Err(_) => String::new(),
        }
    }

    fn text(&mut self, s: &str) {
        if is_blank(s) {
            if self.out.chars().next_back().is_some_and(|c| !java_ws(c)) {
                self.out.push(' ');
            }
            return;
        }
        let line_start = matches!(self.last_byte(), None | Some(b'\n') | Some(b' '));
        self.out.push_str(if line_start {
            s.trim_start_matches(java_ws)
        } else {
            s
        });
    }

    fn trim_spaces(&mut self) {
        let end = self.out.trim_end_matches(' ').len();
        self.out.truncate(end);
    }

    /// Ends the current line and leaves `lines - 1` blank lines after it (none at the very start).
    fn block(&mut self, lines: usize) {
        self.trim_spaces();
        if self.out.is_empty() {
            return;
        }
        let have = self.out.len() - self.out.trim_end_matches('\n').len();
        for _ in have..lines {
            self.out.push('\n');
        }
    }
}

impl<'a> Visitor<'a> for Renderer {
    fn head(&mut self, n: NodeRef<'a, Node>) -> Flow {
        let e = match n.value() {
            Node::Text(t) => {
                self.text(&normalise_ws(t, false));
                return Flow::Continue;
            }
            Node::Element(e) => e,
            _ => return Flow::Continue,
        };
        match e.name() {
            "pre" => {
                self.block(2);
                self.out.push_str("```\n");
                self.out.push_str(
                    whole_text(n)
                        .replace("\r\n", "\n")
                        .trim_end_matches(java_ws),
                );
                self.out.push_str("\n```");
                self.block(2);
                return Flow::Skip;
            }
            "img" => return Flow::Skip,
            name @ ("h1" | "h2" | "h3" | "h4" | "h5" | "h6") => {
                self.block(2);
                let level = usize::from(name.as_bytes()[1] - b'0');
                self.out.push_str(&"#".repeat(level));
                self.out.push(' ');
            }
            "p" | "blockquote" | "table" | "ul" | "ol" | "dl" | "figure" | "hr" => self.block(2),
            "li" => {
                self.block(1);
                self.out.push_str("- ");
            }
            "div" | "section" | "article" | "main" | "header" | "tr" | "dt" | "dd"
            | "figcaption" | "details" | "summary" | "address" => self.block(1),
            "br" => self.out.push('\n'),
            "td" | "th" => {
                if !self.out.is_empty() && self.last_byte() != Some(b'\n') {
                    self.out.push_str(" | ");
                }
            }
            "code" => self.out.push('`'),
            _ => {}
        }
        Flow::Continue
    }

    fn tail(&mut self, n: NodeRef<'a, Node>) {
        let Some(e) = n.value().as_element() else {
            return;
        };
        match e.name() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "blockquote" | "table" | "ul"
            | "ol" | "dl" | "figure" => self.block(2),
            "div" | "section" | "article" | "main" | "header" | "li" | "tr" | "dt" | "dd"
            | "figcaption" | "details" | "summary" | "address" => self.block(1),
            "code" => self.out.push('`'),
            "a" => self.link(n, e),
            _ => {}
        }
    }
}

/// Starts with a scheme (`mailto:`, `javascript:`): jsoup keeps such a value as it is.
fn has_scheme(s: &str) -> bool {
    let mut chars = s.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    for c in chars {
        if c == ':' {
            return true;
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://docs.gradle.org/current/userguide/toolchains.html";

    fn filler() -> String {
        format!(
            "<p>{}</p>",
            "Toolchains let a build pick the JDK it compiles with, whatever runs Gradle. "
                .repeat(4)
        )
    }

    #[test]
    fn keeps_the_main_content_with_code_lists_and_numbered_links() {
        let html = String::new()
            + "<html><head><title>Toolchains - Gradle</title><style>body { color: red }</style><script>var tracking = 1;</script></head><body>"
            + "<header><a href=\"/\">Home</a><nav><a href=\"/docs\">Docs menu</a></nav></header>"
            + "<main><h1>Toolchains for JVM projects</h1>"
            + "<p>A <b>Java toolchain</b> is a set of tools. See <a href=\"jvm.html\">the JVM guide</a> and <a href=\"#usage\">below</a>.</p>"
            + "<pre><code>java {\n    toolchain {\n        languageVersion = JavaLanguageVersion.of(21)\n    }\n}</code></pre>"
            + "<ul><li>First</li><li>Second <code>--offline</code></li></ul>"
            + "<p>Also <a href=\"https://example.org/a\">one</a>, and <a href=\"jvm.html#x\">the guide</a> again.</p>"
            + &filler()
            + "<button>Copy</button></main>"
            + "<footer><a href=\"/privacy\">Privacy</a></footer></body></html>";

        let p = from_html(&html, URL);

        assert_eq!("Toolchains - Gradle", p.title);
        let t = &p.text;
        assert!(t.starts_with("# Toolchains for JVM projects\n\n"), "{t}");
        assert!(
            t.contains("A Java toolchain is a set of tools. See the JVM guide [1] and below."),
            "{t}"
        );
        assert!(
            t.contains("```\njava {\n    toolchain {\n        languageVersion = JavaLanguageVersion.of(21)\n    }\n}\n```"),
            "{t}"
        );
        assert!(t.contains("- First\n- Second `--offline`"), "{t}");
        assert!(t.contains("Also one [2], and the guide [1] again."), "{t}");
        assert_eq!(
            vec![
                "https://docs.gradle.org/current/userguide/jvm.html",
                "https://example.org/a"
            ],
            p.links
        );
        for gone in [
            "tracking",
            "color: red",
            "Docs menu",
            "Home",
            "Privacy",
            "Copy",
        ] {
            assert!(!t.contains(gone), "{gone} should be gone:\n{t}");
        }
    }

    #[test]
    fn without_a_main_element_reads_the_body_without_its_chrome() {
        let html = "<html><body><header><a href=\"/\">Logo</a></header><nav>Menu</nav>\
            <h2>Install</h2><p>Run <code>npm i left-pad</code>.</p><table><tr><th>Flag</th><th>Meaning</th></tr>\
            <tr><td>-g</td><td>global</td></tr></table><aside>Ads</aside><footer>(c) 2026</footer></body></html>";

        let p = from_html(html, "https://example.com/install");

        assert_eq!(
            "## Install\n\nRun `npm i left-pad`.\n\nFlag | Meaning\n-g | global",
            p.text
        );
        assert_eq!(
            "https://example.com/install", p.title,
            "no title and no h1: the address"
        );
    }

    #[test]
    fn plain_text_and_json_come_as_they_are() {
        let json = br#"{"tag_name": "v1.2.3"}"#;
        let p = of(
            "https://api.github.com/repos/o/r/releases/latest",
            Some("application/json; charset=utf-8"),
            json,
            false,
        )
        .unwrap();
        assert_eq!(r#"{"tag_name": "v1.2.3"}"#, p.text);
        assert_eq!("latest", p.title);
        assert!(p.links.is_empty());
    }

    #[test]
    fn reads_the_charset_the_server_names_and_sniffs_a_page_without_a_type() {
        let latin: Vec<u8> = b"<html><body><p>caf\xE9</p></body></html>".to_vec();
        assert_eq!(
            "café",
            of(
                "https://example.com/",
                Some("text/html; charset=ISO-8859-1"),
                &latin,
                false
            )
            .unwrap()
            .text
        );
        let untyped = b"<!DOCTYPE html><html><body><h1>Hi</h1></body></html>";
        assert_eq!(
            "# Hi",
            of("https://example.com/", None, untyped, false)
                .unwrap()
                .text
        );
    }

    #[test]
    fn a_meta_charset_in_the_page_is_honoured() {
        let page = b"<html><head><meta charset=\"windows-1252\"></head><body><p>na\xEFve</p></body></html>";
        assert_eq!(
            "naïve",
            of("https://example.com/", Some("text/html"), page, false)
                .unwrap()
                .text
        );
        let equiv =
            b"<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=iso-8859-1\"></head><body>\xE9t\xE9</body></html>";
        assert_eq!(
            "été",
            of("https://example.com/", None, equiv, false).unwrap().text
        );
    }

    #[test]
    fn refuses_what_is_not_a_page() {
        let e = of(
            "https://example.com/x.png",
            Some("image/png"),
            &[0x89, b'P', b'N', b'G'],
            false,
        )
        .unwrap_err();
        assert!(e.to_string().contains("image/png"), "{e}");
        assert!(
            of(
                "https://example.com/big.pdf",
                Some("application/pdf"),
                b"%PDF-1.7",
                true
            )
            .is_err(),
            "a PDF cut short cannot be parsed"
        );
    }

    #[test]
    fn a_very_long_page_is_cut_and_says_so() {
        let html = format!(
            "<html><body><pre>{}</pre></body></html>",
            "x".repeat(MAX_CHARS + 10)
        );
        let p = from_html(&html, "https://example.com/");
        assert_eq!(MAX_CHARS, p.text.chars().count());
        assert!(p.cut);
    }

    #[test]
    fn a_pdf_is_read_as_text() {
        let p = of(
            "https://example.com/papers/guide.pdf",
            Some("application/pdf"),
            &tiny_pdf("Hello from a PDF"),
            false,
        )
        .unwrap();
        assert!(p.text.contains("Hello from a PDF"), "{:?}", p.text);
        assert_eq!("guide.pdf", p.title);
        let damaged = of(
            "https://example.com/bad.pdf",
            Some("application/pdf"),
            b"%PDF-1.7\nnot really",
            false,
        );
        assert!(damaged
            .unwrap_err()
            .to_string()
            .starts_with("could not read the PDF"));
    }

    #[test]
    fn hidden_things_and_forms_are_dropped_and_a_base_href_is_followed() {
        let html = "<html><head><base href=\"https://cdn.example.net/docs/\"></head><body>\
            <p>Visible <span hidden>secret</span><span aria-hidden=\"TRUE\">icon</span> <a href=\"page.html\">next</a></p>\
            <p><a href=\"mailto:a@b.c\">mail</a> <a href=\"https://x.example/i\"><img src=\"i.png\"></a></p>\
            <form><input value=\"typed\"><select><option>Pick</option></select><textarea>notes</textarea></form></body></html>";
        let p = from_html(html, "https://example.com/start");
        assert_eq!("Visible next [1]\n\nmail", p.text);
        assert_eq!(
            vec!["https://cdn.example.net/docs/page.html"],
            p.links,
            "an image link has no text, so no number"
        );
    }

    #[test]
    fn whitespace_is_collapsed_as_a_browser_shows_it() {
        let html = "<html><body>\n  <div>\n    <p>One\n      two&nbsp;&nbsp;three</p>\n  </div>\n  <p>Four<br>five</p>\n</body></html>";
        assert_eq!(
            "One two three\n\nFour\nfive",
            from_html(html, "https://example.com/").text
        );
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let html = format!(
            "<html><body>{}deep{}</body></html>",
            "<div>".repeat(20_000),
            "</div>".repeat(20_000)
        );
        assert_eq!("deep", from_html(&html, "https://example.com/").text);
    }

    #[test]
    fn link_numbers_stop_at_the_cap() {
        let mut html = String::from("<html><body><p>");
        for i in 0..MAX_LINKS + 5 {
            html.push_str(&format!("<a href=\"/p{i}\">p{i}</a> "));
        }
        html.push_str("</p></body></html>");
        let p = from_html(&html, "https://example.com/");
        assert_eq!(MAX_LINKS, p.links.len());
        assert!(p
            .text
            .contains(&format!("p{} [{}]", MAX_LINKS - 1, MAX_LINKS)));
        assert!(
            p.text.ends_with(&format!("p{}", MAX_LINKS + 4)),
            "a link past the cap keeps its text, unnumbered"
        );
    }

    #[test]
    fn tidy_keeps_code_fences_as_they_are() {
        assert_eq!("a\n\nb", tidy("a  \n\n\n\nb\n\n"));
        assert_eq!("```\nx  \n\n\n```\nc", tidy("```\nx  \n\n\n```\nc"));
    }

    /// A one-page PDF with `text` in Helvetica, its cross-reference table computed.
    fn tiny_pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 18 Tf 72 720 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }
}

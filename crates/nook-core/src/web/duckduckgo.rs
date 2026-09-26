//! DuckDuckGo's HTML results page (html.duckduckgo.com), the one it serves to browsers without
//! JavaScript: no key and no account. It is made for people, not programs, so there is no contract;
//! the parser reads what the page shows (a result is a div.result holding a.result__a and
//! .result__snippet) and skips ads. When many searches come from one connection in a short time,
//! DuckDuckGo answers with a human check instead of results, which [`is_challenge`] spots.
//!
//! Ports `web/DuckDuckGo.java`.

use once_cell::sync::Lazy;
use scraper::{ElementRef, Html, Selector};
use url::Url;

use super::page_text::element_text;
use super::SearchResult;

pub(crate) const ENDPOINT: &str = "https://html.duckduckgo.com/html/";
pub(crate) const MAX_RESULTS: usize = 8;

static RESULT: Lazy<Selector> = Lazy::new(|| selector("div.result"));
static AD_BADGE: Lazy<Selector> = Lazy::new(|| selector(".badge--ad"));
static TITLE: Lazy<Selector> = Lazy::new(|| selector("a.result__a"));
static SNIPPET: Lazy<Selector> = Lazy::new(|| selector(".result__snippet"));

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("a valid selector")
}

/// The form body the page's own search box posts.
pub(crate) fn form(query: &str) -> String {
    format!(
        "q={}",
        url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>()
    )
}

pub(crate) fn parse(html: &str) -> Vec<SearchResult> {
    let doc = Html::parse_document(html);
    let mut out = Vec::new();
    for r in doc.select(&RESULT) {
        if has_class(r, "result--ad")
            || has_class(r, "badge--ad")
            || r.select(&AD_BADGE).next().is_some()
        {
            continue;
        }
        let Some(a) = r.select(&TITLE).next() else {
            continue;
        };
        let Some(url) = target(a.value().attr("href")) else {
            continue;
        };
        let snippet = r
            .select(&SNIPPET)
            .next()
            .map(|s| element_text(*s))
            .unwrap_or_default();
        out.push(SearchResult {
            title: element_text(*a).trim().to_string(),
            url,
            snippet: snippet.trim().to_string(),
        });
        if out.len() >= MAX_RESULTS {
            break;
        }
    }
    out
}

/// jsoup's `hasClass`, which ignores case.
fn has_class(e: ElementRef, class: &str) -> bool {
    e.value().classes().any(|c| c.eq_ignore_ascii_case(class))
}

/// DuckDuckGo's answer when it takes the searches for a bot's: HTTP 202 (or 403, 429) and an
/// "anomaly" page with a picture puzzle. A results page carries none of these markers.
pub(crate) fn is_challenge(status: u16, html: &str) -> bool {
    if status == 202 || status == 403 || status == 429 {
        return true;
    }
    html.contains("anomaly-modal")
        || html.contains("anomaly.js")
        || html.contains("bots use DuckDuckGo")
        || html.contains("challenge-form")
}

/// Where a result link leads: DuckDuckGo's own redirect (`//duckduckgo.com/l/?uddg=...`) undone.
/// None for an ad (its links go through y.js) and for anything but http and https.
pub(crate) fn target(href: Option<&str>) -> Option<String> {
    let h = href?.trim();
    if h.is_empty() {
        return None;
    }
    let h = if h.starts_with("//") {
        format!("https:{h}")
    } else {
        h.to_string()
    };
    let Ok(u) = Url::parse(&h) else {
        return web(&h).then_some(h);
    };
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    if host == "duckduckgo.com" || host.ends_with(".duckduckgo.com") {
        if u.path() != "/l/" {
            return None;
        }
        for kv in u.query()?.split('&') {
            if kv.starts_with("uddg=") {
                let (_, to) = url::form_urlencoded::parse(kv.as_bytes()).next()?;
                return web(&to).then(|| to.into_owned());
            }
        }
        return None;
    }
    web(&h).then_some(h)
}

fn web(url: &str) -> bool {
    let l = url.to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DuckDuckGo's HTML results page, in the shape it had on 2026-09-24.
    const RESULTS: &str = r#"<html><body><div class="serp__results"><div class="results">
<div class="result results_links results_links_deep result--ad ">
  <div class="links_main links_deep result__body">
    <h2 class="result__title"><a rel="nofollow" class="result__a" href="https://duckduckgo.com/y.js?ad_domain=example.com&amp;u3=x">Buy a JDK</a></h2>
    <a class="result__snippet" href="https://duckduckgo.com/y.js?ad_domain=example.com">Cheap</a>
  </div>
</div>
<div class="result results_links results_links_deep web-result ">
  <div class="links_main links_deep result__body">
    <h2 class="result__title">
      <a rel="nofollow" class="result__a" href="https://docs.gradle.org/current/userguide/toolchains.html">Toolchains for JVM projects - Gradle User Manual</a>
    </h2>
    <div class="result__extras"><div class="result__extras__url">
      <a class="result__url" href="https://docs.gradle.org/current/userguide/toolchains.html">docs.gradle.org/current/userguide/toolchains.html</a>
    </div></div>
    <a class="result__snippet" href="https://docs.gradle.org/current/userguide/toolchains.html">A <b>Java</b> <b>toolchain</b> is a set of tools.</a>
  </div>
</div>
<div class="result results_links results_links_deep web-result ">
  <div class="links_main links_deep result__body">
    <h2 class="result__title"><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fstackoverflow.com%2Fq%2F1%3Fa%3Db&amp;rut=abc">Question - Stack Overflow</a></h2>
  </div>
</div>
</div></div></body></html>"#;

    #[test]
    fn reads_results_skips_ads_and_undoes_the_redirect() {
        let r = parse(RESULTS);
        assert_eq!(2, r.len(), "{r:?}");
        assert_eq!(
            SearchResult::new(
                "Toolchains for JVM projects - Gradle User Manual",
                "https://docs.gradle.org/current/userguide/toolchains.html",
                "A Java toolchain is a set of tools."
            ),
            r[0]
        );
        assert_eq!(
            SearchResult::new(
                "Question - Stack Overflow",
                "https://stackoverflow.com/q/1?a=b",
                ""
            ),
            r[1]
        );
    }

    #[test]
    fn an_empty_page_has_no_results() {
        assert!(
            parse("<html><body><div class=\"no-results\">No results.</div></body></html>")
                .is_empty()
        );
    }

    #[test]
    fn at_most_eight_results() {
        let one = "<div class=\"result\"><a class=\"result__a\" href=\"https://example.com/N\">N</a></div>";
        let html = format!(
            "<html><body>{}</body></html>",
            (0..12)
                .map(|n| one.replace('N', &n.to_string()))
                .collect::<String>()
        );
        let r = parse(&html);
        assert_eq!(MAX_RESULTS, r.len());
        assert_eq!("https://example.com/7", r[7].url);
    }

    #[test]
    fn spots_the_human_check() {
        assert!(is_challenge(202, ""));
        assert!(is_challenge(429, ""));
        assert!(is_challenge(
            200,
            "<div class=\"anomaly-modal__title\">Unfortunately, bots use DuckDuckGo too.</div>"
        ));
        assert!(!is_challenge(200, RESULTS));
    }

    #[test]
    fn only_web_addresses_come_out() {
        assert_eq!(None, target(Some("javascript:alert(1)")));
        assert_eq!(
            None,
            target(Some("//duckduckgo.com/l/?uddg=javascript%3Aalert(1)"))
        );
        assert_eq!(
            None,
            target(Some("https://duckduckgo.com/y.js?ad_domain=x"))
        );
        assert_eq!(
            Some("https://example.com/a".to_string()),
            target(Some(" https://example.com/a "))
        );
    }

    #[test]
    fn the_query_is_form_encoded() {
        assert_eq!(
            "q=gradle+%22java+toolchain%22+%26+more",
            form("gradle \"java toolchain\" & more")
        );
    }
}

//! The web as Nook's worker reaches it: a search on DuckDuckGo's HTML page and a plain GET for a
//! page, both straight from this computer's own connection, with no key, account or service in
//! between. Only public addresses are read (never this computer, the local network or Nook's own
//! gateway), each redirect is checked like the address it came from, and a page is cut at
//! [`MAX_BYTES`] and [`TOTAL_TIME`]. Searches are spaced [`SEARCH_GAP`] apart so a busy turn does
//! not look like a bot; when DuckDuckGo asks for a human check anyway, searching pauses for
//! [`PAUSE`]. Nook waits it out and never tries to get past the check.
//!
//! The switch (Settings › General › Web access) is `<home>/web.json`; on unless turned off.
//!
//! Ports `web/WebAccess.java`. Beyond the original, the connection itself goes only to addresses
//! that pass the same public check (the HTTP client resolves names through it), so a name that
//! answers differently between the check and the connection gets nowhere.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Local};
use parking_lot::Mutex;
use reqwest::header::{
    HeaderValue, ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, LOCATION, USER_AGENT as USER_AGENT_HEADER,
};
use tokio::time::Instant;
use url::Url;

use super::{duckduckgo, page_text, Page, SearchResult, Web};
use crate::home::Home;

pub const MAX_BYTES: usize = 5 << 20;
pub const MAX_REDIRECTS: usize = 5;
pub const TOTAL_TIME: Duration = Duration::from_secs(30);
const CONNECT_TIME: Duration = Duration::from_secs(10);
pub const SEARCH_GAP: Duration = Duration::from_secs(2);
pub const PAUSE: Duration = Duration::from_secs(10 * 60);
/// Browser-shaped so sites serve their normal page, and saying who is asking.
#[cfg(not(target_os = "macos"))]
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Nook/0.3";
#[cfg(target_os = "macos")]
pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Nook/0.3";
const ACCEPT_TYPES: &str =
    "text/html,application/xhtml+xml,text/plain;q=0.9,application/json;q=0.8,application/pdf;q=0.8,*/*;q=0.5";

/// Host name to addresses; the system resolver outside tests.
pub(crate) trait Resolver: Send + Sync + 'static {
    fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>>;
}

impl<F> Resolver for F
where
    F: Fn(&str) -> io::Result<Vec<IpAddr>> + Send + Sync + 'static,
{
    fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        self(host)
    }
}

/// The operating system's resolver (`InetAddress.getAllByName` in the original): an address
/// literal as it is, a name looked up.
struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        let bare = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host);
        if let Ok(ip) = bare.parse::<IpAddr>() {
            return Ok(vec![ip]);
        }
        Ok((bare, 0).to_socket_addrs()?.map(|a| a.ip()).collect())
    }
}

pub struct WebAccess {
    settings: PathBuf,
    resolver: Arc<dyn Resolver>,
    client: reqwest::Client,
    /// DuckDuckGo's HTML endpoint; tests point it at a local fake.
    search_endpoint: Url,
    total_time: Duration,
    accept_language: HeaderValue,
    last_search: tokio::sync::Mutex<Option<Instant>>,
    /// Until when searching waits, with the wall-clock time the message names.
    paused_until: Mutex<Option<(Instant, DateTime<Local>)>>,
}

impl WebAccess {
    /// The worker's web over this computer's own connection, its switch at `<home>/web.json`.
    pub fn new(home: &Home) -> Result<WebAccess> {
        WebAccess::with_resolver(home.root().join("web.json"), Arc::new(SystemResolver))
    }

    /// The test seam of the original: names resolved by `resolver`, for the checks and for the
    /// connection alike.
    pub(crate) fn with_resolver(
        settings: PathBuf,
        resolver: Arc<dyn Resolver>,
    ) -> Result<WebAccess> {
        let client = client_builder()
            .dns_resolver(Arc::new(PublicOnly(resolver.clone())))
            .build()
            .context("Could not start the web client")?;
        Ok(WebAccess {
            settings,
            resolver,
            client,
            search_endpoint: Url::parse(duckduckgo::ENDPOINT)?,
            total_time: TOTAL_TIME,
            accept_language: accept_language(),
            last_search: tokio::sync::Mutex::new(None),
            paused_until: Mutex::new(None),
        })
    }

    // ------------------------------------------------------------------ the switch

    /// Whether Nook Code's worker gets the web tools. On until the person turns it off; off when
    /// the setting cannot be read.
    pub fn enabled(&self) -> bool {
        if !self.settings.is_file() {
            return true;
        }
        match std::fs::read(&self.settings)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| read_switch(&bytes))
        {
            Ok(on) => on,
            Err(e) => {
                tracing::warn!(
                    "Could not read {}; web access stays off: {e}",
                    self.settings.display()
                );
                false
            }
        }
    }

    pub fn set_enabled(&self, on: bool) -> Result<()> {
        let json = serde_json::to_vec_pretty(&serde_json::json!({ "enabled": on }))?;
        crate::settings::write_atomic(&self.settings, &json)
    }

    // ------------------------------------------------------------------ search

    fn paused(&self) -> Option<String> {
        match *self.paused_until.lock() {
            Some((until, wall)) if Instant::now() < until => Some(paused(wall)),
            _ => None,
        }
    }

    // ------------------------------------------------------------------ pages

    /// Refuses an address whose host is, or resolves to, this computer, its local network or any
    /// other address that is not on the public internet: a web page must not be able to point the
    /// worker at a router, a NAS or Nook's own gateway.
    pub(crate) async fn check_public(&self, uri: &Url) -> Result<()> {
        let host = uri.host_str().unwrap_or("").to_string();
        let resolver = self.resolver.clone();
        let name = host.clone();
        let all = tokio::task::spawn_blocking(move || resolver.resolve(&name)).await?;
        let all = match all {
            Ok(all) if !all.is_empty() => all,
            _ => bail!("no such host: {host}"),
        };
        for a in all {
            if !is_public(a) {
                bail!("{host} is not on the public internet ({a}); Nook reads public pages only");
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------ plumbing

    /// One exchange, the body cut at [`MAX_BYTES`] and the whole of it bounded by the total time:
    /// a timeout on the headers alone does not cover a body that trickles.
    async fn send(&self, req: reqwest::RequestBuilder, host: &str) -> Result<Exchange> {
        let req = req
            .header(USER_AGENT_HEADER, USER_AGENT)
            .header(ACCEPT, ACCEPT_TYPES)
            .header(ACCEPT_LANGUAGE, self.accept_language.clone());
        match tokio::time::timeout(self.total_time, exchange(req)).await {
            Err(_) => bail!(
                "{host} took longer than {} seconds",
                self.total_time.as_secs()
            ),
            Ok(Err(e)) => Err(failure(&e, host)),
            Ok(Ok(x)) => Ok(x),
        }
    }
}

#[async_trait]
impl Web for WebAccess {
    async fn search(&self, query: &str) -> Result<Vec<SearchResult>> {
        let q = query.trim();
        if q.is_empty() {
            bail!("the query is empty");
        }
        {
            let mut last = self.last_search.lock().await;
            if let Some(message) = self.paused() {
                bail!(message);
            }
            if let Some(at) = *last {
                tokio::time::sleep_until(at + SEARCH_GAP).await;
            }
            *last = Some(Instant::now());
        }
        let host = self.search_endpoint.host_str().unwrap_or("").to_string();
        let req = self
            .client
            .post(self.search_endpoint.clone())
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(duckduckgo::form(q));
        let r = self.send(req, &host).await?;
        let html = String::from_utf8_lossy(&r.body);
        if duckduckgo::is_challenge(r.status, &html) {
            let wall = Local::now() + chrono::Duration::from_std(PAUSE).unwrap_or_default();
            *self.paused_until.lock() = Some((Instant::now() + PAUSE, wall));
            tracing::info!(
                "DuckDuckGo asked for a human check (HTTP {}); web search is paused for {} minutes",
                r.status,
                PAUSE.as_secs() / 60
            );
            bail!(paused(wall));
        }
        if r.status != 200 {
            bail!("DuckDuckGo answered HTTP {}", r.status);
        }
        Ok(duckduckgo::parse(&html))
    }

    async fn fetch(&self, url: &str) -> Result<Page> {
        let mut uri = address(url)?;
        let mut hop = 0;
        loop {
            self.check_public(&uri).await?;
            let host = uri.host_str().unwrap_or("").to_string();
            let r = self.send(self.client.get(uri.clone()), &host).await?;
            let s = r.status;
            if s / 100 == 3 {
                if let Some(location) = &r.location {
                    if hop >= MAX_REDIRECTS {
                        bail!("the address redirects more than {MAX_REDIRECTS} times");
                    }
                    uri = redirect(&uri, location)?;
                    hop += 1;
                    continue;
                }
            }
            if s >= 400 {
                bail!("the page answered HTTP {s}{}", why(s));
            }
            if s != 200 && s != 203 {
                bail!("the page answered HTTP {s} with nothing to read");
            }
            let at = uri.to_string();
            return tokio::task::spawn_blocking(move || {
                page_text::of(&at, r.content_type.as_deref(), &r.body, r.cut)
            })
            .await
            .map_err(|e| anyhow!("could not read the page: {e}"))?;
        }
    }
}

fn paused(until: DateTime<Local>) -> String {
    format!(
        "web search is paused until {}: DuckDuckGo asked for a human check, which it does when many searches \
         come from one connection in a short time, and Nook waits rather than get past it",
        until.format("%H:%M")
    )
}

/// The switch as Jackson read it: `enabled` true unless it says false; an empty file is no setting.
fn read_switch(bytes: &[u8]) -> Result<bool> {
    let bytes = crate::settings::strip_bom(bytes);
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(true);
    }
    let v: serde_json::Value = serde_json::from_slice(bytes)?;
    Ok(match v.get("enabled") {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => s.trim() != "false",
        Some(serde_json::Value::Number(n)) => n.as_f64().is_none_or(|f| f as i64 != 0),
        _ => true,
    })
}

fn why(status: u16) -> &'static str {
    match status {
        401 | 403 => " (the site does not let this reader in)",
        404 | 410 => " (no such page)",
        429 => " (the site wants fewer requests; try later)",
        _ => "",
    }
}

/// A worker's address as a URL: http or https, no fragment, a space or two forgiven.
pub(crate) fn address(url: &str) -> Result<Url> {
    let u = page_text::without_fragment(url.trim());
    if u.is_empty() {
        bail!("the address is empty");
    }
    let uri = match Url::parse(&u.replace(' ', "%20")) {
        Ok(uri) => uri,
        Err(url::ParseError::RelativeUrlWithoutBase) => {
            bail!("only http and https addresses can be read")
        }
        Err(_) => bail!("not a web address: {url}"),
    };
    if uri.scheme() != "http" && uri.scheme() != "https" {
        bail!("only http and https addresses can be read");
    }
    if uri.host_str().is_none_or(str::is_empty) {
        bail!("the address has no host");
    }
    Ok(uri)
}

fn redirect(from: &Url, location: &str) -> Result<Url> {
    match from.join(&location.trim().replace(' ', "%20")) {
        Ok(to) => address(to.as_str()),
        Err(_) => bail!("the page redirects to an address that is not one: {location}"),
    }
}

pub(crate) fn is_public(a: IpAddr) -> bool {
    match a {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            // Java reads ::ffff:a.b.c.d as the IPv4 address it maps, and so does this.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let b = v6.octets();
            let link_local = b[0] == 0xfe && (b[1] & 0xc0) == 0x80;
            let site_local = b[0] == 0xfe && (b[1] & 0xc0) == 0xc0;
            if v6.is_unspecified()
                || v6.is_loopback()
                || link_local
                || site_local
                || v6.is_multicast()
            {
                return false;
            }
            if (b[0] & 0xfe) == 0xfc {
                return false; // fc00::/7, unique local
            }
            let nat64 = b[0] == 0
                && b[1] == 0x64
                && b[2] == 0xff
                && b[3] == 0x9b
                && b[4..12].iter().all(|x| *x == 0);
            let ipv4_compatible = b[..12].iter().all(|x| *x == 0);
            if nat64 || ipv4_compatible {
                return is_public_v4(Ipv4Addr::new(b[12], b[13], b[14], b[15]));
            }
            true
        }
    }
}

fn is_public_v4(a: Ipv4Addr) -> bool {
    if a.is_unspecified()
        || a.is_loopback()
        || a.is_link_local()
        || a.is_private()
        || a.is_multicast()
    {
        return false;
    }
    let [b0, b1, b2, _] = a.octets();
    if b0 == 0 {
        return false; // "this network"
    }
    if b0 == 100 && (b1 & 0xc0) == 64 {
        return false; // 100.64.0.0/10, carrier-grade NAT
    }
    if b0 == 192 && b1 == 0 && b2 == 0 {
        return false; // 192.0.0.0/24, protocol assignments
    }
    if b0 == 198 && (b1 & 0xfe) == 18 {
        return false; // 198.18.0.0/15, benchmarking
    }
    b0 < 240 // 240.0.0.0/4 reserved, and broadcast
}

/// The HTTP client every request goes through: no redirects followed (each hop is checked here
/// first), no proxy (the checks are about where the request really goes), ten seconds to connect.
fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIME)
        .no_proxy()
}

/// The language this computer is set to, then English, as a browser asks.
fn accept_language() -> HeaderValue {
    let tag = sys_locale::get_locale()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "en".to_string());
    HeaderValue::from_str(&format!("{tag},en;q=0.8"))
        .unwrap_or_else(|_| HeaderValue::from_static("en,en;q=0.8"))
}

/// Name resolution for the HTTP client: the same resolver as the checks, and a connection only to
/// addresses that pass them.
struct PublicOnly(Arc<dyn Resolver>);

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let resolver = self.0.clone();
        let host = name.as_str().to_string();
        Box::pin(async move {
            let lookup = host.clone();
            let all = tokio::task::spawn_blocking(move || resolver.resolve(&lookup)).await??;
            if let Some(a) = all.iter().find(|a| !is_public(**a)) {
                let refused = NotPublic(format!(
                    "{host} is not on the public internet ({a}); Nook reads public pages only"
                ));
                return Err(Box::new(refused) as Box<dyn std::error::Error + Send + Sync>);
            }
            let addrs: reqwest::dns::Addrs =
                Box::new(all.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

/// The client's own refusal of an address, told to the worker as the check would.
#[derive(Debug)]
struct NotPublic(String);

impl std::fmt::Display for NotPublic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotPublic {}

struct Exchange {
    status: u16,
    location: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
    cut: bool,
}

/// Sends and collects a body up to [`MAX_BYTES`], hanging up on the rest. A redirect's body is not
/// read. A compressed body (Nook accepts gzip, brotli and deflate) is cut after it is undone.
async fn exchange(req: reqwest::RequestBuilder) -> reqwest::Result<Exchange> {
    let mut resp = req.send().await?;
    let header = |name| {
        resp.headers()
            .get(name)
            .map(|v: &HeaderValue| String::from_utf8_lossy(v.as_bytes()).into_owned())
    };
    let status = resp.status().as_u16();
    let location = header(LOCATION);
    let content_type = header(CONTENT_TYPE);
    let mut body = Vec::new();
    let mut cut = false;
    if status / 100 != 3 {
        while let Some(chunk) = resp.chunk().await? {
            let room = MAX_BYTES - body.len();
            if chunk.len() > room {
                body.extend_from_slice(&chunk[..room]);
                cut = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }
    }
    Ok(Exchange {
        status,
        location,
        content_type,
        body,
        cut,
    })
}

/// What went wrong in an exchange, in the original's words.
fn failure(e: &reqwest::Error, host: &str) -> anyhow::Error {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(e);
    let mut innermost = e.to_string();
    while let Some(s) = source {
        if let Some(refused) = s.downcast_ref::<NotPublic>() {
            return anyhow!("{refused}");
        }
        innermost = s.to_string();
        source = s.source();
    }
    if e.is_connect() || e.is_timeout() {
        return anyhow!("could not connect to {host}");
    }
    anyhow!("{host}: {innermost}")
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener as StdListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::{Body, Bytes};
    use axum::http::{header, HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::Router;

    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn only_public_addresses_are_public() {
        for inside in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.1",
            "64:ff9b::a00:1",
            "192.0.0.8",
            "198.18.0.1",
            "::",
            "fec0::1",
            "ff02::1",
            "::10.0.0.1",
        ] {
            assert!(!is_public(ip(inside)), "{inside}");
        }
        for outside in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "2606:4700:4700::1111",
            "64:ff9b::808:808",
            "::ffff:8.8.8.8",
        ] {
            assert!(is_public(ip(outside)), "{outside}");
        }
    }

    fn fake_resolver() -> Arc<dyn Resolver> {
        Arc::new(|host: &str| -> io::Result<Vec<IpAddr>> {
            if let Ok(literal) = host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
            {
                return Ok(vec![literal]);
            }
            Ok(match host {
                "intranet.example" => vec![ip("93.184.216.34"), ip("10.0.0.5")],
                "localhost" => vec![ip("127.0.0.1")],
                "nowhere.example" => {
                    return Err(io::Error::new(io::ErrorKind::NotFound, "no such host"))
                }
                _ => vec![ip("93.184.216.34")],
            })
        })
    }

    #[tokio::test]
    async fn a_host_that_resolves_inside_is_refused_before_any_request() {
        let home = tempfile::tempdir().unwrap();
        let web = WebAccess::with_resolver(home.path().join("web.json"), fake_resolver()).unwrap();
        let e = web
            .fetch("http://localhost:41434/runtime/status")
            .await
            .unwrap_err();
        assert!(e.to_string().contains("not on the public internet"), "{e}");
        assert!(
            web.check_public(&Url::parse("https://intranet.example/wiki").unwrap())
                .await
                .is_err(),
            "one inside address is enough"
        );
        assert!(web
            .check_public(&Url::parse("https://docs.example/guide").unwrap())
            .await
            .is_ok());
        let e = web.fetch("https://nowhere.example/").await.unwrap_err();
        assert_eq!("no such host: nowhere.example", e.to_string());
        let e = web.fetch("http://[::1]:41434/").await.unwrap_err();
        assert!(e.to_string().contains("not on the public internet"), "{e}");
    }

    #[tokio::test]
    async fn the_connection_goes_only_where_the_check_allows() {
        // A name that answers a public address to the check and this computer to the connection
        // (DNS rebinding) is refused by the client's own resolution, before anything connects.
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let rebinding = Arc::new(move |_: &str| -> io::Result<Vec<IpAddr>> {
            Ok(if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![ip("93.184.216.34")]
            } else {
                vec![ip("127.0.0.1")]
            })
        });
        let home = tempfile::tempdir().unwrap();
        let web = WebAccess::with_resolver(home.path().join("web.json"), rebinding).unwrap();
        let e = web.fetch("http://rebind.example:41434/").await.unwrap_err();
        assert_eq!(
            "rebind.example is not on the public internet (127.0.0.1); Nook reads public pages only",
            e.to_string()
        );
        assert_eq!(2, calls.load(Ordering::SeqCst));
    }

    #[test]
    fn only_http_and_https_addresses() {
        for bad in [
            "file:///C:/Windows/win.ini",
            "ftp://example.com/a",
            "example.com/a",
            "",
            "https://",
        ] {
            assert!(address(bad).is_err(), "{bad}");
        }
        assert_eq!(
            "https://example.com/a%20b?x=1",
            address(" https://example.com/a b?x=1#part ")
                .unwrap()
                .to_string()
        );
        assert_eq!(
            "the address is empty",
            address("  ").unwrap_err().to_string()
        );
        assert_eq!(
            "only http and https addresses can be read",
            address("ftp://example.com/a").unwrap_err().to_string()
        );
    }

    #[test]
    fn the_switch_is_on_until_turned_off_and_off_when_unreadable() {
        let home = tempfile::tempdir().unwrap();
        let file = home.path().join("web.json");
        let web = WebAccess::with_resolver(file.clone(), fake_resolver()).unwrap();
        assert!(web.enabled());
        web.set_enabled(false).unwrap();
        assert!(!web.enabled());
        web.set_enabled(true).unwrap();
        assert!(web.enabled());
        std::fs::write(&file, "{not json").unwrap();
        assert!(
            !web.enabled(),
            "a setting that cannot be read keeps the web off"
        );
        std::fs::write(&file, "{\"enabled\": \"false\"}").unwrap();
        assert!(!web.enabled());
        std::fs::write(&file, "").unwrap();
        assert!(web.enabled(), "an empty file is no setting");
    }

    // ------------------------------------------------------------------ against a local fake web

    /// Refuses every name: in these tests a connection goes only where a test sends it.
    struct NoDns;

    impl reqwest::dns::Resolve for NoDns {
        fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
            let host = name.as_str().to_string();
            Box::pin(async move { Err(format!("tests resolve no names ({host})").into()) })
        }
    }

    async fn serve(router: Router) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        addr
    }

    /// A WebAccess whose checks see public addresses (the fake resolver) while its connections go
    /// to local servers: the safety checks run as in production, only the wire is redirected.
    fn local(
        dir: &std::path::Path,
        hosts: &[(&str, SocketAddr)],
        total_time: Duration,
    ) -> WebAccess {
        let mut builder = client_builder().dns_resolver(Arc::new(NoDns));
        for (host, addr) in hosts {
            builder = builder.resolve(host, *addr);
        }
        let search = hosts
            .iter()
            .find(|(h, _)| *h == "search.example")
            .map(|(_, a)| a.port())
            .unwrap_or(9);
        WebAccess {
            settings: dir.join("web.json"),
            resolver: fake_resolver(),
            client: builder.build().unwrap(),
            search_endpoint: Url::parse(&format!("http://search.example:{search}/html/")).unwrap(),
            total_time,
            accept_language: accept_language(),
            last_search: tokio::sync::Mutex::new(None),
            paused_until: Mutex::new(None),
        }
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(bytes).unwrap();
        z.finish().unwrap()
    }

    fn pages() -> Router {
        let html = [(header::CONTENT_TYPE, "text/html; charset=utf-8")];
        Router::new()
            .route(
                "/guide",
                get(move |headers: HeaderMap| async move {
                    let agent = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                    let page = format!(
                        "<html><head><title>Guide</title></head><body><p>Read <a href=\"/api\">the API</a>. Agent: {agent}</p></body></html>"
                    );
                    (html, page)
                }),
            )
            .route("/moved", get(|| async { (StatusCode::FOUND, [(header::LOCATION, "/guide")]) }))
            .route("/loop", get(|| async { (StatusCode::FOUND, [(header::LOCATION, "/loop")]) }))
            .route(
                "/inside",
                get(|| async { (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, "http://localhost:41434/runtime/status")]) }),
            )
            .route("/missing", get(|| async { StatusCode::NOT_FOUND }))
            .route("/private", get(|| async { StatusCode::FORBIDDEN }))
            .route("/empty", get(|| async { StatusCode::NO_CONTENT }))
            .route("/unchanged", get(|| async { StatusCode::NOT_MODIFIED }))
            .route("/big", get(|| async { ([(header::CONTENT_TYPE, "text/plain")], vec![b'a'; MAX_BYTES + 1024 * 1024]) }))
            .route("/exact", get(|| async { ([(header::CONTENT_TYPE, "text/plain")], vec![b'b'; MAX_BYTES]) }))
            .route(
                "/zipped",
                get(|| async {
                    let body = gzip(b"<html><body><h1>Packed</h1><p>Undone by the reader.</p></body></html>");
                    ([(header::CONTENT_TYPE, "text/html"), (header::CONTENT_ENCODING, "gzip")], body)
                }),
            )
            .route(
                "/latin",
                get(|| async {
                    ([(header::CONTENT_TYPE, "text/html; charset=ISO-8859-1")], b"<p>caf\xE9</p>".to_vec())
                }),
            )
            .route("/image", get(|| async { ([(header::CONTENT_TYPE, "image/png")], vec![0x89u8, b'P', b'N', b'G']) }))
            .route(
                "/slow",
                get(|| async {
                    let trickle = futures::stream::unfold(0, |n| async move {
                        if n > 0 {
                            tokio::time::sleep(Duration::from_secs(30)).await;
                        }
                        Some((Ok::<_, io::Error>(Bytes::from_static(b"<html><body>")), n + 1))
                    });
                    ([(header::CONTENT_TYPE, "text/html")], Body::from_stream(trickle)).into_response()
                }),
            )
    }

    #[tokio::test]
    async fn reads_a_page_and_follows_redirects_checking_each_hop() {
        let dir = tempfile::tempdir().unwrap();
        let server = serve(pages()).await;
        let web = local(dir.path(), &[("docs.example", server)], TOTAL_TIME);
        let at = |path: &str| format!("http://docs.example:{}{path}", server.port());

        let p = web.fetch(&at("/guide#top")).await.unwrap();
        assert_eq!(at("/guide"), p.url);
        assert_eq!("Guide", p.title);
        assert!(p.text.starts_with("Read the API [1]."), "{}", p.text);
        assert!(
            p.text.contains(USER_AGENT),
            "the reader says who it is: {}",
            p.text
        );
        assert_eq!(vec![at("/api")], p.links);

        let moved = web.fetch(&at("/moved")).await.unwrap();
        assert_eq!(
            at("/guide"),
            moved.url,
            "the page is where the redirect led"
        );

        let e = web.fetch(&at("/loop")).await.unwrap_err();
        assert_eq!("the address redirects more than 5 times", e.to_string());
        let e = web.fetch(&at("/inside")).await.unwrap_err();
        assert_eq!(
            "localhost is not on the public internet (127.0.0.1); Nook reads public pages only",
            e.to_string()
        );
    }

    #[tokio::test]
    async fn statuses_say_why_there_is_nothing_to_read() {
        let dir = tempfile::tempdir().unwrap();
        let server = serve(pages()).await;
        let web = local(dir.path(), &[("docs.example", server)], TOTAL_TIME);
        let at = |path: &str| format!("http://docs.example:{}{path}", server.port());
        let err = |e: anyhow::Error| e.to_string();

        assert_eq!(
            "the page answered HTTP 404 (no such page)",
            err(web.fetch(&at("/missing")).await.unwrap_err())
        );
        assert_eq!(
            "the page answered HTTP 403 (the site does not let this reader in)",
            err(web.fetch(&at("/private")).await.unwrap_err())
        );
        assert_eq!(
            "the page answered HTTP 204 with nothing to read",
            err(web.fetch(&at("/empty")).await.unwrap_err())
        );
        assert_eq!(
            "the page answered HTTP 304 with nothing to read",
            err(web.fetch(&at("/unchanged")).await.unwrap_err())
        );
        assert_eq!(
            "that address is not a page Nook can read (image/png)",
            err(web.fetch(&at("/image")).await.unwrap_err())
        );
    }

    #[tokio::test]
    async fn bodies_are_cut_at_the_limit_decoded_and_read_in_their_charset() {
        let dir = tempfile::tempdir().unwrap();
        let server = serve(pages()).await;
        let web = local(dir.path(), &[("docs.example", server)], TOTAL_TIME);
        let at = |path: &str| format!("http://docs.example:{}{path}", server.port());

        let big = web
            .send(web.client.get(at("/big")), "docs.example")
            .await
            .unwrap();
        assert_eq!((MAX_BYTES, true), (big.body.len(), big.cut));
        let exact = web
            .send(web.client.get(at("/exact")), "docs.example")
            .await
            .unwrap();
        assert_eq!(
            (MAX_BYTES, false),
            (exact.body.len(), exact.cut),
            "a body of exactly the limit is whole"
        );
        let page = web.fetch(&at("/big")).await.unwrap();
        assert!(page.cut);
        assert_eq!(page_text::MAX_CHARS, page.text.len());

        let zipped = web.fetch(&at("/zipped")).await.unwrap();
        assert_eq!("# Packed\n\nUndone by the reader.", zipped.text);
        assert_eq!("café", web.fetch(&at("/latin")).await.unwrap().text);
    }

    #[tokio::test]
    async fn a_page_that_trickles_is_stopped_and_a_closed_port_is_said_so() {
        let dir = tempfile::tempdir().unwrap();
        let server = serve(pages()).await;
        let closed = {
            let l = StdListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        let web = local(
            dir.path(),
            &[("docs.example", server)],
            Duration::from_secs(1),
        );
        let e = web
            .fetch(&format!("http://docs.example:{}/slow", server.port()))
            .await
            .unwrap_err();
        assert_eq!("docs.example took longer than 1 seconds", e.to_string());

        // Windows retries a refused connection for about two seconds before it gives up.
        let web = local(dir.path(), &[("closed.example", closed)], TOTAL_TIME);
        let e = web
            .fetch(&format!("http://closed.example:{}/", closed.port()))
            .await
            .unwrap_err();
        assert_eq!("could not connect to closed.example", e.to_string());
    }

    const RESULTS: &str = r#"<html><body>
        <div class="result"><h2><a class="result__a" href="https://docs.gradle.org/current/userguide/toolchains.html">Toolchains</a></h2>
        <a class="result__snippet" href="https://docs.gradle.org/">A <b>Java</b> toolchain.</a></div></body></html>"#;

    #[tokio::test]
    async fn searches_post_the_form_and_are_spaced_apart() {
        let dir = tempfile::tempdir().unwrap();
        let seen = Arc::new(parking_lot::Mutex::new(
            Vec::<(String, String, Instant)>::new(),
        ));
        let log = seen.clone();
        let server = serve(Router::new().route(
            "/html/",
            post(move |headers: HeaderMap, body: String| {
                let log = log.clone();
                async move {
                    let kind = headers
                        .get(header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    log.lock().push((kind, body, Instant::now()));
                    ([(header::CONTENT_TYPE, "text/html")], RESULTS)
                }
            }),
        ))
        .await;
        let web = local(dir.path(), &[("search.example", server)], TOTAL_TIME);

        let r = web.search("  gradle toolchain ").await.unwrap();
        assert_eq!(
            vec![SearchResult::new(
                "Toolchains",
                "https://docs.gradle.org/current/userguide/toolchains.html",
                "A Java toolchain."
            )],
            r
        );
        web.search("second").await.unwrap();
        let seen = seen.lock().clone();
        assert_eq!("application/x-www-form-urlencoded", seen[0].0);
        assert_eq!("q=gradle+toolchain", seen[0].1);
        let gap = seen[1].2 - seen[0].2;
        assert!(
            gap >= SEARCH_GAP - Duration::from_millis(50),
            "searches {gap:?} apart"
        );
        assert_eq!(
            "the query is empty",
            web.search("   ").await.unwrap_err().to_string()
        );
    }

    #[tokio::test]
    async fn a_human_check_pauses_searching_without_asking_again() {
        let dir = tempfile::tempdir().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        let server = serve(Router::new().route(
            "/html/",
            post(move || {
                let counted = counted.clone();
                async move {
                    counted.fetch_add(1, Ordering::SeqCst);
                    (StatusCode::ACCEPTED, "<div class=\"anomaly-modal__title\">Unfortunately, bots use DuckDuckGo too.</div>")
                }
            }),
        ))
        .await;
        let web = local(dir.path(), &[("search.example", server)], TOTAL_TIME);

        let first = web.search("gradle").await.unwrap_err().to_string();
        assert!(first.starts_with("web search is paused until "), "{first}");
        assert!(
            first.ends_with("and Nook waits rather than get past it"),
            "{first}"
        );
        let again = web.search("gradle").await.unwrap_err().to_string();
        assert_eq!(first, again);
        assert_eq!(
            1,
            hits.load(Ordering::SeqCst),
            "the paused search never left the machine"
        );
    }

    #[tokio::test]
    async fn a_failing_search_says_what_duckduckgo_answered() {
        let dir = tempfile::tempdir().unwrap();
        let server = serve(Router::new().route(
            "/html/",
            post(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "oops") }),
        ))
        .await;
        let web = local(dir.path(), &[("search.example", server)], TOTAL_TIME);
        assert_eq!(
            "DuckDuckGo answered HTTP 500",
            web.search("gradle").await.unwrap_err().to_string()
        );
    }
}

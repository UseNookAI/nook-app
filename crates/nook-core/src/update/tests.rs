//! Ports `UpdateSourceTest.java` and `VersionUpdateServiceTest.kt`, plus the updater's download,
//! verify, install and cancel paths over a folder feed and a local HTTP server.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use ed25519_dalek::{SigningKey, VerifyingKey};
use parking_lot::Mutex;

use super::manifest::{self, Release};
use super::source::{self, choose_base, to_url, ChannelStore, UpdateSource};
use super::updater::{due_for_check, wait_note, Updater};
use crate::build_info::{compare_versions, BuildInfo};
use crate::busy::BusyWork;
use crate::settings::Settings;

/// A throwaway key's output from tools/release.py; the app's verification must agree with the
/// script's signing (and so with nook-release, which writes the same bytes).
const PYTHON_KEY: &str = "NFhvGZ0EX-AjPBbLmmWRXkbYpQUfwO_79Z2sS_aJDsc";
const PYTHON_SIG: &str =
    "g-nzLW3mrpXusnwMzkf8fn4xjUDAiiEGUz9T5-MBHsTPlsNDiFmO8gmdYX6UzLJeoJ81N1kqVXOYlykUFtFtCg";
const PYTHON_BODY_B64: &str = "ewogICJjaGFubmVsIjogInN0YWJsZSIsCiAgInZlcnNpb24iOiAiMC4zLjEiLAogICJmaWxlIjogIk5vb2stMC4zLjEuZXhlIiwKICAidXJsIjogImh0dHBzOi8vZGwuZXhhbXBsZS9Ob29rLTAuMy4xLmV4ZSIsCiAgInNoYTI1NiI6ICJhYmFiYWJhYmFiYWJhYmFiYWJhYmFiYWJhYmFiYWJhYmFiYWJhYmFiYWJhYmFiYWJhYmFiYWJhYmFiYWJhYmFiIiwKICAic2l6ZSI6IDEyMzQsCiAgImNvbW1pdCI6ICJhYmMxMjM0IiwKICAibm90ZXMiOiAiZml4dHVyZSIsCiAgInB1Ymxpc2hlZCI6ICIyMDI2LTA5LTIyVDEyOjAwOjAwWiIKfQo";

/// https://dl.usenook.ai/nook/stable/latest.json and its .sig as the host served them on
/// 2026-09-24: the original Nook 0.3.0, signed with the original Nook's key.
const HOST_KEY: &str = "Q0sCjPkBA0lYpSRxhdXKEg9OFx7J9M-gxW0b7YBBXWg";
const HOST_SIG: &str =
    "XnP65WxDh5xpYEhrkANavYZkmY6b0oSvDTVfi7tA0yQOJkvjKdk3U7US5CXiz1B2MlHIfLUbnQt30ODsfSjbBg";
const HOST_BODY_B64: &str = "ewogICJjaGFubmVsIjogInN0YWJsZSIsCiAgInZlcnNpb24iOiAiMC4zLjAiLAogICJmaWxlIjogIk5vb2stMC4zLjAuZXhlIiwKICAidXJsIjogImh0dHBzOi8vZGwudXNlbm9vay5haS9ub29rL2J1aWxkcy8wLjMuMC0yZjBiZmJjL05vb2stMC4zLjAuZXhlIiwKICAic2hhMjU2IjogIjc1N2JmZWFlZDQ2NTdiMzNkYzY1NmQwMDViNjdiZjRiYzRkNmU1MmRhNjBlZmYzMTMzZDU3YjI3YTRhNDkzMjIiLAogICJzaXplIjogMjQ4MzY0MjM1LAogICJjb21taXQiOiAiMmYwYmZiYyIsCiAgIm5vdGVzIjogIlRoZSAwLjMuMCBidWlsZC4iLAogICJwdWJsaXNoZWQiOiAiMjAyNi0wOS0yMlQxMTo1ODowNFoiCn0K";

fn time(iso: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(iso)
        .unwrap()
        .with_timezone(&Utc)
}

fn this_build() -> BuildInfo {
    BuildInfo {
        version: "0.3.0".into(),
        commit: "0efb5a2".into(),
        time: Some(time("2026-09-22T10:00:00Z")),
    }
}

fn new_key() -> SigningKey {
    SigningKey::from_bytes(&rand::random::<[u8; 32]>())
}

/// The key as the key file carries it, read back: what the app would hold.
fn public(key: &SigningKey) -> VerifyingKey {
    manifest::public_key(&manifest::encode_public_key(&key.verifying_key())).unwrap()
}

fn b64(text: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .unwrap()
}

/// Where a channel's manifest is for this system: the channel's own folder for Windows, its
/// `macos-arm64` folder on a Mac.
fn channel_dir(channel: &str) -> String {
    match super::source::PLATFORM_DIR {
        Some(platform) => format!("{channel}/{platform}"),
        None => channel.to_string(),
    }
}

/// Writes a signed manifest for a channel into the folder that stands in for the download host.
fn publish(root: &Path, key: &SigningKey, channel: &str, json: &str) -> PathBuf {
    let dir = root.join(channel_dir(channel));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("latest.json"), json).unwrap();
    std::fs::write(
        dir.join("latest.json.sig"),
        manifest::sign(key, json.as_bytes()) + "\n",
    )
    .unwrap();
    dir.join("latest.json")
}

fn manifest_json(
    channel: &str,
    version: &str,
    commit: &str,
    published: &str,
    sha256: &str,
    size: u64,
) -> String {
    manifest_with_url(
        channel,
        version,
        commit,
        published,
        sha256,
        size,
        &format!("https://dl.example/Nook-{version}.exe"),
    )
}

fn manifest_with_url(
    channel: &str,
    version: &str,
    commit: &str,
    published: &str,
    sha256: &str,
    size: u64,
    url: &str,
) -> String {
    format!(
        "{{\"channel\":\"{channel}\",\"version\":\"{version}\",\"file\":\"Nook-{version}.exe\",\"url\":\"{url}\",\
         \"sha256\":\"{sha256}\",\"size\":{size},\"commit\":\"{commit}\",\"notes\":\"n\",\"published\":\"{published}\"}}"
    )
}

fn source_at(root: &Path, key: VerifyingKey) -> UpdateSource {
    UpdateSource::new(
        &to_url(&root.to_string_lossy()),
        vec![key],
        this_build(),
        None,
    )
}

fn ab() -> String {
    "ab".repeat(32)
}

// ------------------------------------------------------------------ UpdateSourceTest

#[tokio::test]
async fn the_stable_channel_offers_a_higher_version_and_nothing_else() {
    let host = tempfile::tempdir().unwrap();
    let key = new_key();
    let s = source_at(host.path(), public(&key));
    assert!(s.enabled());
    assert_eq!(
        s.manifest_url("stable"),
        format!(
            "{}/{}/latest.json",
            to_url(&host.path().to_string_lossy()).trim_end_matches('/'),
            channel_dir("stable")
        )
    );

    // nothing published yet: no update, and the reason is kept
    assert!(s.check("stable").await.is_none());
    assert!(s.last_error().is_some());

    publish(
        host.path(),
        &key,
        "stable",
        &manifest_json(
            "stable",
            "0.3.0",
            "0efb5a2",
            "2026-09-22T09:00:00Z",
            &ab(),
            10,
        ),
    );
    assert!(
        s.check("stable").await.is_none(),
        "the same version is not an update"
    );
    assert_eq!(s.last_error(), None, "nothing wrong, nothing newer");

    publish(
        host.path(),
        &key,
        "stable",
        &manifest_json(
            "stable",
            "0.3.1",
            "abc1234",
            "2026-09-23T09:00:00Z",
            &ab(),
            10,
        ),
    );
    let r = s.check("stable").await.expect("an update");
    assert_eq!(r.version, "0.3.1");
    assert_eq!(r.title(), "0.3.1 (abc1234)");
    assert_eq!(r.published, Some(time("2026-09-23T09:00:00Z")));

    // a newer dev build of the same version means nothing to the stable channel
    publish(
        host.path(),
        &key,
        "stable",
        &manifest_json(
            "stable",
            "0.3.0",
            "abc1234",
            "2026-09-23T09:00:00Z",
            &ab(),
            10,
        ),
    );
    assert!(s.check("stable").await.is_none());
}

#[tokio::test]
async fn the_dev_channel_takes_every_later_build_of_main() {
    let host = tempfile::tempdir().unwrap();
    let key = new_key();
    let s = source_at(host.path(), public(&key));

    publish(
        host.path(),
        &key,
        "dev",
        &manifest_json("dev", "0.3.0", "abc1234", "2026-09-22T11:00:00Z", &ab(), 10),
    );
    assert!(
        s.check("dev").await.is_some(),
        "same version, another commit, published after this build"
    );
    publish(
        host.path(),
        &key,
        "dev",
        &manifest_json("dev", "0.3.0", "0efb5a2", "2026-09-22T11:00:00Z", &ab(), 10),
    );
    assert!(
        s.check("dev").await.is_none(),
        "this very commit is not an update"
    );
    publish(
        host.path(),
        &key,
        "dev",
        &manifest_json("dev", "0.3.0", "abc1234", "2026-09-22T09:00:00Z", &ab(), 10),
    );
    assert!(
        s.check("dev").await.is_none(),
        "published before this build was made: older, whatever the commit"
    );
    publish(
        host.path(),
        &key,
        "dev",
        &manifest_json("dev", "0.2.9", "abc1234", "2026-09-23T09:00:00Z", &ab(), 10),
    );
    assert!(
        s.check("dev").await.is_none(),
        "a lower version is never an update"
    );
    // a manifest for the other channel served under this one is refused
    publish(
        host.path(),
        &key,
        "dev",
        &manifest_json(
            "stable",
            "0.9.0",
            "abc1234",
            "2026-09-23T09:00:00Z",
            &ab(),
            10,
        ),
    );
    assert!(s.check("dev").await.is_none());
    let error = s.last_error().unwrap();
    assert!(error.contains("stable channel"), "{error}");
}

#[tokio::test]
async fn an_unsigned_or_tampered_manifest_is_no_manifest() {
    let host = tempfile::tempdir().unwrap();
    let key = new_key();
    let stranger = new_key();
    let s = source_at(host.path(), public(&key));

    // signed by someone else
    publish(
        host.path(),
        &stranger,
        "stable",
        &manifest_json(
            "stable",
            "9.9.9",
            "abc1234",
            "2026-09-23T09:00:00Z",
            &ab(),
            10,
        ),
    );
    assert!(s.check("stable").await.is_none());
    assert!(
        s.last_error().unwrap().contains("not signed by Nook"),
        "{:?}",
        s.last_error()
    );

    // signed by us, then a byte changed
    let file = publish(
        host.path(),
        &key,
        "stable",
        &manifest_json(
            "stable",
            "9.9.9",
            "abc1234",
            "2026-09-23T09:00:00Z",
            &ab(),
            10,
        ),
    );
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, text.replace("9.9.9", "9.9.8")).unwrap();
    assert!(s.check("stable").await.is_none());
    assert!(
        s.last_error().unwrap().contains("not signed by Nook"),
        "{:?}",
        s.last_error()
    );

    // signed, but not the shape of a manifest
    let junk = b"{\"version\":\"9.9.9\"}";
    std::fs::write(&file, junk).unwrap();
    std::fs::write(
        file.with_file_name("latest.json.sig"),
        manifest::sign(&key, junk),
    )
    .unwrap();
    assert!(s.check("stable").await.is_none());
    assert!(
        s.last_error().unwrap().contains("expected form"),
        "{:?}",
        s.last_error()
    );

    // no signature file at all
    std::fs::remove_file(file.with_file_name("latest.json.sig")).unwrap();
    assert!(s.check("stable").await.is_none());
    assert!(
        s.last_error().unwrap().contains("could not read"),
        "{:?}",
        s.last_error()
    );

    // a blank base: never checks
    let off = UpdateSource::new("", vec![public(&key)], this_build(), None);
    assert!(!off.enabled());
    assert!(off.check("stable").await.is_none());
    assert_eq!(
        choose_base(Some("https://y"), Some("https://x")),
        "https://x",
        "the environment wins"
    );
    assert_eq!(choose_base(Some("https://y"), Some(" ")), "https://y");
}

#[tokio::test]
async fn a_folder_is_a_base_as_well_as_a_url() {
    let host = tempfile::tempdir().unwrap();
    let folder = host.path().to_string_lossy().to_string();
    // a base can be a plain folder, as a test feed is
    let url = to_url(&folder);
    assert!(url.starts_with("file:///") && url.ends_with('/'), "{url}");
    assert_eq!(to_url("https://dl.example/nook"), "https://dl.example/nook");
    assert_eq!(to_url("file:///C:/feed"), "file:///C:/feed");
    assert_eq!(
        choose_base(Some(""), None),
        "",
        "blank stays blank: never checks"
    );
    assert_eq!(choose_base(None, Some("")), "");
    assert_eq!(
        choose_base(Some("https://y"), Some(&folder)),
        url,
        "the environment wins, as a folder too"
    );

    let key = new_key();
    let s = UpdateSource::new(
        &choose_base(Some(&folder), None),
        vec![public(&key)],
        this_build(),
        None,
    );
    publish(
        host.path(),
        &key,
        "stable",
        &manifest_json(
            "stable",
            "0.3.1",
            "abc1234",
            "2026-09-23T09:00:00Z",
            &ab(),
            10,
        ),
    );
    assert_eq!(s.check("stable").await.unwrap().version, "0.3.1");
}

#[test]
fn a_folder_with_a_space_is_still_a_feed() {
    let root = tempfile::tempdir().unwrap();
    let feed = root.path().join("Nook updates");
    std::fs::create_dir_all(&feed).unwrap();
    let url = to_url(&feed.to_string_lossy());
    assert!(url.contains("Nook%20updates"), "{url}");
    let back = url::Url::parse(&url).unwrap().to_file_path().unwrap();
    assert_eq!(
        std::fs::canonicalize(back).unwrap(),
        std::fs::canonicalize(&feed).unwrap()
    );
}

#[test]
fn only_a_feed_on_this_machine_is_local() {
    // a local feed is read every minute, as the dev channel is, and stable on a web host every
    // fifteen (the updater)
    let host = tempfile::tempdir().unwrap();
    let local = UpdateSource::new(
        &choose_base(Some(&host.path().to_string_lossy()), None),
        vec![],
        this_build(),
        None,
    );
    assert!(local.local());
    assert!(UpdateSource::new("FILE:///C:/feed", vec![], this_build(), None).local());
    assert!(!UpdateSource::new("https://dl.example/nook", vec![], this_build(), None).local());
    assert!(!UpdateSource::new("", vec![], this_build(), None).local());
}

#[test]
fn the_download_must_be_what_the_manifest_named() {
    let host = tempfile::tempdir().unwrap();
    let installer = host.path().join("Nook-0.3.1.exe");
    std::fs::write(&installer, "MZ this is the installer").unwrap();
    let sha = source::sha256(&installer).unwrap();
    let size = std::fs::metadata(&installer).unwrap().len();

    let good = manifest::parse(
        manifest_json(
            "stable",
            "0.3.1",
            "abc1234",
            "2026-09-23T09:00:00Z",
            &sha,
            size,
        )
        .as_bytes(),
    )
    .unwrap();
    source::verify_download(&installer, &good).unwrap();
    assert!(installer.exists());

    let wrong_size = manifest::parse(
        manifest_json(
            "stable",
            "0.3.1",
            "abc1234",
            "2026-09-23T09:00:00Z",
            &sha,
            size + 1,
        )
        .as_bytes(),
    )
    .unwrap();
    let e = source::verify_download(&installer, &wrong_size)
        .unwrap_err()
        .to_string();
    assert!(e.contains("bytes"), "{e}");
    assert!(!installer.exists(), "a wrong download is deleted");

    std::fs::write(&installer, "MZ this is the installer").unwrap();
    let wrong_hash = manifest::parse(
        manifest_json(
            "stable",
            "0.3.1",
            "abc1234",
            "2026-09-23T09:00:00Z",
            &"cd".repeat(32),
            size,
        )
        .as_bytes(),
    )
    .unwrap();
    let e = source::verify_download(&installer, &wrong_hash)
        .unwrap_err()
        .to_string();
    assert!(e.contains("sha256"), "{e}");
    assert!(e.contains(&sha[..12]) && e.contains("cdcdcdcdcdcd…"), "{e}");
    assert!(!installer.exists());
}

#[test]
fn the_publishing_scripts_manifest_verifies_here() {
    let body = b64(PYTHON_BODY_B64);
    let keys = vec![manifest::public_key(PYTHON_KEY).unwrap()];
    assert!(
        manifest::verify(&body, PYTHON_SIG, &keys),
        "tools/release.py and the app agree"
    );
    assert!(
        !manifest::verify(&body, PYTHON_SIG, &manifest::bundled_keys().unwrap()),
        "the shipped key did not sign the fixture"
    );
    let r = manifest::parse(&body).unwrap();
    assert_eq!(r.version, "0.3.1");
    assert_eq!(r.commit, "abc1234");
    assert_eq!(r.size, 1234);
    assert_eq!(r.notes, "fixture");
    assert!(
        !manifest::bundled_keys().unwrap().is_empty(),
        "release-keys.txt carries the release keys"
    );
}

#[test]
fn the_hosts_manifest_verifies_with_the_bundled_keys() {
    // The verification is the original's, byte for byte: a manifest the download host served,
    // signed with the original key, checks out against that key and against the bundled keys, which
    // carry it because this app now publishes to that host for every Nook.
    let body = b64(HOST_BODY_B64);
    assert!(manifest::verify(
        &body,
        HOST_SIG,
        &[manifest::public_key(HOST_KEY).unwrap()]
    ));
    assert!(
        manifest::verify(&body, HOST_SIG, &manifest::bundled_keys().unwrap()),
        "the bundled keys believe the download host"
    );
    let r = manifest::parse(&body).unwrap();
    assert_eq!(r.channel, "stable");
    assert_eq!(
        r.url,
        "https://dl.usenook.ai/nook/builds/0.3.0-2f0bfbc/Nook-0.3.0.exe"
    );
    assert_eq!(r.size, 248_364_235);
}

#[tokio::test]
async fn an_offer_is_not_kept_when_it_no_longer_applies() {
    // Codex QA of ed57b1f, finding 3: the check only ever set the offer, so one found on stable
    // stayed on screen after a switch to dev, and a later check that found nothing left it there.
    let host = tempfile::tempdir().unwrap();
    let key = new_key();
    publish(
        host.path(),
        &key,
        "stable",
        &manifest_json(
            "stable",
            "0.3.1",
            "abc1234",
            "2026-09-23T09:00:00Z",
            &ab(),
            10,
        ),
    );
    let s = source_at(host.path(), public(&key));

    let offered = s.check("stable").await.unwrap();
    assert_eq!(offered.version, "0.3.1");

    // nothing published on dev: the check could not be made, which is an error, not an answer
    assert!(s.check("dev").await.is_none());
    assert!(
        s.last_error().is_some(),
        "an unreadable manifest is an error, not an answer"
    );

    // published but not newer: a clean answer of "nothing for you"
    publish(
        host.path(),
        &key,
        "dev",
        &manifest_json("dev", "0.1.0", "abc1234", "2026-09-23T09:00:00Z", &ab(), 10),
    );
    assert!(s.check("dev").await.is_none());
    assert_eq!(
        s.last_error(),
        None,
        "nothing wrong, nothing newer: the caller may drop what it was offering"
    );
}

#[test]
fn versions_compare_as_the_build_numbers_them() {
    assert!(compare_versions("0.3.1", "0.3.0").is_gt());
    assert!(compare_versions("0.10.0", "0.9.9").is_gt());
    assert!(compare_versions("0.3", "0.3.0").is_eq());
    assert!(compare_versions("0.3.0+396a3b4", "0.3.0-dev").is_eq());
    assert!(compare_versions("1.0.0", "0.99.99").is_gt());
    assert!(compare_versions("0.2.9", "0.3.0").is_lt());
    assert_eq!(this_build().label(), "0.3.0 (0efb5a2, 2026-09-22)");
    assert!(compare_versions(&BuildInfo::current().version, "0.5.0").is_ge());
}

#[test]
fn manifests_parse_as_jackson_read_them() {
    let ok = |json: &str| manifest::parse(json.as_bytes());
    let base = manifest_json(
        "stable",
        "0.3.1",
        "abc1234",
        "2026-09-23T09:00:00Z",
        &ab().to_uppercase(),
        10,
    );
    assert_eq!(ok(&base).unwrap().sha256, ab(), "the digest is lowercased");
    assert!(
        ok(&base.replace("\"size\":10", "\"size\":0")).is_none(),
        "a size must be positive"
    );
    assert!(
        ok(&base.replace("\"size\":10", "\"size\":\"10\"")).is_some(),
        "a number in a string is a number"
    );
    assert!(
        ok(&base.replace(&ab().to_uppercase(), "abc")).is_none(),
        "a digest is 64 characters"
    );
    assert!(
        ok(&base.replace("2026-09-23T09:00:00Z", "yesterday")).is_none(),
        "a bad time spoils it"
    );
    let no_time = ok(&base.replace("2026-09-23T09:00:00Z", "")).unwrap();
    assert_eq!(no_time.published, None);
    assert!(ok("not json").is_none());
    assert_eq!(
        ok(&base.replace("\"commit\":\"abc1234\",", ""))
            .unwrap()
            .title(),
        "0.3.1",
        "no commit, the version alone"
    );
}

#[test]
fn a_release_key_file_takes_comments_and_refuses_bad_lines() {
    let key = new_key();
    let line = manifest::encode_public_key(&key.verifying_key());
    let text = format!("# header\n\n{line}   # 2026-09-25, a comment\n{line} text after the key\n");
    assert_eq!(manifest::read_keys(&text).unwrap().len(), 2);
    let e = manifest::read_keys("AAAA\n").unwrap_err().to_string();
    assert!(
        e.contains("bad release key line") && e.contains("32 bytes"),
        "{e}"
    );
}

#[test]
fn the_release_serializes_with_its_title_for_the_dialog() {
    let r = manifest::parse(
        manifest_json("dev", "0.3.2", "1a2b3c4", "2026-09-23T09:00:00Z", &ab(), 10).as_bytes(),
    )
    .unwrap();
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["title"], "0.3.2 (1a2b3c4)");
    assert_eq!(v["version"], "0.3.2");
    assert_eq!(v["notes"], "n");
    let back: Release = serde_json::from_value(v).unwrap();
    assert_eq!(back, r);
}

// ------------------------------------------------------------------ VersionUpdateServiceTest

struct Busy(Option<&'static str>);
impl BusyWork for Busy {
    fn busy_with(&self) -> Option<String> {
        self.0.map(str::to_string)
    }
}

struct Broken;
impl BusyWork for Broken {
    fn busy_with(&self) -> Option<String> {
        panic!("no answer")
    }
}

fn idle_updater() -> Arc<Updater> {
    let source = UpdateSource::new(
        "",
        vec![],
        BuildInfo {
            version: "0.3.1".into(),
            commit: "fadc54a".into(),
            time: None,
        },
        None,
    );
    Updater::new(
        source,
        std::env::temp_dir().join("nook-rs-update-test-unused"),
    )
}

#[test]
fn a_local_feed_and_the_dev_channel_are_read_every_minute_and_stable_on_a_web_host_every_fifteen() {
    let now = 10_000_000i64;
    assert!(due_for_check(true, false, now - 1_000, now));
    assert!(due_for_check(false, true, now - 60_000, now));
    assert!(!due_for_check(false, false, now - 60_000, now));
    assert!(due_for_check(false, false, now - 900_000, now));
    assert!(due_for_check(false, false, 0, now), "never checked");
}

#[test]
fn nothing_registered_is_nothing_running() {
    assert_eq!(idle_updater().busy_with(), None);
}

#[test]
fn the_first_busy_work_names_what_is_running() {
    let u = idle_updater();
    u.register_busy("idle", Arc::new(Busy(None)));
    u.register_busy("video", Arc::new(Busy(Some("a clip is rendering"))));
    u.register_busy("downloads", Arc::new(Busy(Some("a model is downloading"))));
    assert_eq!(u.busy_with().as_deref(), Some("a clip is rendering"));
    // Quitting asks about all of them, each once.
    u.register_busy("video again", Arc::new(Busy(Some("a clip is rendering"))));
    assert_eq!(
        u.busy_all(),
        vec![
            "a clip is rendering".to_string(),
            "a model is downloading".to_string()
        ]
    );
}

#[test]
fn one_that_cannot_say_is_skipped() {
    let u = idle_updater();
    u.register_busy("broken", Arc::new(Broken));
    u.register_busy("code", Arc::new(Busy(Some("a Code session is working"))));
    assert_eq!(u.busy_with().as_deref(), Some("a Code session is working"));

    let only_broken = idle_updater();
    only_broken.register_busy("throws", Arc::new(Broken));
    assert_eq!(only_broken.busy_with(), None);
}

#[test]
fn the_waiting_note_says_which_build_and_why() {
    let release = Release {
        channel: "dev".into(),
        version: "0.3.2".into(),
        file: "Nook-0.3.2.exe".into(),
        url: "file:///f/Nook-0.3.2.exe".into(),
        sha256: "ab".into(),
        size: 1,
        commit: "1a2b3c4".into(),
        notes: String::new(),
        published: None,
    };
    assert_eq!(
        wait_note(&release, "a clip is rendering"),
        "Nook 0.3.2 (1a2b3c4) installs once nothing is running; for now a clip is rendering."
    );
}

// ------------------------------------------------------------------ the updater end to end

struct Feed {
    _dir: tempfile::TempDir,
    root: PathBuf,
    downloads: PathBuf,
    key: SigningKey,
    settings: Arc<Settings>,
}

impl Feed {
    fn new() -> Feed {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("feed");
        std::fs::create_dir_all(&root).unwrap();
        let settings =
            Arc::new(Settings::load(dir.path().join("data").join("settings.json")).unwrap());
        Feed {
            downloads: dir.path().join("tmp").join("update"),
            root,
            _dir: dir,
            key: new_key(),
            settings,
        }
    }

    fn updater_at(&self, base: &str) -> Arc<Updater> {
        let store: Arc<dyn ChannelStore> = self.settings.clone();
        let source = UpdateSource::new(base, vec![public(&self.key)], this_build(), Some(store));
        Updater::new(source, &self.downloads)
    }

    fn updater(&self) -> Arc<Updater> {
        self.updater_at(&to_url(&self.root.to_string_lossy()))
    }

    /// Puts an installer into the feed and signs a manifest naming it by file: URL.
    fn release(&self, channel: &str, version: &str, commit: &str, content: &[u8]) -> PathBuf {
        let dir = self.root.join("builds").join(format!("{version}-{commit}"));
        std::fs::create_dir_all(&dir).unwrap();
        let installer = dir.join(format!("Nook-{version}.exe"));
        std::fs::write(&installer, content).unwrap();
        let sha = source::sha256(&installer).unwrap();
        let url = url::Url::from_file_path(&installer).unwrap().to_string();
        let json = manifest_with_url(
            channel,
            version,
            commit,
            "2026-09-23T09:00:00Z",
            &sha,
            content.len() as u64,
            &url,
        );
        publish(&self.root, &self.key, channel, &json);
        installer
    }
}

/// Records what would have been installed and whether the app was told to quit.
#[derive(Clone, Default)]
struct Installs {
    launched: Arc<Mutex<Vec<PathBuf>>>,
    quits: Arc<Mutex<u32>>,
}

impl Installs {
    fn attach(&self, u: &Updater) {
        let launched = self.launched.clone();
        u.set_launcher(move |path| {
            assert!(path.is_file(), "the installer is there when it runs");
            launched.lock().push(path.to_path_buf());
            Ok(())
        });
        let quits = self.quits.clone();
        u.set_quit_hook(move || *quits.lock() += 1);
    }
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never happened: {what}");
}

#[tokio::test]
async fn stable_offers_the_build_and_later_hides_it_until_a_new_offer() {
    let feed = Feed::new();
    let u = feed.updater();
    u.check_now().await;
    let st = u.status();
    assert!(!st.is_update_available);
    assert!(
        st.last_check_error
            .as_deref()
            .unwrap_or("")
            .contains("could not read"),
        "{st:?}"
    );

    feed.release("stable", "0.3.1", "abc1234", b"MZ one");
    u.check_now().await;
    let st = u.status();
    assert!(st.is_update_available && !st.checking && st.enabled);
    assert_eq!(st.channel, "stable");
    assert_eq!(st.last_check_error, None);
    assert_eq!(
        st.latest_version_info.as_ref().unwrap().title(),
        "0.3.1 (abc1234)"
    );
    assert_eq!(st.current_version, "0.3.0");

    u.snooze();
    assert!(u.status().snoozed);
    u.check_now().await;
    assert!(u.status().snoozed, "the same offer stays put away");

    // withdrawn: a clean answer of "nothing newer" takes the offer away
    publish(
        &feed.root,
        &feed.key,
        "stable",
        &manifest_json(
            "stable",
            "0.3.0",
            "0efb5a2",
            "2026-09-22T09:00:00Z",
            &ab(),
            10,
        ),
    );
    u.check_now().await;
    assert!(!u.status().is_update_available);

    feed.release("stable", "0.3.2", "def5678", b"MZ two");
    u.check_now().await;
    let st = u.status();
    assert!(
        st.is_update_available && !st.snoozed,
        "a new offer shows again"
    );
    let json = serde_json::to_value(&st).unwrap();
    assert_eq!(json["latestVersionInfo"]["title"], "0.3.2 (def5678)");
    assert_eq!(json["isUpdateAvailable"], true);
    assert!(json.get("downloadProgress").is_some() && json.get("waitingNote").is_some());
}

#[tokio::test]
async fn switching_the_channel_drops_the_other_channels_offer() {
    let feed = Feed::new();
    feed.release("stable", "0.3.1", "abc1234", b"MZ stable");
    let u = feed.updater();
    u.check_now().await;
    assert!(u.status().is_update_available);

    u.set_channel("DEV").unwrap();
    assert_eq!(
        feed.settings
            .get(crate::settings::UPDATE_CHANNEL)
            .as_deref(),
        Some("dev")
    );
    assert_eq!(u.channel(), "dev");
    assert!(
        !u.status().is_update_available,
        "what stable offered is not on offer on dev"
    );
    u.check_now().await;
    let st = u.status();
    assert!(!st.is_update_available);
    assert!(st.last_check_error.is_some(), "dev has no manifest yet");
    u.set_channel("nonsense").unwrap();
    assert_eq!(u.channel(), "stable");
}

#[tokio::test]
async fn update_now_downloads_verifies_runs_the_installer_and_quits() {
    let feed = Feed::new();
    let content = vec![7u8; 300_000];
    feed.release("stable", "0.3.1", "abc1234", &content);
    let u = feed.updater();
    let installs = Installs::default();
    installs.attach(&u);
    let mut events = crate::events::subscribe();

    u.check_now().await;
    let task = u
        .start_download_and_install(false)
        .expect("a download starts");
    assert!(
        u.start_download_and_install(false).is_none(),
        "one download at a time"
    );
    task.await.unwrap();

    let launched = installs.launched.lock().clone();
    assert_eq!(launched, vec![feed.downloads.join("Nook-0.3.1.exe")]);
    assert_eq!(std::fs::read(&launched[0]).unwrap(), content);
    assert_eq!(*installs.quits.lock(), 1, "the app is told to quit");
    let st = u.status();
    assert_eq!(st.download_progress, 1.0);
    assert_eq!(st.update_error, None);

    let mut saw_progress = false;
    while let Ok(event) = events.try_recv() {
        if event.topic == crate::events::topic::UPDATE && event.payload["isDownloading"] == true {
            saw_progress |= event.payload["downloadProgress"].as_f64().unwrap_or(0.0) > 0.0;
        }
    }
    assert!(saw_progress, "progress goes out on the update topic");
}

#[tokio::test]
async fn a_download_that_is_not_what_the_manifest_named_is_refused() {
    let feed = Feed::new();
    let installer = feed.release("stable", "0.3.1", "abc1234", b"MZ the real one");
    std::fs::write(&installer, b"MZ a different one").unwrap();
    let u = feed.updater();
    let installs = Installs::default();
    installs.attach(&u);

    u.check_now().await;
    u.start_download_and_install(false).unwrap().await.unwrap();
    let st = u.status();
    assert!(!st.is_downloading);
    let error = st.update_error.unwrap_or_default();
    assert!(
        error.starts_with("Update failed: the download is 18 bytes, the manifest says 15; refused"),
        "{error}"
    );
    assert!(installs.launched.lock().is_empty(), "nothing runs");
    assert!(
        !feed.downloads.join("Nook-0.3.1.exe").exists(),
        "the wrong download is deleted"
    );
    assert!(st.is_update_available, "the offer stays for another try");
}

#[tokio::test]
async fn an_installer_that_does_not_start_says_so() {
    let feed = Feed::new();
    feed.release("stable", "0.3.1", "abc1234", b"MZ");
    let u = feed.updater();
    u.set_launcher(|_| anyhow::bail!("access is denied"));
    u.set_quit_hook(|| panic!("must not quit"));
    u.check_now().await;
    u.start_download_and_install(false).unwrap().await.unwrap();
    let st = u.status();
    assert_eq!(
        st.update_error.as_deref(),
        Some("Failed to launch installer: access is denied")
    );
    assert!(!st.is_downloading);
}

#[tokio::test]
async fn the_dev_channel_installs_by_itself_once_nothing_is_busy() {
    let feed = Feed::new();
    feed.settings
        .set(crate::settings::UPDATE_CHANNEL, "dev")
        .unwrap();
    feed.release("dev", "0.3.0", "abc1234", b"MZ dev build");
    let u = feed.updater();
    let installs = Installs::default();
    installs.attach(&u);
    let busy = Arc::new(Mutex::new(Some("a model is downloading")));
    struct Switch(Arc<Mutex<Option<&'static str>>>);
    impl BusyWork for Switch {
        fn busy_with(&self) -> Option<String> {
            self.0.lock().map(str::to_string)
        }
    }
    u.register_busy("downloads", Arc::new(Switch(busy.clone())));

    u.check_now().await;
    let st = u.status();
    assert!(st.is_update_available && !st.is_downloading);
    assert_eq!(
        st.waiting_note.as_deref(),
        Some("Nook 0.3.0 (abc1234) installs once nothing is running; for now a model is downloading.")
    );

    *busy.lock() = None;
    u.check_now().await;
    eventually("the dev build is installed", || {
        !installs.launched.lock().is_empty()
    })
    .await;
    assert_eq!(u.status().waiting_note, None);
    assert_eq!(*installs.quits.lock(), 1);
}

#[tokio::test]
async fn a_blank_base_never_checks() {
    let feed = Feed::new();
    let u = feed.updater_at("");
    u.check_now().await;
    let st = u.status();
    assert!(!st.enabled && !st.checking && !st.is_update_available);
    assert_eq!(st.last_check_error, None);
}

// ------------------------------------------------------------------ over HTTP

async fn serve(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

/// A host serving a folder, as nginx serves the download host's.
async fn serve_folder(root: PathBuf) -> String {
    use axum::extract::Path as UrlPath;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let router = axum::Router::new().route(
        "/{*path}",
        axum::routing::get(move |UrlPath(path): UrlPath<String>| {
            let root = root.clone();
            async move {
                match tokio::fs::read(root.join(path)).await {
                    Ok(bytes) => bytes.into_response(),
                    Err(_) => StatusCode::NOT_FOUND.into_response(),
                }
            }
        }),
    );
    serve(router).await
}

#[tokio::test]
async fn a_web_host_is_checked_and_downloaded_from() {
    let feed = Feed::new();
    let base = serve_folder(feed.root.clone()).await;
    let content = b"MZ from the web".to_vec();
    let dir = feed.root.join("builds").join("0.3.1-abc1234");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Nook-0.3.1.exe"), &content).unwrap();
    let sha = source::sha256(&dir.join("Nook-0.3.1.exe")).unwrap();
    let url = format!("{base}/builds/0.3.1-abc1234/Nook-0.3.1.exe");
    publish(
        &feed.root,
        &feed.key,
        "stable",
        &manifest_with_url(
            "stable",
            "0.3.1",
            "abc1234",
            "2026-09-23T09:00:00Z",
            &sha,
            content.len() as u64,
            &url,
        ),
    );

    let u = feed.updater_at(&format!("{base}/"));
    assert!(!u.source().local());
    assert_eq!(
        u.source().manifest_url("stable"),
        format!("{base}/{}/latest.json", channel_dir("stable"))
    );
    let installs = Installs::default();
    installs.attach(&u);
    u.check_now().await;
    assert!(u.status().is_update_available, "{:?}", u.status());
    u.start_download_and_install(false).unwrap().await.unwrap();
    assert_eq!(installs.launched.lock().len(), 1, "{:?}", u.status());

    // once the installer runs the app is on its way out, and nothing starts a second download
    assert!(u.status().is_downloading);
    assert!(u.start_download_and_install(false).is_none());

    // a build that vanished from the host
    std::fs::remove_file(dir.join("Nook-0.3.1.exe")).unwrap();
    let again = feed.updater_at(&base);
    installs.attach(&again);
    again.check_now().await;
    again
        .start_download_and_install(false)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(
        again.status().update_error.as_deref(),
        Some("Update failed: Server returned HTTP 404")
    );
    assert!(!again.status().is_downloading);

    // an unreachable manifest names the address and keeps the offer
    let dead = feed.updater_at("http://127.0.0.1:9");
    dead.check_now().await;
    assert!(dead
        .status()
        .last_check_error
        .unwrap_or_default()
        .starts_with(&format!(
            "could not read http://127.0.0.1:9/{}/latest.json: ",
            channel_dir("stable")
        )));
}

#[tokio::test]
async fn a_manifest_over_the_cap_is_refused() {
    let feed = Feed::new();
    let big = format!(
        "{{\"notes\":\"{}\"}}",
        "x".repeat(source::MANIFEST_MAX_BYTES)
    );
    publish(&feed.root, &feed.key, "stable", &big);
    let base = serve_folder(feed.root.clone()).await;
    let s = UpdateSource::new(&base, vec![public(&feed.key)], this_build(), None);
    assert!(s.check("stable").await.is_none());
    let error = s.last_error().unwrap();
    assert!(error.ends_with("more than 65536 bytes"), "{error}");
}

#[tokio::test]
async fn cancel_stops_the_download_and_deletes_the_file() {
    use axum::body::Body;
    use axum::http::header::CONTENT_LENGTH;
    use axum::response::Response;

    const CHUNKS: usize = 2_000;
    let router = axum::Router::new().route(
        "/slow.exe",
        axum::routing::get(|| async {
            let stream = futures::stream::unfold(0usize, |i| async move {
                if i >= CHUNKS {
                    return None;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
                Some((
                    Ok::<_, std::io::Error>(bytes::Bytes::from(vec![1u8; 1024])),
                    i + 1,
                ))
            });
            Response::builder()
                .header(CONTENT_LENGTH, CHUNKS * 1024)
                .body(Body::from_stream(stream))
                .unwrap()
        }),
    );
    let base = serve(router).await;
    let feed = Feed::new();
    let json = manifest_with_url(
        "stable",
        "0.3.1",
        "abc1234",
        "2026-09-23T09:00:00Z",
        &ab(),
        (CHUNKS * 1024) as u64,
        &format!("{base}/slow.exe"),
    );
    publish(&feed.root, &feed.key, "stable", &json);
    let u = feed.updater();
    let installs = Installs::default();
    installs.attach(&u);
    u.check_now().await;
    let task = u.start_download_and_install(false).unwrap();
    eventually("the download is under way", || {
        u.status().download_progress > 0.0
    })
    .await;
    assert!(u.status().is_downloading);
    u.cancel_download();
    assert!(!u.status().is_downloading, "cancel shows at once");
    task.await.unwrap();
    let st = u.status();
    assert!(!st.is_downloading);
    assert_eq!(st.download_progress, 0.0);
    assert_eq!(st.update_error, None, "a cancel is not a failure");
    assert!(
        !feed.downloads.join("Nook-0.3.1.exe").exists(),
        "the partial download is deleted"
    );
    assert!(installs.launched.lock().is_empty());
}

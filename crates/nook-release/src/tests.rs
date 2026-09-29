use super::*;

/// The public half of the throwaway seed 00 01 02 .. 1f, as tools/release.py prints it.
const FIXTURE_PUBLIC: &str = "A6EHv_POEL4dcN0Y50vAmWfk1jCbpQ1fHdyGZBJVMbg";
/// What tools/release.py wrote for that seed with
/// `manifest KEY --channel dev --sha256 0123456789abcdef(x4) --size 123456789
///  --url https://dl.example/nook/builds/0.5.1-abc1234/Nook-RS-0.5.1-setup.exe --version 0.5.1
///  --commit abc1234def --notes "Fixture — with a dash and a é" --published 2026-09-25T12:00:00Z`.
const FIXTURE_SIG: &str =
    "TA0fZzZqsp5dME209jxwuLEae5nMVCV__QJQJe0Ufyv2-t6DFB0WrUKHZwHJVza6jRbNJWuwUYYChfX3X08LCg";
const FIXTURE_BODY: &str = "{\n  \"channel\": \"dev\",\n  \"version\": \"0.5.1\",\n  \"file\": \"Nook-RS-0.5.1-setup.exe\",\n  \"url\": \"https://dl.example/nook/builds/0.5.1-abc1234/Nook-RS-0.5.1-setup.exe\",\n  \"sha256\": \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\",\n  \"size\": 123456789,\n  \"commit\": \"abc1234\",\n  \"notes\": \"Fixture \\u2014 with a dash and a \\u00e9\",\n  \"published\": \"2026-09-25T12:00:00Z\"\n}\n";

fn fixture_key(dir: &Path) -> PathBuf {
    let path = dir.join("fixture-key.json");
    let seed: Vec<u8> = (0u8..32).collect();
    let file = KeyFile {
        algorithm: "Ed25519".into(),
        purpose: PURPOSE.into(),
        seed: hex::encode(seed),
        public: FIXTURE_PUBLIC.into(),
        created: "2026-09-25T00:00:00+00:00".into(),
    };
    std::fs::write(&path, serde_json::to_string_pretty(&file).unwrap()).unwrap();
    path
}

#[test]
fn the_same_inputs_give_release_pys_bytes_and_signature() {
    let dir = tempfile::tempdir().unwrap();
    let key = load_key(&fixture_key(dir.path())).unwrap();
    assert_eq!(public_line(&key), FIXTURE_PUBLIC);
    let req = ManifestRequest {
        channel: "dev".into(),
        sha256: Some("0123456789abcdef".repeat(4)),
        size: Some(123_456_789),
        url: Some("https://dl.example/nook/builds/0.5.1-abc1234/Nook-RS-0.5.1-setup.exe".into()),
        version: Some("0.5.1".into()),
        commit: Some("abc1234def".into()),
        notes: Some("Fixture — with a dash and a é".into()),
        published: Some("2026-09-25T12:00:00Z".into()),
        ..Default::default()
    };
    let m = build_manifest(&req).unwrap();
    assert_eq!(String::from_utf8(manifest_bytes(&m)).unwrap(), FIXTURE_BODY);
    let path = write_signed(&key, &m, &dir.path().join("out")).unwrap();
    assert_eq!(path, dir.path().join("out").join("dev").join("latest.json"));
    assert_eq!(
        std::fs::read_to_string(path.with_file_name("latest.json.sig")).unwrap(),
        format!("{FIXTURE_SIG}\n")
    );
    // and the app believes it, given the key
    let r = verify_files(FIXTURE_PUBLIC, &path)
        .unwrap()
        .expect("verifies");
    assert_eq!(r.title(), "0.5.1 (abc1234)");
    assert_eq!(r.notes, "Fixture — with a dash and a é");
}

#[test]
fn keygen_writes_release_pys_key_file_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets").join("release-signing.json");
    let public = keygen(&path).unwrap();
    let file: KeyFile = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(file.algorithm, "Ed25519");
    assert_eq!(file.purpose, "nook-release-v1");
    assert_eq!(file.seed.len(), 64);
    assert_eq!(file.public, public);
    assert!(file.created.ends_with("+00:00"), "{}", file.created);
    assert_eq!(public_line(&load_key(&path).unwrap()), public);
    assert!(
        nook_core::update::manifest::public_key(&public).is_ok(),
        "a line release-keys.txt can carry"
    );

    let e = keygen(&path).unwrap_err().to_string();
    assert!(e.starts_with("refusing to overwrite"), "{e}");
    assert_eq!(
        serde_json::from_str::<KeyFile>(&std::fs::read_to_string(&path).unwrap())
            .unwrap()
            .seed,
        file.seed
    );
}

#[test]
fn a_manifest_for_an_installer_measures_it_and_follows_the_hosts_layout() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("key.json");
    keygen(&key_path).unwrap();
    let key = load_key(&key_path).unwrap();
    let build = dir.path().join("bundle");
    std::fs::create_dir_all(&build).unwrap();
    let installer = build.join("Nook RS_0.5.1_x64-setup.exe");
    std::fs::write(&installer, b"MZ the installer").unwrap();
    std::fs::write(
        build.join("build.json"),
        r#"{"version":"0.5.1","commit":"1234567890abcdef"}"#,
    )
    .unwrap();

    let req = ManifestRequest {
        channel: "stable".into(),
        installer: Some(installer.clone()),
        base: Some("https://dl.example/nook-rs/".into()),
        notes: Some("First".into()),
        ..Default::default()
    };
    let m = build_manifest(&req).unwrap();
    assert_eq!(m.version, "0.5.1");
    assert_eq!(m.commit, "1234567", "the short commit");
    assert_eq!(m.file, "Nook RS_0.5.1_x64-setup.exe");
    assert_eq!(
        m.url,
        "https://dl.example/nook-rs/builds/0.5.1-1234567/Nook%20RS_0.5.1_x64-setup.exe"
    );
    assert_eq!(m.size, 16);
    assert_eq!(m.sha256, sha256_of(&installer).unwrap());
    assert!(
        regex!(r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$").is_match(&m.published),
        "{}",
        m.published
    );

    let out = dir.path().join("site");
    let path = write_signed(&key, &m, &out).unwrap();
    let keys_file = dir.path().join("release-keys.txt");
    std::fs::write(
        &keys_file,
        format!("# keys\n{}   # 2026-09-25, a test key\n", public_line(&key)),
    )
    .unwrap();
    let r = verify_files(&keys_file.to_string_lossy(), &path)
        .unwrap()
        .expect("verifies against the key file");
    assert_eq!(r.size, 16);
    assert_eq!(r.channel, "stable");

    // a stranger's key does not verify it, and neither does a changed byte
    let stranger = dir.path().join("stranger.json");
    let stranger_public = keygen(&stranger).unwrap();
    assert!(verify_files(&stranger_public, &path).unwrap().is_none());
    let body = std::fs::read_to_string(&path)
        .unwrap()
        .replace("0.5.1", "0.5.2");
    std::fs::write(&path, body).unwrap();
    assert!(verify_files(&keys_file.to_string_lossy(), &path)
        .unwrap()
        .is_none());
}

#[test]
fn arguments_win_over_build_json_and_the_name() {
    let dir = tempfile::tempdir().unwrap();
    let installer = dir.path().join("Nook-RS-0.5.3-setup.exe");
    std::fs::write(&installer, b"MZ").unwrap();
    let mut req = ManifestRequest {
        channel: "dev".into(),
        installer: Some(installer),
        url: Some("file:///C:/feed/x.exe".into()),
        ..Default::default()
    };
    let m = build_manifest(&req).unwrap();
    assert_eq!(
        (m.version.as_str(), m.commit.as_str(), m.url.as_str()),
        ("0.5.3", "", "file:///C:/feed/x.exe")
    );
    req.version = Some("0.6.0".into());
    req.commit = Some("fedcba9876".into());
    let m = build_manifest(&req).unwrap();
    assert_eq!(
        (m.version.as_str(), m.commit.as_str()),
        ("0.6.0", "fedcba9")
    );
}

#[test]
fn what_a_manifest_cannot_be_made_from() {
    let err = |req: ManifestRequest| build_manifest(&req).unwrap_err().to_string();
    let base = ManifestRequest {
        channel: "stable".into(),
        url: Some("https://x/Nook-RS-0.5.1-setup.exe".into()),
        ..Default::default()
    };
    assert!(err(ManifestRequest {
        channel: "beta".into(),
        ..base.clone()
    })
    .contains("stable or dev"));
    assert!(err(base.clone()).starts_with("pass --installer, or --sha256 and --size"));
    assert!(err(ManifestRequest {
        sha256: Some("AB".repeat(32)),
        size: Some(1),
        ..base.clone()
    })
    .contains("64 lowercase hex"));
    assert!(err(ManifestRequest {
        installer: Some("C:/nowhere/x.exe".into()),
        ..base.clone()
    })
    .starts_with("no installer at"));
    let measured = ManifestRequest {
        sha256: Some("ab".repeat(32)),
        size: Some(1),
        ..base.clone()
    };
    assert_eq!(
        build_manifest(&measured).unwrap().version,
        "0.5.1",
        "from the file name in the URL"
    );
    let unnamed = ManifestRequest {
        url: Some("https://x/setup.exe".into()),
        ..measured.clone()
    };
    assert!(err(unnamed).starts_with("no version"));
    let escaped = ManifestRequest {
        url: Some("https://x/b/Nook%20RS_0.5.2_x64-setup.exe".into()),
        ..measured
    };
    let m = build_manifest(&escaped).unwrap();
    assert_eq!(
        (m.file.as_str(), m.version.as_str()),
        ("Nook RS_0.5.2_x64-setup.exe", "0.5.2")
    );
}

#[test]
fn installer_names_carry_their_version() {
    assert_eq!(
        version_from_name("Nook RS_0.5.1_x64-setup.exe").as_deref(),
        Some("0.5.1")
    );
    assert_eq!(
        version_from_name("Nook-RS-0.5.1-setup.exe").as_deref(),
        Some("0.5.1")
    );
    assert_eq!(
        version_from_name("Nook-0.4.2.exe").as_deref(),
        Some("0.4.2")
    );
    assert_eq!(
        version_from_name("Nook-0.6.0-macos-arm64.app.tar.gz").as_deref(),
        Some("0.6.0")
    );
    assert_eq!(version_from_name("setup.exe"), None);
    assert_eq!(version_from_name("Nook.app.tar.gz"), None);
}

#[test]
fn a_mac_update_has_its_manifest_under_the_channel() {
    let dir = tempfile::tempdir().unwrap();
    let key = load_key(&fixture_key(dir.path())).unwrap();
    let app = dir.path().join("Nook-0.6.0-macos-arm64.app.tar.gz");
    std::fs::write(&app, b"a bundle").unwrap();
    let m = build_manifest(&ManifestRequest {
        channel: "stable".into(),
        installer: Some(app),
        url: Some("https://dl.example/nook/builds/0.6.0/Nook-0.6.0-macos-arm64.app.tar.gz".into()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(m.version, "0.6.0");
    let out = dir.path().join("out");
    let path = write_signed_for(&key, &m, &out, Some("macos-arm64")).unwrap();
    assert_eq!(
        path,
        out.join("stable").join("macos-arm64").join("latest.json")
    );
    assert!(verify_files(FIXTURE_PUBLIC, &path).unwrap().is_some());
    // Windows' stays where every Nook before the Mac's reads it
    let windows = write_signed_for(&key, &m, &out, None).unwrap();
    assert_eq!(windows, out.join("stable").join("latest.json"));
    assert!(write_signed_for(&key, &m, &out, Some("../up")).is_err());
}

#[test]
fn a_bad_key_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("k.json");
    std::fs::write(&path, r#"{"algorithm":"Ed25519","purpose":"nook-release-v1","seed":"abcd","public":"","created":""}"#).unwrap();
    assert_eq!(
        load_key(&path).unwrap_err().to_string(),
        "the key file's seed is not 32 bytes"
    );
}

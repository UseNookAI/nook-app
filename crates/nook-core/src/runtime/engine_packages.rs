//! Ports `runtime/EnginePackages.java`.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::backend::Backend;
use super::downloader::{Downloader, Outcome};
use super::engine_component::EngineComponent;
use super::model_catalog::{long, text};
use super::{Progress, StagedProgress};
use crate::home::Home;

/// One archive of an engine package. `bytes` is 0 when the manifest does not say.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineArtifact {
    pub name: String,
    pub url: String,
    pub sha256: Option<String>,
    pub bytes: u64,
}

/// A component's archives for one backend.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Package {
    pub component: EngineComponent,
    pub backend: Backend,
    pub version: String,
    pub artifacts: Vec<EngineArtifact>,
}

impl Package {
    pub fn total_bytes(&self) -> u64 {
        self.artifacts.iter().map(|a| a.bytes).sum()
    }
}

/// Installs engine binaries from the pinned manifest in `runtime/engines.json`. Each component
/// (llama.cpp, whisper.cpp, stable-diffusion.cpp, audio.cpp) has its own archives per backend;
/// FFmpeg has one archive under the backend key `any`. Archives are downloaded with resume and
/// checksum verification, then extracted into `runtime/bin/<backend>` (llama),
/// `runtime/bin/<backend>/<component>`, or `runtime/bin/ffmpeg`. A marker file records the
/// installed version so a newer manifest triggers a reinstall.
pub struct EnginePackages {
    home: Home,
    downloader: Arc<Downloader>,
    versions: HashMap<EngineComponent, String>,
    packages: HashMap<(EngineComponent, Backend), Package>,
    /// One install at a time under the backends' folders (the Java method was `synchronized`:
    /// a llama install moves the other engines' folders inside its own).
    install_lock: tokio::sync::Mutex<()>,
    /// FFmpeg and PDFium live in folders of their own, so each installs beside the others: a
    /// 4 MB PDF engine never waits behind a gigabyte of speech engine.
    own_locks: HashMap<EngineComponent, tokio::sync::Mutex<()>>,
}

impl EnginePackages {
    /// The packages of the manifest built into the app.
    pub fn new(home: Home, downloader: Arc<Downloader>) -> Result<EnginePackages> {
        EnginePackages::from_json(home, downloader, crate::resources::ENGINES_JSON)
            .context("Cannot read runtime/engines.json")
    }

    /// Packages from a manifest in the format of `runtime/engines.json`.
    pub fn from_json(
        home: Home,
        downloader: Arc<Downloader>,
        json: &str,
    ) -> Result<EnginePackages> {
        let root: Value = serde_json::from_str(json)?;
        let mut versions = HashMap::new();
        let mut packages = HashMap::new();
        if let Some(components) = root.get("components").and_then(Value::as_object) {
            for (key, c) in components {
                let component = EngineComponent::from_id(key)
                    .ok_or_else(|| anyhow!("Unknown engine component: {key}"))?;
                let version = text(c.get("version")).unwrap_or_default();
                versions.insert(component, version.clone());
                for (b, artifacts) in c
                    .get("backends")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                {
                    // "any": one build for every backend (FFmpeg runs on the processor).
                    let backends = if b == "any" {
                        Backend::ALL.to_vec()
                    } else {
                        vec![Backend::from_id(b)?]
                    };
                    let artifacts: Vec<EngineArtifact> = artifacts
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|a| EngineArtifact {
                            name: text(a.get("name")).unwrap_or_default(),
                            url: text(a.get("url")).unwrap_or_default(),
                            sha256: text(a.get("sha256")),
                            bytes: long(a.get("bytes")).filter(|b| *b > 0).unwrap_or(0) as u64,
                        })
                        .collect();
                    for backend in backends {
                        packages.insert(
                            (component, backend),
                            Package {
                                component,
                                backend,
                                version: version.clone(),
                                artifacts: artifacts.clone(),
                            },
                        );
                    }
                }
            }
        }
        Ok(EnginePackages {
            home,
            downloader,
            versions,
            packages,
            install_lock: tokio::sync::Mutex::new(()),
            own_locks: EngineComponent::ALL
                .into_iter()
                .filter(|c| c.one_for_all())
                .map(|c| (c, tokio::sync::Mutex::new(())))
                .collect(),
        })
    }

    /// Version of the text engine, shown in the UI as "the runtime version".
    pub fn version(&self) -> &str {
        self.version_of(EngineComponent::Llama)
    }

    pub fn version_of(&self, component: EngineComponent) -> &str {
        self.versions
            .get(&component)
            .map(String::as_str)
            .unwrap_or("?")
    }

    /// The component's package for a backend: the build it runs there
    /// ([`EngineComponent::runs_on`]).
    pub fn package_for(&self, component: EngineComponent, backend: Backend) -> Result<&Package> {
        let backend = component.runs_on(backend);
        self.packages
            .get(&(component, backend))
            .ok_or_else(|| anyhow!("No {} package for backend {}", component.id(), backend.id()))
    }

    /// Directory holding a component's binaries for a backend: `runtime/bin/<backend>` for
    /// llama, `runtime/bin/<backend>/<component>` for the others (the backend its build runs
    /// on), and `runtime/bin/<component>` for those with one build for all (FFmpeg, PDFium,
    /// Pandoc, LibreOffice).
    pub fn dir(&self, component: EngineComponent, backend: Backend) -> PathBuf {
        if component.one_for_all() {
            return self.home.runtime_dir().join("bin").join(component.id());
        }
        let root = self.home.bin_dir(component.runs_on(backend).id());
        if component == EngineComponent::Llama {
            root
        } else {
            root.join(component.id())
        }
    }

    pub fn server_executable(&self, backend: Backend) -> PathBuf {
        self.dir(EngineComponent::Llama, backend)
            .join("llama-server.exe")
    }

    /// First existing executable among the candidates inside the component directory, else the
    /// first candidate.
    pub fn executable(
        &self,
        component: EngineComponent,
        backend: Backend,
        candidates: &[&str],
    ) -> PathBuf {
        let dir = self.dir(component, backend);
        candidates
            .iter()
            .map(|c| dir.join(c))
            .find(|p| p.exists())
            .unwrap_or_else(|| dir.join(candidates.first().copied().unwrap_or_default()))
    }

    /// True when the component's marker names this manifest's version.
    pub fn is_installed(&self, component: EngineComponent, backend: Backend) -> bool {
        let marker = self.dir(component, backend).join("installed.json");
        let Ok(bytes) = std::fs::read(&marker) else {
            return false;
        };
        let Ok(n) = serde_json::from_slice::<Value>(&bytes) else {
            return false;
        };
        let comp =
            text(n.get("component")).unwrap_or_else(|| EngineComponent::Llama.id().to_string());
        text(n.get("version")).as_deref() == Some(self.version_of(component))
            && comp == component.id()
    }

    /// Downloads and extracts the component when it is missing or outdated. Safe to call
    /// repeatedly. `progress` gets `("download" | "extract", done, total)`.
    ///
    /// Returns true when the component is ready to run afterwards, false when cancelled.
    pub async fn ensure_installed(
        &self,
        component: EngineComponent,
        backend: Backend,
        progress: Option<StagedProgress>,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        let _guard = match self.own_locks.get(&component) {
            Some(own) => own.lock().await,
            None => self.install_lock.lock().await,
        };
        if self.is_installed(component, backend) {
            return Ok(true);
        }
        let pkg = self.package_for(component, backend)?.clone();
        let total = pkg.total_bytes();
        let downloads = self.home.downloads_dir();
        tokio::fs::create_dir_all(&downloads)
            .await
            .with_context(|| format!("Could not create {}", downloads.display()))?;
        let mut done_before = 0u64;
        let mut archives = Vec::new();
        for a in &pkg.artifacts {
            let zip = downloads.join(&a.name);
            let before = done_before;
            let per_file: Option<Progress> = progress.clone().map(|p| {
                let p: Progress = Arc::new(move |done, _| p("download", before + done, total));
                p
            });
            let outcome = self
                .downloader
                .download(
                    &a.url,
                    &zip,
                    a.sha256.as_deref(),
                    a.bytes,
                    per_file.as_ref(),
                    cancel,
                )
                .await?;
            if outcome == Outcome::Cancelled {
                return Ok(false);
            }
            done_before += if a.bytes > 0 {
                a.bytes
            } else {
                tokio::fs::metadata(&zip)
                    .await
                    .map(|m| m.len())
                    .unwrap_or(0)
            };
            archives.push(zip);
        }

        let target = self.dir(component, backend);
        // Nook 0.5.2 to 0.5.5 ran the voice engine's Vulkan build on NVIDIA cards too; once the
        // CUDA build is in, that copy never runs again.
        let replaced = (component == EngineComponent::Audio
            && component.runs_on(backend) == Backend::Cuda)
            .then(|| self.dir(component, Backend::Vulkan));
        let progress_extract = progress.clone();
        let version = pkg.version.clone();
        let target_for_log = target.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let staging = super::downloader::with_suffix(&target, ".staging");
            delete_tree(&staging)?;
            std::fs::create_dir_all(&staging)
                .with_context(|| format!("Could not create {}", staging.display()))?;
            for zip in &archives {
                if let Some(p) = &progress_extract {
                    p("extract", total, total);
                }
                extract(zip, &staging)?;
            }
            if component == EngineComponent::Office {
                settle_office(&staging)?;
            }
            let marker = serde_json::json!({
                "version": version,
                "backend": backend.id(),
                "component": component.id(),
            });
            std::fs::write(staging.join("installed.json"), marker.to_string())
                .with_context(|| format!("Could not write {}", staging.display()))?;
            if component == EngineComponent::Llama {
                // Preserve sub-component directories that live inside the llama root.
                for other in EngineComponent::ALL {
                    if other == EngineComponent::Llama {
                        continue;
                    }
                    let sub = target.join(other.id());
                    if sub.is_dir() {
                        std::fs::rename(&sub, staging.join(other.id()))
                            .with_context(|| format!("Could not move {}", sub.display()))?;
                    }
                }
            }
            delete_tree(&target)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Could not create {}", parent.display()))?;
            }
            std::fs::rename(&staging, &target)
                .with_context(|| format!("Could not move {} into place", target.display()))?;
            for zip in &archives {
                let _ = std::fs::remove_file(zip);
            }
            if let Some(old) = &replaced {
                if let Err(e) = delete_tree(old) {
                    tracing::warn!("{e:#}");
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| anyhow!("the engine install was interrupted: {e}"))??;
        tracing::info!(
            "Installed {} {} ({}) into {}",
            component.id(),
            pkg.version,
            backend.id(),
            target_for_log.display()
        );
        Ok(true)
    }
}

/// Extracts a zip, or a `.tgz` / `.tar.gz` (PDFium's), flattening a single top-level directory if
/// the archive has one. Refuses entries that would land outside `into`.
pub fn extract(zip: &Path, into: &Path) -> Result<()> {
    let name = zip.to_string_lossy().to_lowercase();
    if name.ends_with(".tgz") || name.ends_with(".tar.gz") {
        return extract_tgz(zip, into);
    }
    if name.ends_with(".msi") {
        return extract_msi(zip, into);
    }
    let file =
        std::fs::File::open(zip).with_context(|| format!("Could not read {}", zip.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("{} is not a zip archive", zip.display()))?;
    let common_root = common_root_dir(&mut archive);
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .with_context(|| format!("Could not read {}", zip.display()))?;
        let raw = entry.name().replace('\\', "/");
        let mut name = raw.as_str();
        if let Some(root) = &common_root {
            if let Some(rest) = name.strip_prefix(root.as_str()) {
                name = rest;
            }
        }
        if name.trim().is_empty() {
            continue;
        }
        let out = resolve_inside(into, name)
            .ok_or_else(|| anyhow!("Zip entry escapes target directory: {raw}"))?;
        if entry.is_dir() {
            std::fs::create_dir_all(&out)
                .with_context(|| format!("Could not create {}", out.display()))?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Could not create {}", parent.display()))?;
            }
            let mut f = std::fs::File::create(&out)
                .with_context(|| format!("Could not write {}", out.display()))?;
            std::io::copy(&mut entry, &mut f)
                .with_context(|| format!("Could not write {}", out.display()))?;
        }
    }
    Ok(())
}

/// A tar name with forward slashes and no leading `./`.
fn tar_name(entry: &tar::Entry<'_, impl std::io::Read>) -> Result<String> {
    let name = entry.path()?.to_string_lossy().replace('\\', "/");
    Ok(name.trim_start_matches("./").to_string())
}

fn extract_tgz(archive: &Path, into: &Path) -> Result<()> {
    let open = || -> Result<tar::Archive<flate2::read::GzDecoder<std::fs::File>>> {
        let file = std::fs::File::open(archive)
            .with_context(|| format!("Could not read {}", archive.display()))?;
        Ok(tar::Archive::new(flate2::read::GzDecoder::new(file)))
    };
    // The single top-level directory every entry sits in, as for a zip.
    let mut root: Option<String> = None;
    let mut single = true;
    for entry in open()?
        .entries()
        .with_context(|| format!("{} is not a tar archive", archive.display()))?
    {
        let name = tar_name(&entry?)?;
        let Some(slash) = name.find('/') else {
            if !name.is_empty() {
                single = false;
            }
            continue;
        };
        let top = &name[..=slash];
        match &root {
            None => root = Some(top.to_string()),
            Some(r) if r != top => single = false,
            Some(_) => {}
        }
    }
    let root = root.filter(|_| single);
    for entry in open()?.entries()? {
        let mut entry = entry?;
        let raw = tar_name(&entry)?;
        let mut name = raw.as_str();
        if let Some(r) = &root {
            if let Some(rest) = name.strip_prefix(r.as_str()) {
                name = rest;
            }
        }
        if name.trim().is_empty() {
            continue;
        }
        let out = resolve_inside(into, name)
            .ok_or_else(|| anyhow!("Archive entry escapes target directory: {raw}"))?;
        match entry.header().entry_type() {
            tar::EntryType::Directory => std::fs::create_dir_all(&out)
                .with_context(|| format!("Could not create {}", out.display()))?,
            tar::EntryType::Regular => {
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("Could not create {}", parent.display()))?;
                }
                let mut f = std::fs::File::create(&out)
                    .with_context(|| format!("Could not write {}", out.display()))?;
                std::io::copy(&mut entry, &mut f)
                    .with_context(|| format!("Could not write {}", out.display()))?;
            }
            // Links and the like are not part of an engine.
            _ => {}
        }
    }
    Ok(())
}

/// `into/name` normalised lexically, or None when it is absolute or climbs out of `into`.
fn resolve_inside(into: &Path, name: &str) -> Option<PathBuf> {
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for c in Path::new(name).components() {
        match c {
            Component::Normal(p) => parts.push(p),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    let mut out = into.to_path_buf();
    out.extend(parts);
    Some(out)
}

/// The single top-level directory every entry sits in (`"dir/"`), or None when a file sits at
/// the top level or there are several roots.
fn common_root_dir<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Option<String> {
    let mut root: Option<String> = None;
    for name in archive.file_names() {
        let name = name.replace('\\', "/");
        let slash = name.find('/')?; // a file at the top level: nothing to flatten
        let top = &name[..=slash];
        match &root {
            None => root = Some(top.to_string()),
            Some(r) if r != top => return None,
            Some(_) => {}
        }
    }
    root
}

/// LibreOffice as `msiexec /a` leaves it, made to run from its folder: the Visual C++ runtime it
/// would install into Windows goes beside its programs (where Windows looks first), and the copy
/// of the package's database goes.
fn settle_office(dir: &Path) -> Result<()> {
    let program = dir.join("program");
    for from in ["System64", "System"] {
        let Ok(entries) = std::fs::read_dir(dir.join(from)) else {
            continue;
        };
        for e in entries.flatten() {
            let to = program.join(e.file_name());
            if e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("dll"))
                && !to.exists()
            {
                std::fs::copy(e.path(), &to)
                    .with_context(|| format!("Could not copy {}", e.path().display()))?;
            }
        }
    }
    for e in std::fs::read_dir(dir)?.flatten() {
        if e.path()
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("msi"))
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
    Ok(())
}

/// Unpacks a Windows installer package (LibreOffice's) the way an administrator makes a network
/// image of it: `msiexec /a`, which only copies the files out. Nothing is installed or
/// registered, and no rights beyond writing `into` are needed.
fn extract_msi(msi: &Path, into: &Path) -> Result<()> {
    std::fs::create_dir_all(into)
        .with_context(|| format!("Could not create {}", into.display()))?;
    let log = into.with_extension("msi-log.txt");
    let status = std::process::Command::new("msiexec")
        .arg("/a")
        .arg(msi)
        .arg("/qn")
        .arg(format!("TARGETDIR={}", into.display()))
        .arg("/l*")
        .arg(&log)
        .status()
        .context("Could not start Windows Installer (msiexec)")?;
    match status.code() {
        Some(0) => {
            let _ = std::fs::remove_file(&log);
            Ok(())
        }
        code => {
            let tail = std::fs::read_to_string(&log)
                .map(|l| {
                    l.lines()
                        .rev()
                        .filter(|x| !x.trim().is_empty())
                        .take(3)
                        .collect::<Vec<_>>()
                        .join(" / ")
                })
                .unwrap_or_default();
            bail!(
                "Windows Installer could not unpack {} (exit code {code:?}). {tail}",
                msi.display()
            )
        }
    }
}

pub(crate) fn delete_tree(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(dir).with_context(|| format!("Could not delete {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::downloader::tests::serve;
    use axum::routing::get;
    use axum::Router;
    use sha2::Digest;
    use std::io::Write;

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            for (name, data) in entries {
                if name.ends_with('/') {
                    w.add_directory(*name, opts).unwrap();
                } else {
                    w.start_file(*name, opts).unwrap();
                    w.write_all(data).unwrap();
                }
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    fn tgz_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, *data).unwrap();
        }
        let raw = tar.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&raw).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn a_tgz_unpacks_as_a_zip_does() {
        let dir = tempfile::tempdir().unwrap();
        // PDFium's layout: no single root, so nothing is flattened
        let plain = dir.path().join("pdfium.tgz");
        std::fs::write(
            &plain,
            tgz_bytes(&[("bin/pdfium.dll", b"dll"), ("LICENSE", b"bsd")]),
        )
        .unwrap();
        let out = dir.path().join("pdfium");
        extract(&plain, &out).unwrap();
        assert_eq!(
            std::fs::read(out.join("bin").join("pdfium.dll")).unwrap(),
            b"dll"
        );
        assert!(out.join("LICENSE").is_file());
        // one root folder is taken away, as for a zip
        let rooted = dir.path().join("rooted.tar.gz");
        std::fs::write(
            &rooted,
            tgz_bytes(&[("pkg/bin/a.dll", b"a"), ("pkg/b.txt", b"b")]),
        )
        .unwrap();
        let out = dir.path().join("rooted");
        extract(&rooted, &out).unwrap();
        assert!(out.join("bin").join("a.dll").is_file() && out.join("b.txt").is_file());
    }

    #[test]
    fn extract_flattens_a_single_root_and_refuses_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("a.zip");
        std::fs::write(
            &zip,
            zip_bytes(&[
                ("llama/", b""),
                ("llama/llama-server.exe", b"exe"),
                ("llama/lib/x.dll", b"dll"),
            ]),
        )
        .unwrap();
        let out = dir.path().join("out");
        extract(&zip, &out).unwrap();
        assert_eq!(std::fs::read(out.join("llama-server.exe")).unwrap(), b"exe");
        assert_eq!(
            std::fs::read(out.join("lib").join("x.dll")).unwrap(),
            b"dll"
        );

        let flat = dir.path().join("b.zip");
        std::fs::write(&flat, zip_bytes(&[("top.dll", b"t"), ("sub/y.dll", b"y")])).unwrap();
        let out2 = dir.path().join("out2");
        extract(&flat, &out2).unwrap();
        assert!(out2.join("top.dll").is_file());
        assert!(
            out2.join("sub").join("y.dll").is_file(),
            "a file at the top: nothing is flattened"
        );

        assert!(resolve_inside(&out, "../evil.dll").is_none());
        assert!(resolve_inside(&out, "a/../../evil.dll").is_none());
        assert_eq!(
            resolve_inside(&out, "a/./b/../c.dll").unwrap(),
            out.join("a").join("c.dll")
        );
    }

    fn manifest(base: &str, sha: &str) -> String {
        format!(
            r#"{{"components":{{
                "llama":{{"version":"b1","backends":{{"cuda":[{{"name":"llama.zip","url":"{base}/llama.zip","sha256":"{sha}","bytes":0}}]}}}},
                "whisper":{{"version":"w1","backends":{{"cuda":[{{"name":"whisper.zip","url":"{base}/whisper.zip"}}]}}}}
            }}}}"#
        )
    }

    #[tokio::test]
    async fn installs_verifies_and_keeps_sub_components() {
        let llama = zip_bytes(&[("bin/", b""), ("bin/llama-server.exe", b"server")]);
        let whisper = zip_bytes(&[("whisper-server.exe", b"w")]);
        let sha = hex::encode(sha2::Sha256::digest(&llama));
        let (l, w) = (llama.clone(), whisper.clone());
        let base = serve(
            Router::new()
                .route(
                    "/llama.zip",
                    get(move || {
                        let l = l.clone();
                        async move { l }
                    }),
                )
                .route(
                    "/whisper.zip",
                    get(move || {
                        let w = w.clone();
                        async move { w }
                    }),
                ),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        let packages = EnginePackages::from_json(
            home.clone(),
            Arc::new(Downloader::new()),
            &manifest(&base, &sha),
        )
        .unwrap();
        assert_eq!(packages.version(), "b1");
        assert_eq!(packages.version_of(EngineComponent::Sd), "?");
        assert_eq!(
            packages
                .package_for(EngineComponent::Sd, Backend::Cuda)
                .unwrap_err()
                .to_string(),
            "No sd package for backend cuda"
        );
        assert!(!packages.is_installed(EngineComponent::Llama, Backend::Cuda));

        let stages = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let s = stages.clone();
        let progress: StagedProgress =
            Arc::new(move |stage, _, _| s.lock().push(stage.to_string()));
        let never = CancellationToken::new();
        assert!(packages
            .ensure_installed(EngineComponent::Whisper, Backend::Cuda, None, &never)
            .await
            .unwrap());
        assert!(packages.is_installed(EngineComponent::Whisper, Backend::Cuda));
        assert!(packages
            .ensure_installed(
                EngineComponent::Llama,
                Backend::Cuda,
                Some(progress),
                &never
            )
            .await
            .unwrap());
        assert!(packages.is_installed(EngineComponent::Llama, Backend::Cuda));
        assert!(stages.lock().iter().any(|s| s == "download"));
        assert_eq!(stages.lock().last().map(String::as_str), Some("extract"));
        assert_eq!(
            std::fs::read(packages.server_executable(Backend::Cuda)).unwrap(),
            b"server"
        );
        assert!(
            packages.is_installed(EngineComponent::Whisper, Backend::Cuda),
            "whisper survived the llama install"
        );
        assert_eq!(
            packages.executable(
                EngineComponent::Whisper,
                Backend::Cuda,
                &["nope.exe", "whisper-server.exe"]
            ),
            home.bin_dir("cuda")
                .join("whisper")
                .join("whisper-server.exe")
        );
        assert_eq!(
            packages.executable(
                EngineComponent::Sd,
                Backend::Cuda,
                &["sd-cli.exe", "sd.exe"]
            ),
            home.bin_dir("cuda").join("sd").join("sd-cli.exe")
        );
        assert!(
            !home.downloads_dir().join("llama.zip").exists(),
            "archives are removed after install"
        );
        assert!(!home.bin_dir("cuda").with_file_name("cuda.staging").exists());

        // A newer manifest means a reinstall.
        let newer = manifest(&base, &sha).replace("\"b1\"", "\"b2\"");
        let packages2 =
            EnginePackages::from_json(home.clone(), Arc::new(Downloader::new()), &newer).unwrap();
        assert!(!packages2.is_installed(EngineComponent::Llama, Backend::Cuda));
    }

    #[tokio::test]
    async fn the_pdf_engine_does_not_wait_behind_a_big_engine() {
        let pdfium = tgz_bytes(&[("bin/pdfium.dll", b"dll")]);
        let base = serve(
            Router::new()
                // the speech engine's archive: answered only after half a minute
                .route(
                    "/whisper.zip",
                    get(|| async {
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        Vec::<u8>::new()
                    }),
                )
                .route(
                    "/pdfium.tgz",
                    get(move || {
                        let p = pdfium.clone();
                        async move { p }
                    }),
                ),
        )
        .await;
        let manifest = format!(
            r#"{{"components":{{
                "whisper":{{"version":"w1","backends":{{"cuda":[{{"name":"whisper.zip","url":"{base}/whisper.zip","bytes":100}}]}}}},
                "pdfium":{{"version":"p1","backends":{{"any":[{{"name":"pdfium.tgz","url":"{base}/pdfium.tgz","bytes":10}}]}}}}
            }}}}"#
        );
        let dir = tempfile::tempdir().unwrap();
        let packages = Arc::new(
            EnginePackages::from_json(Home::at(dir.path()), Arc::new(Downloader::new()), &manifest)
                .unwrap(),
        );
        let stop = CancellationToken::new();
        let slow = {
            let (packages, stop) = (packages.clone(), stop.clone());
            tokio::spawn(async move {
                packages
                    .ensure_installed(EngineComponent::Whisper, Backend::Cuda, None, &stop)
                    .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let quick = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            packages.ensure_installed(
                EngineComponent::Pdfium,
                Backend::Cuda,
                None,
                &CancellationToken::new(),
            ),
        )
        .await
        .expect("the PDF engine waited for the speech engine")
        .unwrap();
        assert!(quick);
        assert!(packages.is_installed(EngineComponent::Pdfium, Backend::Cpu));
        assert!(
            !slow.is_finished(),
            "the speech engine is still downloading"
        );
        stop.cancel();
        assert!(!slow.await.unwrap().unwrap(), "stopped");
    }

    #[tokio::test]
    async fn the_cuda_voice_engine_replaces_the_vulkan_one() {
        // audio.cpp's CUDA build comes as two archives with their files at the root: the
        // engine, and the CUDA runtime its DLLs need beside it.
        let bin = zip_bytes(&[("audiocpp_cli.exe", b"cli"), ("ggml-cuda.dll", b"g")]);
        let cudart = zip_bytes(&[("cudart64_12.dll", b"rt"), ("cufft64_11.dll", b"fft")]);
        let base = serve(
            Router::new()
                .route(
                    "/bin.zip",
                    get(move || {
                        let b = bin.clone();
                        async move { b }
                    }),
                )
                .route(
                    "/cudart.zip",
                    get(move || {
                        let c = cudart.clone();
                        async move { c }
                    }),
                ),
        )
        .await;
        let manifest = format!(
            r#"{{"components":{{"audio":{{"version":"a1","backends":{{"cuda":[
                {{"name":"bin.zip","url":"{base}/bin.zip"}},
                {{"name":"cudart.zip","url":"{base}/cudart.zip"}}
            ]}}}}}}}}"#
        );
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        // What Nook 0.5.2 to 0.5.5 installed on an NVIDIA card.
        let vulkan = home.bin_dir("vulkan").join("audio");
        std::fs::create_dir_all(&vulkan).unwrap();
        std::fs::write(vulkan.join("audiocpp_cli.exe"), b"old").unwrap();
        let packages =
            EnginePackages::from_json(home.clone(), Arc::new(Downloader::new()), &manifest)
                .unwrap();

        assert!(packages
            .ensure_installed(
                EngineComponent::Audio,
                Backend::Cuda,
                None,
                &CancellationToken::new()
            )
            .await
            .unwrap());
        let cuda = home.bin_dir("cuda").join("audio");
        assert_eq!(
            packages.executable(EngineComponent::Audio, Backend::Cuda, &["audiocpp_cli.exe"]),
            cuda.join("audiocpp_cli.exe")
        );
        assert_eq!(
            std::fs::read(cuda.join("audiocpp_cli.exe")).unwrap(),
            b"cli"
        );
        assert!(
            cuda.join("cudart64_12.dll").exists() && cuda.join("cufft64_11.dll").exists(),
            "the CUDA runtime sits beside the engine"
        );
        assert!(!vulkan.exists(), "the Vulkan copy is gone");
    }

    #[tokio::test]
    async fn a_bad_checksum_fails_the_install() {
        let llama = zip_bytes(&[("llama-server.exe", b"server")]);
        let base = serve(Router::new().route(
            "/llama.zip",
            get(move || {
                let l = llama.clone();
                async move { l }
            }),
        ))
        .await;
        let dir = tempfile::tempdir().unwrap();
        let packages = EnginePackages::from_json(
            Home::at(dir.path()),
            Arc::new(Downloader::new()),
            &manifest(&base, &"a".repeat(64)),
        )
        .unwrap();
        let err = packages
            .ensure_installed(
                EngineComponent::Llama,
                Backend::Cuda,
                None,
                &CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("Checksum mismatch for llama.zip"), "{err}");
        assert!(!packages.is_installed(EngineComponent::Llama, Backend::Cuda));
    }

    #[test]
    fn the_bundled_manifest_has_every_component() {
        let dir = tempfile::tempdir().unwrap();
        let packages =
            EnginePackages::new(Home::at(dir.path()), Arc::new(Downloader::new())).unwrap();
        for c in EngineComponent::ALL {
            for b in Backend::ALL {
                let p = packages.package_for(c, b).unwrap();
                assert!(!p.artifacts.is_empty());
                assert!(p
                    .artifacts
                    .iter()
                    .all(|a| a.sha256.as_deref().map(str::len) == Some(64) && a.bytes > 0));
            }
        }
        assert_ne!(packages.version(), "?");
        assert_eq!(
            packages.server_executable(Backend::Vulkan),
            dir.path()
                .join("runtime")
                .join("bin")
                .join("vulkan")
                .join("llama-server.exe")
        );
    }
}

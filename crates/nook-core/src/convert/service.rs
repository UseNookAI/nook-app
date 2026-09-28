//! The document converter Nooklet: what the chosen files can become (and what each target still
//! needs downloaded), that one download, and the conversions, one job at a time, their progress
//! on [`topic::CONVERT`] as `{"job": Job}` and `{"install": Install | null}`.
//!
//! A result goes beside its file ("report.pdf" beside "report.docx", "report (2).pdf" when that
//! is taken), or into the folder asked for.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use anyhow::{anyhow, bail, Result};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::formats::{self, Format, Kind, FORMATS};
use super::routes::{self, Engine, Have, Step};
use super::run::{self, Kit};
use super::system;
use crate::busy::BusyWork;
use crate::events::{self, topic};
use crate::flow::{Install, Reader, ReaderNeed, Sample, Stopped};
use crate::home::Home;
use crate::pdf::PdfEditor;
use crate::runtime::{Backend, EngineComponent, RuntimeManager, StagedProgress};

/// A file as the converter sees it: its format when Nook reads it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub path: String,
    pub name: String,
    pub format: Option<String>,
    pub format_name: Option<String>,
    pub kind: Option<Kind>,
}

/// An engine still to download for a target.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Need {
    pub engine: Engine,
    pub what: String,
    pub bytes: u64,
}

/// A format the files can become: what does it ("Word", "Pandoc"...), what is still to download,
/// or why it cannot be done here (`missing`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub by: String,
    pub needs: Vec<Need>,
    pub missing: Option<String>,
}

/// What a set of files can become. `combine`: they are pictures, and can go into one PDF.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Offer {
    pub files: Vec<FileInfo>,
    pub targets: Vec<Target>,
    pub combine: bool,
    /// Why there is nothing to offer, when there is not.
    pub note: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Status {
    Waiting,
    Converting,
    Done,
    Failed,
    Stopped,
}

/// One file of a job, and what became of it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub input: String,
    pub name: String,
    pub status: Status,
    pub outputs: Vec<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub to: String,
    pub to_name: String,
    /// When it was asked for, ms since the epoch.
    pub at: u64,
    pub status: Status,
    pub items: Vec<Item>,
}

impl Job {
    pub fn finished(&self) -> bool {
        !matches!(self.status, Status::Waiting | Status::Converting)
    }
}

/// What the converter's downloads are called.
fn what(engine: Engine) -> &'static str {
    match engine {
        Engine::Pandoc => "the document engine (Pandoc)",
        Engine::Pdf => "the PDF engine",
        Engine::LibreOffice => "the office engine (LibreOffice)",
        Engine::Edge => "Microsoft Edge",
        Engine::MsOffice => "Microsoft Office",
    }
}

fn component(engine: Engine) -> Option<EngineComponent> {
    match engine {
        Engine::Pandoc => Some(EngineComponent::Pandoc),
        Engine::Pdf => Some(EngineComponent::Pdfium),
        Engine::LibreOffice => Some(EngineComponent::Office),
        Engine::Edge | Engine::MsOffice => None,
    }
}

/// Who does the routes' work, in a word for the picker ("Word + Pandoc + Edge").
fn by(routes: &[Vec<Step>]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for s in routes.iter().flatten() {
        let n = match s {
            Step::Office {
                app: routes::App::Word,
                ..
            } => "Word",
            Step::Office {
                app: routes::App::Excel,
                ..
            } => "Excel",
            Step::Office {
                app: routes::App::PowerPoint,
                ..
            } => "PowerPoint",
            Step::Office {
                app: routes::App::Libre,
                ..
            } => "LibreOffice",
            Step::Pandoc { .. } => "Pandoc",
            Step::Print => "Edge",
            Step::PdfPages { .. } | Step::PdfText { .. } | Step::ImagesToPdf => "PDFium",
            Step::Image { .. } | Step::Table { .. } => "Nook",
        };
        if !names.contains(&n) {
            names.push(n);
        }
    }
    names.join(" + ")
}

/// A name that is free in `dir`: "name.ext", else "name (2).ext" and on.
pub fn free_path(dir: &Path, name: &str, ext: &str) -> PathBuf {
    let mut n = 1;
    loop {
        let file = if n == 1 {
            format!("{name}.{ext}")
        } else {
            format!("{name} ({n}).{ext}")
        };
        let p = dir.join(file);
        let pages = dir.join(format!(
            "{} pages",
            p.file_stem().unwrap_or_default().to_string_lossy()
        ));
        if !p.exists() && !pages.exists() {
            return p;
        }
        n += 1;
    }
}

pub struct ConvertService {
    runtime: Arc<RuntimeManager>,
    pdf: Arc<PdfEditor>,
    home: Home,
    jobs: Mutex<Vec<Job>>,
    stops: Mutex<HashMap<String, CancellationToken>>,
    /// One job at a time: office programs and LibreOffice's profile take one file each.
    turn: tokio::sync::Mutex<()>,
    install: Mutex<Option<Install>>,
    install_cancel: Mutex<Option<CancellationToken>>,
    stopping: CancellationToken,
    next: AtomicU64,
    /// Where Microsoft Office stands, for tests; None: this computer's.
    have: Option<Have>,
    me: Weak<ConvertService>,
}

/// Jobs kept to show.
const KEEP: usize = 30;

impl ConvertService {
    pub fn new(
        runtime: Arc<RuntimeManager>,
        pdf: Arc<PdfEditor>,
        home: Home,
    ) -> Arc<ConvertService> {
        Self::with_office(runtime, pdf, home, None)
    }

    pub fn with_office(
        runtime: Arc<RuntimeManager>,
        pdf: Arc<PdfEditor>,
        home: Home,
        have: Option<Have>,
    ) -> Arc<ConvertService> {
        Arc::new_cyclic(|me| ConvertService {
            runtime,
            pdf,
            home,
            jobs: Mutex::new(Vec::new()),
            stops: Mutex::new(HashMap::new()),
            turn: tokio::sync::Mutex::new(()),
            install: Mutex::new(None),
            install_cancel: Mutex::new(None),
            stopping: CancellationToken::new(),
            next: AtomicU64::new(1),
            have,
            me: me.clone(),
        })
    }

    fn have(&self) -> Have {
        self.have.unwrap_or_else(system::microsoft_office)
    }

    fn installed(&self, engine: Engine) -> bool {
        match engine {
            Engine::Edge => system::edge().is_some(),
            Engine::MsOffice => true,
            e => {
                component(e).is_some_and(|c| self.runtime.packages().is_installed(c, Backend::Cpu))
            }
        }
    }

    fn bytes(&self, engine: Engine) -> u64 {
        component(engine)
            .and_then(|c| self.runtime.packages().package_for(c, Backend::Cpu).ok())
            .map_or(0, |p| p.total_bytes())
    }

    /// What `paths` can become, together.
    pub fn offer(&self, paths: &[PathBuf]) -> Offer {
        let have = self.have();
        let files: Vec<FileInfo> = paths
            .iter()
            .map(|p| {
                let f = formats::of_path(p);
                FileInfo {
                    path: p.display().to_string(),
                    name: p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    format: f.map(|f| f.id.to_string()),
                    format_name: f.map(|f| f.name.to_string()),
                    kind: f.map(|f| f.kind),
                }
            })
            .collect();
        let known: Vec<&'static Format> =
            paths.iter().filter_map(|p| formats::of_path(p)).collect();
        let mut targets = Vec::new();
        if !known.is_empty() {
            for to in FORMATS {
                let routes: Option<Vec<Vec<Step>>> = known
                    .iter()
                    .map(|from| routes::route(from, to, &have))
                    .collect();
                let Some(routes) = routes else { continue };
                let mut engines: Vec<Engine> = Vec::new();
                for e in routes.iter().flat_map(|r| routes::engines(r)) {
                    if !engines.contains(&e) {
                        engines.push(e);
                    }
                }
                let missing =
                    (engines.contains(&Engine::Edge) && system::edge().is_none()).then(|| {
                        "Microsoft Edge makes these PDFs, and it is not on this computer."
                            .to_string()
                    });
                let needs = engines
                    .iter()
                    .filter(|e| component(**e).is_some() && !self.installed(**e))
                    .map(|&e| Need {
                        engine: e,
                        what: what(e).to_string(),
                        bytes: self.bytes(e),
                    })
                    .collect();
                targets.push(Target {
                    id: to.id.into(),
                    name: to.name.into(),
                    kind: to.kind,
                    by: by(&routes),
                    needs,
                    missing,
                });
            }
        }
        let note = if paths.is_empty() {
            None
        } else if known.is_empty() {
            Some("Nook does not read these files.".into())
        } else if targets.is_empty() {
            Some(
                "These files have no format in common to become: convert them one kind at a time."
                    .into(),
            )
        } else {
            None
        };
        Offer {
            combine: known.len() > 1
                && known.len() == paths.len()
                && known.iter().all(|f| f.kind == Kind::Image),
            files,
            targets,
            note,
        }
    }

    // ------------------------------------------------------------------ the download

    pub fn install_state(&self) -> Option<Install> {
        self.install.lock().clone()
    }

    fn install_changed(&self) {
        events::emit(topic::CONVERT, json!({ "install": self.install_state() }));
    }

    /// Downloads the engines in `engines` that are not in yet, one after another as one
    /// download, in the background.
    pub fn start_install(&self, engines: &[Engine]) -> Result<(), String> {
        if self.stopping.is_cancelled() {
            return Err("Nook is closing.".into());
        }
        if self.install_state().is_some_and(|i| i.error.is_none()) {
            return Ok(());
        }
        let wanted: Vec<Engine> = engines
            .iter()
            .copied()
            .filter(|e| component(*e).is_some() && !self.installed(*e))
            .collect();
        if wanted.is_empty() {
            return Ok(());
        }
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "Downloads need the app's async runtime.".to_string())?;
        let total: u64 = wanted.iter().map(|e| self.bytes(*e)).sum();
        let named = wanted
            .iter()
            .map(|e| what(*e))
            .collect::<Vec<_>>()
            .join(" and ");
        let cancel = self.stopping.child_token();
        *self.install_cancel.lock() = Some(cancel.clone());
        *self.install.lock() = Some(Install {
            what: named.clone(),
            done: 0,
            total,
            error: None,
        });
        self.install_changed();
        let me = self.me.clone();
        handle.spawn(async move {
            let Some(me) = me.upgrade() else { return };
            let mut before = 0u64;
            let mut outcome: Result<bool> = Ok(true);
            for e in wanted {
                let Some(c) = component(e) else { continue };
                let base = before;
                let progress: StagedProgress = {
                    let me = Arc::downgrade(&me);
                    Arc::new(move |_stage: &str, done, _of| {
                        let Some(me) = me.upgrade() else { return };
                        if let Some(i) = me.install.lock().as_mut().filter(|i| i.error.is_none()) {
                            i.done = (base + done).min(i.total);
                        }
                        me.install_changed();
                    })
                };
                outcome = me
                    .runtime
                    .ensure_component(c, Some(progress), &cancel)
                    .await;
                before += me.bytes(e);
                if !matches!(outcome, Ok(true)) {
                    break;
                }
            }
            let done = me.install_state().map_or(0, |i| i.done);
            *me.install.lock() = match outcome {
                Ok(true) => None,
                Ok(false) => Some(Install {
                    what: named.clone(),
                    done,
                    total,
                    error: Some("The download was stopped.".into()),
                }),
                Err(e) => {
                    tracing::warn!("The converter's engines did not install: {e:#}");
                    Some(Install {
                        what: named.clone(),
                        done,
                        total,
                        error: Some(format!("The download failed: {e:#}")),
                    })
                }
            };
            me.install_cancel.lock().take();
            me.install_changed();
        });
        Ok(())
    }

    pub fn cancel_install(&self) {
        if let Some(c) = self.install_cancel.lock().as_ref() {
            c.cancel();
        }
    }

    pub fn clear_install_error(&self) {
        let cleared = {
            let mut i = self.install.lock();
            if i.as_ref().is_some_and(|i| i.error.is_some()) {
                *i = None;
                true
            } else {
                false
            }
        };
        if cleared {
            self.install_changed();
        }
    }

    // ------------------------------------------------------------------ the jobs

    pub fn jobs(&self) -> Vec<Job> {
        self.jobs.lock().iter().rev().cloned().collect()
    }

    pub fn job(&self, id: &str) -> Option<Job> {
        self.jobs.lock().iter().find(|j| j.id == id).cloned()
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Job)) {
        let job = {
            let mut jobs = self.jobs.lock();
            let Some(j) = jobs.iter_mut().find(|j| j.id == id) else {
                return;
            };
            f(j);
            j.clone()
        };
        events::emit(topic::CONVERT, json!({ "job": job }));
    }

    /// Whether `path` is a result of a job (what may be opened or shown from the page).
    pub fn is_output(&self, path: &Path) -> bool {
        let p = path.display().to_string();
        self.jobs
            .lock()
            .iter()
            .flat_map(|j| &j.items)
            .flat_map(|i| &i.outputs)
            .any(|o| *o == p || path.starts_with(o) || Path::new(o).parent() == Some(path))
    }

    /// Converts `paths` into `to`, beside each file or into `folder`; pictures into one PDF when
    /// `combine`. The job runs in the background, after any before it.
    pub fn start(
        &self,
        paths: Vec<PathBuf>,
        to: &str,
        combine: bool,
        folder: Option<PathBuf>,
    ) -> Result<Job> {
        if self.stopping.is_cancelled() {
            bail!("Nook is closing.");
        }
        let target = formats::by_id(to).ok_or_else(|| anyhow!("Nook does not write {to}"))?;
        let offer = self.offer(&paths);
        let offered = offer
            .targets
            .iter()
            .find(|t| t.id == target.id)
            .ok_or_else(|| anyhow!("These files cannot become {}", target.name))?;
        if let Some(why) = &offered.missing {
            bail!("{why}");
        }
        if !offered.needs.is_empty() {
            bail!(
                "Download {} first",
                offered
                    .needs
                    .iter()
                    .map(|n| n.what.as_str())
                    .collect::<Vec<_>>()
                    .join(" and ")
            );
        }
        let combine = combine && offer.combine && target.id == "pdf";
        let items: Vec<Item> = if combine {
            vec![Item {
                input: paths[0].display().to_string(),
                name: format!("{} pictures", paths.len()),
                status: Status::Waiting,
                outputs: Vec::new(),
                error: None,
            }]
        } else {
            offer
                .files
                .iter()
                .map(|f| Item {
                    input: f.path.clone(),
                    name: f.name.clone(),
                    status: Status::Waiting,
                    outputs: Vec::new(),
                    error: None,
                })
                .collect()
        };
        let id = format!("cv_{}", self.next.fetch_add(1, Ordering::SeqCst));
        let job = Job {
            id: id.clone(),
            to: target.id.into(),
            to_name: target.name.into(),
            at: now_ms(),
            status: Status::Waiting,
            items,
        };
        {
            let mut jobs = self.jobs.lock();
            jobs.push(job.clone());
            let extra = jobs.len().saturating_sub(KEEP);
            jobs.drain(..extra);
        }
        let cancel = self.stopping.child_token();
        self.stops.lock().insert(id.clone(), cancel.clone());
        events::emit(topic::CONVERT, json!({ "job": job }));
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow!("Conversions need the app's async runtime."))?;
        let me = self.me.clone();
        let (to_id, paths_all) = (target.id, paths);
        handle.spawn(async move {
            let Some(me) = me.upgrade() else { return };
            me.work(&id, &paths_all, to_id, combine, folder, &cancel)
                .await;
            me.stops.lock().remove(&id);
        });
        Ok(job)
    }

    /// Stops a job: the file being converted and those after it.
    pub fn cancel(&self, id: &str) {
        if let Some(c) = self.stops.lock().get(id) {
            c.cancel();
        }
    }

    fn kit(&self) -> Kit {
        let packages = self.runtime.packages();
        let installed = |c: EngineComponent| packages.is_installed(c, Backend::Cpu);
        let soffice = installed(EngineComponent::Office).then(|| {
            let dir = packages.dir(EngineComponent::Office, Backend::Cpu);
            find_file(&dir, "soffice.com", 4).unwrap_or_else(|| dir.join("program/soffice.com"))
        });
        Kit {
            pandoc: installed(EngineComponent::Pandoc).then(|| {
                packages.executable(EngineComponent::Pandoc, Backend::Cpu, &["pandoc.exe"])
            }),
            soffice,
            edge: system::edge(),
            pdf: self.pdf.clone(),
            office_profile: self.home.runtime_dir().join("office-profile"),
            edge_profile: self.home.temp_dir().join("edge-profile"),
        }
    }

    async fn work(
        &self,
        id: &str,
        paths: &[PathBuf],
        to: &'static str,
        combine: bool,
        folder: Option<PathBuf>,
        cancel: &CancellationToken,
    ) {
        let turn = tokio::select! {
            t = self.turn.lock() => t,
            _ = cancel.cancelled() => {
                self.update(id, |j| {
                    j.status = Status::Stopped;
                    for i in &mut j.items { i.status = Status::Stopped; }
                });
                return;
            }
        };
        self.update(id, |j| j.status = Status::Converting);
        let kit = self.kit();
        let have = self.have();
        let target = formats::by_id(to).expect("a known format");
        let work_root = self.home.temp_dir().join("convert").join(id);
        let where_to = |input: &Path| -> PathBuf {
            folder
                .clone()
                .or_else(|| input.parent().map(Path::to_path_buf))
                .unwrap_or_default()
        };
        let name_of = |input: &Path| {
            input
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "converted".into())
        };
        let jobs: Vec<(usize, Vec<PathBuf>)> = if combine {
            vec![(0, paths.to_vec())]
        } else {
            paths
                .iter()
                .enumerate()
                .map(|(i, p)| (i, vec![p.clone()]))
                .collect()
        };
        let mut stopped = false;
        for (index, inputs) in jobs {
            if cancel.is_cancelled() {
                stopped = true;
                break;
            }
            self.update(id, |j| j.items[index].status = Status::Converting);
            let first = &inputs[0];
            let work = work_root.join(index.to_string());
            // A name no file has yet: whatever is there after a stop is this run's, half made.
            let out = free_path(&where_to(first), &name_of(first), target.ext);
            let result: Result<Vec<PathBuf>> = async {
                let from = formats::of_path(first)
                    .ok_or_else(|| anyhow!("Nook does not read this file"))?;
                if inputs.len() > 1 {
                    std::fs::create_dir_all(&work)?;
                    kit.pdf
                        .pictures_pdf(inputs.clone(), &out, &work, cancel)
                        .await?;
                    return Ok(vec![out.clone()]);
                }
                let steps = routes::route(from, target, &have)
                    .ok_or_else(|| anyhow!("{} cannot become {}", from.name, target.name))?;
                run::convert(&kit, &steps, first, &out, &work, cancel).await
            }
            .await;
            let _ = std::fs::remove_dir_all(&work);
            // Stop pressed while the last of it was made: stopped all the same, and what it wrote
            // goes rather than turn up as a result no one waited for.
            let result = match result {
                Ok(outputs) if cancel.is_cancelled() => {
                    remove_made(&outputs);
                    Err(Stopped.into())
                }
                Err(e) if e.is::<Stopped>() || cancel.is_cancelled() => {
                    remove_made(std::slice::from_ref(&out));
                    Err(e)
                }
                other => other,
            };
            match result {
                Ok(outputs) => self.update(id, |j| {
                    let i = &mut j.items[index];
                    i.status = Status::Done;
                    i.outputs = outputs.iter().map(|p| p.display().to_string()).collect();
                }),
                Err(e) if e.is::<Stopped>() || cancel.is_cancelled() => {
                    stopped = true;
                    self.update(id, |j| j.items[index].status = Status::Stopped);
                    break;
                }
                Err(e) => {
                    tracing::warn!("Converting {} to {to} failed: {e:#}", first.display());
                    self.update(id, |j| {
                        let i = &mut j.items[index];
                        i.status = Status::Failed;
                        i.error = Some(format!("{e:#}"));
                    });
                }
            }
        }
        let _ = std::fs::remove_dir_all(&work_root);
        drop(turn);
        self.update(id, |j| {
            for i in &mut j.items {
                if i.status == Status::Waiting {
                    i.status = Status::Stopped;
                }
            }
            j.status = if stopped {
                Status::Stopped
            } else if j.items.iter().all(|i| i.status == Status::Failed) {
                Status::Failed
            } else {
                Status::Done
            };
        });
    }

    pub fn shutdown(&self) {
        self.stopping.cancel();
    }
}

/// Removes files a stopped conversion wrote, and the folder a PDF's pages went into when it is
/// left empty.
fn remove_made(files: &[PathBuf]) {
    for file in files {
        let _ = std::fs::remove_file(file);
        if let Some(dir) = file.parent().filter(|d| {
            d.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(" pages"))
        }) {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// A file named `name` in `dir` or its folders, `depth` deep.
fn find_file(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let here = dir.join(name);
    if here.is_file() {
        return Some(here);
    }
    if depth == 0 {
        return None;
    }
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .find_map(|e| find_file(&e.path(), name, depth - 1))
}

/// The converter reads documents for the Nooklets that take them (Summarize, Read aloud): into
/// Markdown, by the same routes as a conversion to it, one at a time with the conversions.
#[async_trait::async_trait]
impl Reader for ConvertService {
    fn needs(&self, path: &Path) -> std::result::Result<Vec<ReaderNeed>, String> {
        let from = formats::of_path(path).ok_or_else(|| {
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if ext.is_empty() {
                "Nook cannot tell what kind of file this is.".to_string()
            } else {
                format!("Nook does not read .{ext} files.")
            }
        })?;
        if from.kind == Kind::Image {
            return Err("This is a picture. Choose a document, or a PDF of it.".into());
        }
        if ["md", "txt"].contains(&from.id) {
            return Ok(Vec::new());
        }
        let md = formats::by_id("md").expect("Markdown is a format");
        let steps = routes::route(from, md, &self.have())
            .ok_or_else(|| format!("Nook cannot read the words of a {} yet.", from.name))?;
        let mut needs: Vec<ReaderNeed> = Vec::new();
        for e in routes::engines(&steps) {
            let Some(c) = component(e).filter(|_| !self.installed(e)) else {
                continue;
            };
            if !needs.iter().any(|n| n.component == c) {
                needs.push(ReaderNeed {
                    component: c,
                    what: what(e).to_string(),
                    bytes: self.bytes(e),
                });
            }
        }
        Ok(needs)
    }

    async fn read(&self, path: &Path, work: &Path, cancel: &CancellationToken) -> Result<String> {
        let from = formats::of_path(path).ok_or_else(|| anyhow!("Nook does not read this file"))?;
        if ["md", "txt"].contains(&from.id) {
            return crate::flow::reader::read_plain(path);
        }
        let md = formats::by_id("md").expect("Markdown is a format");
        let steps = routes::route(from, md, &self.have())
            .ok_or_else(|| anyhow!("Nook cannot read the words of a {} yet.", from.name))?;
        // Only Office, LibreOffice and Edge (one profile each) take turns with the conversions;
        // Pandoc runs alongside, and PDFium has a queue of its own.
        let _turn = if steps
            .iter()
            .any(|s| matches!(s, Step::Office { .. } | Step::Print))
        {
            Some(tokio::select! {
                t = self.turn.lock() => t,
                _ = cancel.cancelled() => return Err(Stopped.into()),
            })
        } else {
            None
        };
        let out = work.join("words.md");
        let written = run::convert(&self.kit(), &steps, path, &out, work, cancel).await;
        let text = written.and_then(|files| {
            let mut all = String::new();
            for f in files.iter().filter(|f| f.is_file()) {
                if !all.is_empty() {
                    all.push_str("\n\n");
                }
                all.push_str(&crate::flow::reader::read_plain(f)?);
            }
            Ok(all)
        });
        let _ = std::fs::remove_dir_all(work);
        text
    }

    async fn sample(&self, path: &Path, cancel: &CancellationToken) -> Result<Option<Sample>> {
        if formats::of_path(path).is_none_or(|f| f.id != "pdf") || !self.installed(Engine::Pdf) {
            return Ok(None);
        }
        let (pages, of) = self
            .kit()
            .pdf
            .text_sample(path, SAMPLE_PAGES, cancel)
            .await?;
        Ok(Some(Sample {
            text: super::pdftext::write(&pages, false),
            pages: pages.len(),
            of,
        }))
    }
}

/// The pages of a PDF a preview reads.
const SAMPLE_PAGES: usize = 3;

impl BusyWork for ConvertService {
    fn busy_with(&self) -> Option<String> {
        if self.install_state().is_some_and(|i| i.error.is_none()) {
            return Some("the converter's engines are downloading".into());
        }
        self.jobs
            .lock()
            .iter()
            .any(|j| !j.finished())
            .then(|| "documents are being converted".to_string())
    }
}

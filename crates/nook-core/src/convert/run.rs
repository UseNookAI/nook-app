//! A conversion carried out: its steps one after another, each in the program it needs, the
//! files between them in the conversion's work folder.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use tokio_util::sync::CancellationToken;

use super::routes::{App, Step};
use super::{formats, images, pdftext, tables, tools};
use crate::flow::Stopped;
use crate::pdf::PdfEditor;

/// Pictures of PDF pages are drawn at this many dots an inch.
pub const PAGE_DPI: f32 = 150.0;

/// What a conversion runs with: the engines found (None when not installed) and where Edge and
/// LibreOffice keep their profiles.
#[derive(Clone)]
pub struct Kit {
    pub pandoc: Option<PathBuf>,
    pub soffice: Option<PathBuf>,
    pub edge: Option<PathBuf>,
    pub pdf: Arc<PdfEditor>,
    pub office_profile: PathBuf,
    pub edge_profile: PathBuf,
}

/// The extension a step writes.
fn writes(step: &Step) -> &'static str {
    match step {
        Step::Pandoc { to, .. } => match *to {
            "html5" => "html",
            "gfm" => "md",
            "plain" => "txt",
            "latex" => "tex",
            "asciidoc" => "adoc",
            "typst" => "typ",
            "mediawiki" => "wiki",
            "epub3" => "epub",
            other => other,
        },
        Step::Print | Step::ImagesToPdf => "pdf",
        Step::Office { to, .. }
        | Step::Image { to }
        | Step::Table { to }
        | Step::PdfPages { to } => to,
        Step::PdfText { plain: true } => "txt",
        Step::PdfText { plain: false } => "md",
    }
}

fn stop_if(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(Stopped.into())
    } else {
        Ok(())
    }
}

/// Converts `input` along `steps` into `out`. A PDF's pages become several pictures, in a
/// folder "<name> pages" beside `out` (one page: `out` itself), and a workbook's sheets several
/// CSV files beside it. Returns what was written.
pub async fn convert(
    kit: &Kit,
    steps: &[Step],
    input: &Path,
    out: &Path,
    work: &Path,
    cancel: &CancellationToken,
) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(work)
        .with_context(|| format!("Could not create {}", work.display()))?;
    let need = |p: &Option<PathBuf>, what: &str| -> Result<PathBuf> {
        p.clone().ok_or_else(|| anyhow!("{what} is not installed"))
    };
    let mut current = input.to_path_buf();
    for (i, step) in steps.iter().enumerate() {
        stop_if(cancel)?;
        let last = i + 1 == steps.len();
        let target = if last {
            out.to_path_buf()
        } else {
            work.join(format!(
                "step {} of {}.{}",
                i + 1,
                steps.len(),
                writes(step)
            ))
        };
        match step {
            Step::Pandoc { from, to } => {
                let exe = need(&kit.pandoc, "The document engine (Pandoc)")?;
                tools::pandoc(&exe, &current, from, to, &target, work, cancel).await?;
            }
            Step::Print => {
                let edge = need(&kit.edge, "Microsoft Edge")?;
                tools::print(&edge, &current, &target, &kit.edge_profile, cancel).await?;
            }
            Step::Office {
                app: App::Libre,
                to,
            } => {
                let soffice = need(&kit.soffice, "The office engine (LibreOffice)")?;
                tools::libre_office(
                    &soffice,
                    &current,
                    to,
                    &target,
                    work,
                    &kit.office_profile,
                    cancel,
                )
                .await?;
            }
            Step::Office { app, to }
                if *app == App::Excel && tables::has_macro_sheets(&current) =>
            {
                // Excel 4.0 macro sheets, which a hidden Excel may stop to ask about: LibreOffice,
                // its macros off, converts them when it is here.
                let Some(soffice) = kit.soffice.clone() else {
                    bail!("This workbook has Excel 4.0 macro sheets, and Excel would stop to ask about them with no one to answer, so Nook does not open it in Excel. Save a copy without the macro sheets (or as .xlsx) and convert that, or get the office engine (LibreOffice) for Nook to use instead.");
                };
                tools::libre_office(
                    &soffice,
                    &current,
                    to,
                    &target,
                    work,
                    &kit.office_profile,
                    cancel,
                )
                .await?;
            }
            Step::Office { app, to } => {
                tools::ms_office(*app, &current, to, &target, work, cancel).await?;
            }
            Step::PdfPages { to } => {
                if !last {
                    bail!("Pages as pictures come last");
                }
                let name = out
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let dir = out.with_file_name(format!("{name} pages"));
                return kit
                    .pdf
                    .page_pictures(&current, to, PAGE_DPI, out, &dir, cancel)
                    .await;
            }
            Step::PdfText { plain } => {
                let pages = kit.pdf.text_lines(&current, cancel).await?;
                stop_if(cancel)?;
                let text = pdftext::write(&pages, *plain);
                if text.trim().is_empty() {
                    bail!("This PDF has no text Nook can read: its pages are pictures Windows could not read either");
                }
                std::fs::write(&target, text)
                    .with_context(|| format!("Could not write {}", target.display()))?;
            }
            Step::ImagesToPdf => {
                kit.pdf
                    .pictures_pdf(vec![current.clone()], &target, work, cancel)
                    .await?;
            }
            Step::Image { to } => {
                let (from, to, into) = (current.clone(), to.to_string(), target.clone());
                tokio::task::spawn_blocking(move || images::convert(&from, &to, &into))
                    .await
                    .map_err(|e| anyhow!("The conversion was interrupted: {e}"))??;
            }
            Step::Table { to } => {
                let format = formats::of_path(&current)
                    .map(|f| f.id)
                    .ok_or_else(|| anyhow!("{} is not a table", current.display()))?;
                let (from, to_id, into) = (current.clone(), to.to_string(), target.clone());
                let written = tokio::task::spawn_blocking(move || {
                    let sheets = tables::read(&from, format)?;
                    tables::write(&sheets, &to_id, &into)
                })
                .await
                .map_err(|e| anyhow!("The conversion was interrupted: {e}"))??;
                if last {
                    return Ok(written);
                }
                if written.len() != 1 {
                    bail!("A workbook of several sheets goes on as one file");
                }
            }
        }
        current = target;
    }
    Ok(vec![current])
}

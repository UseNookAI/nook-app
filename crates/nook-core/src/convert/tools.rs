//! The programs a conversion runs: Pandoc, Edge (printing a page to PDF), Word, Excel and
//! PowerPoint (through a PowerShell script), and LibreOffice (headless). Each runs to its end,
//! with no window, and is killed when the conversion is stopped or runs too long.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio_util::sync::CancellationToken;

use super::routes::App;
use crate::flow::Stopped;

/// How long one program may take over one file.
const LIMIT: Duration = Duration::from_secs(5 * 60);

/// Runs `cmd` to its end: its output, or why it failed (the last lines it wrote).
async fn run(
    cmd: &mut tokio::process::Command,
    what: &str,
    cancel: &CancellationToken,
) -> Result<String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child =
        crate::process::spawn_managed(cmd).with_context(|| format!("Could not start {what}"))?;
    let out = tokio::select! {
        out = child.wait_with_output() => out.with_context(|| format!("{what} did not finish"))?,
        _ = tokio::time::sleep(LIMIT) => bail!("{what} took over {} minutes and was stopped", LIMIT.as_secs() / 60),
        _ = cancel.cancelled() => return Err(Stopped.into()),
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() {
        let tail: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let tail = tail[tail.len().saturating_sub(3)..].join(" ");
        bail!(
            "{what} could not do it: {}",
            if tail.is_empty() {
                format!("exit code {:?}", out.status.code())
            } else {
                tail
            }
        );
    }
    Ok(text)
}

fn written(out: &Path, what: &str) -> Result<()> {
    match std::fs::metadata(out) {
        Ok(m) if m.len() > 0 => Ok(()),
        _ => bail!("{what} wrote nothing"),
    }
}

/// A file's `file:///` address, for a browser.
pub fn file_url(path: &Path) -> String {
    let mut out = String::from("file:///");
    for b in path.to_string_lossy().replace('\\', "/").bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The style a page Pandoc writes is printed in: readable type, tables ruled, code set off.
pub const PAGE_CSS: &str =
    "html { font: 11pt/1.5 'Segoe UI', Calibri, Arial, sans-serif; color: #1a1a1a; }
body { max-width: 46em; margin: 0 auto; padding: 16px; }
h1, h2, h3, h4 { line-height: 1.25; margin: 1.2em 0 0.4em; }
h1 { font-size: 1.8em; } h2 { font-size: 1.4em; } h3 { font-size: 1.15em; }
p { margin: 0 0 0.7em; } img { max-width: 100%; }
table { border-collapse: collapse; margin: 0.8em 0; }
th, td { border: 1px solid #c8c8c8; padding: 3px 8px; text-align: left; vertical-align: top; }
th { background: #f0f0f0; }
pre, code { font-family: Consolas, 'Cascadia Mono', monospace; font-size: 0.92em; }
pre { background: #f5f5f5; padding: 8px 10px; white-space: pre-wrap; }
blockquote { margin: 0.8em 0; padding-left: 1em; border-left: 3px solid #d0d0d0; color: #444; }
header#title-block-header { display: none; }
@page { margin: 18mm 16mm; }
";

/// Pandoc from its format `from` to `to`. A page gets `css` inlined (with its pictures), and a
/// page or an e-book a title from the file's name when the document has none.
pub async fn pandoc(
    exe: &Path,
    input: &Path,
    from: &str,
    to: &str,
    out: &Path,
    work: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    let title = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Metadata from a file gives way to the document's own.
    let meta = work.join("pandoc-meta.yaml");
    std::fs::write(
        &meta,
        format!("title: {}\npagetitle: {}\n", yaml(&title), yaml(&title)),
    )?;
    let mut cmd = crate::process::command(exe);
    cmd.arg("--from")
        .arg(from)
        .arg("--to")
        .arg(to)
        .arg("--standalone")
        .arg("--metadata-file")
        .arg(&meta)
        .arg("--output")
        .arg(out);
    if let Some(dir) = input.parent() {
        cmd.arg(format!("--resource-path={}", dir.display()))
            .current_dir(dir);
    }
    if to == "html5" {
        let css = work.join("page.css");
        std::fs::write(&css, PAGE_CSS)?;
        cmd.arg("--embed-resources").arg("--css").arg(&css);
    }
    // Text formats point at their pictures: those of a Word file or an e-book are written out
    // to a folder beside the result (made only when there are some).
    if [
        "gfm",
        "latex",
        "rst",
        "org",
        "asciidoc",
        "typst",
        "mediawiki",
    ]
    .contains(&to)
    {
        if let (Some(dir), Some(stem)) = (out.parent(), out.file_stem()) {
            let media = dir.join(format!("{} media", stem.to_string_lossy()));
            cmd.arg(format!("--extract-media={}", media.display()));
        }
    }
    cmd.arg(input);
    run(&mut cmd, "Pandoc", cancel).await?;
    written(out, "Pandoc")
}

fn yaml(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A web page printed to PDF by Edge, headless, in a profile of its own (`profile`), without
/// the date and address in the margins.
pub async fn print(
    edge: &Path,
    html: &Path,
    pdf: &Path,
    profile: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut cmd = crate::process::command(edge);
    cmd.arg("--headless=new")
        .arg("--disable-gpu")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-extensions")
        .arg("--disable-sync")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--no-pdf-header-footer")
        .arg(format!("--print-to-pdf={}", pdf.display()))
        .arg(file_url(html));
    run(&mut cmd, "Edge", cancel).await?;
    written(pdf, "Edge")
}

/// The number Word, Excel or PowerPoint saves `to` as (-1: Excel's PDF export).
pub fn office_format(app: App, to: &str) -> Option<i32> {
    Some(match (app, to) {
        (App::Word, "pdf") => 17,
        (App::Word, "docx") => 16,
        (App::Word, "doc") => 0,
        (App::Word, "rtf") => 6,
        (App::Word, "odt") => 23,
        (App::Excel, "pdf") => -1,
        (App::Excel, "xlsx") => 51,
        (App::Excel, "xls") => 56,
        (App::Excel, "ods") => 60,
        (App::PowerPoint, "pdf") => 32,
        (App::PowerPoint, "pptx") => 24,
        (App::PowerPoint, "ppt") => 1,
        (App::PowerPoint, "odp") => 35,
        _ => return None,
    })
}

/// A file opened in Word, Excel or PowerPoint (hidden, read-only) and saved as `to`.
pub async fn ms_office(
    app: App,
    input: &Path,
    to: &str,
    out: &Path,
    work: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    let (name, what) = match app {
        App::Word => ("word", "Word"),
        App::Excel => ("excel", "Excel"),
        App::PowerPoint => ("powerpoint", "PowerPoint"),
        App::Libre => bail!("LibreOffice is not Microsoft Office"),
    };
    let format = office_format(app, to).ok_or_else(|| anyhow!("{what} does not save {to}"))?;
    let script = work.join("office.ps1");
    std::fs::write(&script, include_str!("office.ps1"))?;
    let pids = work.join(format!("{name}.pid"));
    let _ = std::fs::remove_file(&pids);
    let mut cmd = crate::process::command("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
    ])
    .arg(&script)
    .arg("-App")
    .arg(name)
    .arg("-In")
    .arg(absolute(input)?)
    .arg("-Out")
    .arg(absolute(out)?)
    .arg("-Format")
    .arg(format.to_string())
    .arg("-PidFile")
    .arg(absolute(&pids)?);
    if let Err(e) = run(&mut cmd, what, cancel).await {
        // Stopped, or stuck on something it shows no one: the program it started goes too.
        end_started(&pids, app);
        return Err(e);
    }
    written(out, what)
}

/// Ends the Office program a run started (its process ids in `pids`), should it still run.
fn end_started(pids: &Path, app: App) {
    let image = match app {
        App::Word => "WINWORD.EXE",
        App::Excel => "EXCEL.EXE",
        App::PowerPoint => "POWERPNT.EXE",
        App::Libre => return,
    };
    let Ok(list) = std::fs::read_to_string(pids) else {
        return;
    };
    for pid in list
        .lines()
        .map(str::trim)
        .filter(|l| l.chars().all(|c| c.is_ascii_digit()) && !l.is_empty())
    {
        let _ = crate::process::std_command("taskkill.exe")
            .args([
                "/F",
                "/FI",
                &format!("PID eq {pid}"),
                "/FI",
                &format!("IMAGENAME eq {image}"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn absolute(p: &Path) -> Result<PathBuf> {
    std::path::absolute(p).with_context(|| format!("No full path for {}", p.display()))
}

/// A file converted by LibreOffice, headless, in a profile of its own (`profile`, kept between
/// runs: its first start is slow).
pub async fn libre_office(
    soffice: &Path,
    input: &Path,
    to: &str,
    out: &Path,
    work: &Path,
    profile: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    let filter = match to {
        "docx" => "docx:MS Word 2007 XML",
        "doc" => "doc:MS Word 97",
        "xlsx" => "xlsx:Calc MS Excel 2007 XML",
        "xls" => "xls:MS Excel 97",
        "pptx" => "pptx:Impress MS PowerPoint 2007 XML",
        "ppt" => "ppt:MS PowerPoint 97",
        other => other,
    };
    let outdir = work.join("office-out");
    std::fs::create_dir_all(&outdir)?;
    no_macros(profile)?;
    let mut cmd = crate::process::command(soffice);
    cmd.args([
        "--headless",
        "--norestore",
        "--nolockcheck",
        "--nologo",
        "--nodefault",
    ])
    .arg(format!(
        "-env:UserInstallation={}",
        file_url(&absolute(profile)?)
    ))
    .arg("--convert-to")
    .arg(filter)
    .arg("--outdir")
    .arg(&outdir)
    .arg(absolute(input)?);
    run(&mut cmd, "LibreOffice", cancel).await?;
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let made = outdir.join(format!("{stem}.{to}"));
    written(&made, "LibreOffice")?;
    std::fs::rename(&made, out)
        .or_else(|_| std::fs::copy(&made, out).map(|_| ()))
        .with_context(|| format!("Could not write {}", out.display()))
}

/// The settings LibreOffice's profile gets before every run, so a document's macros never run:
/// macros off altogether, and the "very high" level that runs only those from trusted places
/// (there are none) should the first be ignored.
const NO_MACROS: [&str; 2] = [
    r#"<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="DisableMacrosExecution" oor:op="fuse"><value>true</value></prop></item>"#,
    r#"<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="MacroSecurityLevel" oor:op="fuse"><value>3</value></prop></item>"#,
];

/// Writes [`NO_MACROS`] into the profile's `user/registrymodifications.xcu`, replacing whatever
/// the two settings were, keeping every other line LibreOffice keeps there.
fn no_macros(profile: &Path) -> Result<()> {
    let file = profile.join("user").join("registrymodifications.xcu");
    let old = std::fs::read_to_string(&file).unwrap_or_default();
    let new = with_no_macros(&old);
    if new != old {
        std::fs::create_dir_all(file.parent().unwrap_or(profile))?;
        std::fs::write(&file, new)
            .with_context(|| format!("Could not set up LibreOffice in {}", profile.display()))?;
    }
    Ok(())
}

fn with_no_macros(xcu: &str) -> String {
    const END: &str = "</oor:items>";
    let setting = |line: &str| {
        line.contains("/Security/Scripting\"")
            && (line.contains("\"DisableMacrosExecution\"")
                || line.contains("\"MacroSecurityLevel\""))
    };
    if !xcu.contains(END) {
        return format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<oor:items xmlns:oor=\"http://openoffice.org/2001/registry\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\n{}\n{END}\n",
            NO_MACROS.join("\n")
        );
    }
    let mut out = String::with_capacity(xcu.len() + 400);
    for line in xcu.lines() {
        if NO_MACROS.contains(&line) || !setting(line) {
            if let Some(at) = line.find(END) {
                // Ours go in once, just before the end, whether or not they were there already.
                out.push_str(&line[..at]);
                for item in NO_MACROS {
                    if !xcu.lines().any(|l| l == item) {
                        out.push_str(item);
                        out.push('\n');
                    }
                }
                out.push_str(&line[at..]);
            } else {
                out.push_str(line);
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libre_office_profiles_turn_macros_off() {
        let fresh = with_no_macros("");
        for item in NO_MACROS {
            assert_eq!(fresh.matches(item).count(), 1, "{fresh}");
        }
        assert!(fresh.trim_end().ends_with("</oor:items>"));
        // Settled: nothing changes, so the file is not rewritten.
        assert_eq!(with_no_macros(&fresh), fresh);

        // A profile LibreOffice wrote, with macros at "low" and other settings to keep.
        let kept = r#"<item oor:path="/org.openoffice.Setup/Office"><prop oor:name="ooSetupInstCompleted" oor:op="fuse"><value>true</value></prop></item>"#;
        let low = r#"<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="MacroSecurityLevel" oor:op="fuse"><value>0</value></prop></item>"#;
        let written = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<oor:items xmlns:oor=\"http://openoffice.org/2001/registry\">\n{kept}\n{low}\n</oor:items>\n"
        );
        let fixed = with_no_macros(&written);
        assert!(fixed.contains(kept));
        assert!(!fixed.contains(low));
        for item in NO_MACROS {
            assert_eq!(fixed.matches(item).count(), 1, "{fixed}");
        }
        assert!(fixed.trim_end().ends_with("</oor:items>"));
        assert_eq!(with_no_macros(&fixed), fixed);

        let dir = tempfile::tempdir().unwrap();
        no_macros(dir.path()).unwrap();
        let file = dir.path().join("user").join("registrymodifications.xcu");
        assert_eq!(std::fs::read_to_string(file).unwrap(), fresh);
    }

    #[test]
    fn addresses_and_formats() {
        assert_eq!(
            file_url(Path::new(r"C:\My Files\a#1.html")),
            "file:///C:/My%20Files/a%231.html"
        );
        assert_eq!(office_format(App::Word, "pdf"), Some(17));
        assert_eq!(office_format(App::Excel, "pdf"), Some(-1));
        assert_eq!(office_format(App::PowerPoint, "docx"), None);
    }
}

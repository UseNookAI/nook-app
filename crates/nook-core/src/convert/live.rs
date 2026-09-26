//! The converter with the real engines: `NOOK_TEST_PDFIUM` names `pdfium.dll`, `NOOK_TEST_PANDOC`
//! `pandoc.exe`, `NOOK_TEST_SOFFICE` (optional) LibreOffice's `soffice.com`, `NOOK_TEST_OUT` a
//! folder for the results; Edge and, when installed, Word, Excel and PowerPoint are this
//! computer's: `cargo test -p nook-core converts_with_the_real_engines -- --ignored --nocapture`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::formats::by_id;
use super::routes::{self, App, Have, Step};
use super::run::{self, Kit};
use super::{system, tables};
use crate::pdf::PdfEditor;

fn env(name: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("set {name}")))
}

const REPORT: &str = "# Quarterly report

Sales rose **12%** over the quarter, led by the *north* region.

## Highlights

- New customers: 42
- Returning customers: 118

| Region | Sales |
|--------|------:|
| North  | 1,200 |
| South  |   950 |

Contact: sales@example.com
";

#[tokio::test]
#[ignore = "needs Pandoc, PDFium and this computer's Edge and Office"]
async fn converts_with_the_real_engines() {
    let out = env("NOOK_TEST_OUT");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let dll = env("NOOK_TEST_PDFIUM");
    let kit = Kit {
        pandoc: Some(env("NOOK_TEST_PANDOC")),
        soffice: std::env::var("NOOK_TEST_SOFFICE").ok().map(PathBuf::from),
        edge: system::edge(),
        pdf: Arc::new(PdfEditor::new(move || Some(dll.clone()))),
        office_profile: out.join("profiles").join("office"),
        edge_profile: out.join("profiles").join("edge"),
    };
    let ms = system::microsoft_office();
    println!("Microsoft Office: {ms:?}; Edge: {:?}", kit.edge);
    let cancel = CancellationToken::new();
    let md = out.join("report.md");
    std::fs::write(&md, REPORT).unwrap();
    std::fs::write(
        out.join("prices.csv"),
        "Item;Price;Code\nTea;3.5;007\nCoffee;12;42\n",
    )
    .unwrap();
    let mut shot = image::RgbaImage::from_pixel(640, 400, image::Rgba([240, 244, 250, 255]));
    for x in 80..560 {
        for y in 120..280 {
            shot.put_pixel(x, y, image::Rgba([40, 90, 160, 255]));
        }
    }
    shot.save(out.join("shot.png")).unwrap();

    // What to make, from what, and whether the result is to be checked for this text.
    let mut cases: Vec<(&str, &str, Option<&str>)> = vec![
        ("report.md", "docx", None),
        ("report.md", "html", Some("Quarterly report")),
        ("report.md", "pdf", None),
        ("report.md", "epub", None),
        ("report.md", "pptx", None),
        ("report.docx", "md", Some("New customers: 42")),
        ("report.docx", "txt", Some("Sales rose 12%")),
        ("report.pdf", "txt", Some("Quarterly report")),
        ("report.pdf", "md", Some("# Quarterly report")),
        ("report.pdf", "png", None),
        ("report.pdf", "docx", None),
        ("report.html", "pdf", None),
        ("prices.csv", "xlsx", None),
        ("prices.xlsx", "json", Some("\"Item\": \"Coffee\"")),
        ("prices.csv", "pdf", None),
        ("prices.xlsx", "md", Some("| Tea | 3.5 | 007 |")),
        ("shot.png", "jpg", None),
        ("shot.png", "pdf", None),
        ("shot.jpg", "webp", None),
    ];
    if ms.word {
        cases.extend([
            ("report.docx", "pdf", None),
            ("report.docx", "odt", None),
            ("report.docx", "rtf", None),
            ("report.docx", "doc", None),
            ("report.doc", "md", Some("New customers")),
        ]);
    }
    if ms.excel {
        cases.extend([("prices.xlsx", "pdf", None), ("prices.xlsx", "ods", None)]);
    }
    if ms.powerpoint {
        cases.extend([("report.pptx", "pdf", None), ("report.pptx", "png", None)]);
    }
    // The same through LibreOffice, when there is one.
    let libre = kit.soffice.is_some();
    let mut failures = Vec::new();
    for (from, to, expect) in cases {
        let input = out.join(from);
        let f = super::formats::of_path(&input).unwrap();
        let steps = routes::route(f, by_id(to).unwrap(), &ms).expect("a route");
        // Each result in a folder of its own; the first of a name is also put beside the
        // sources, for the cases after it that start from it.
        let name = format!(
            "{}.{}",
            input.file_stem().unwrap().to_string_lossy(),
            by_id(to).unwrap().ext
        );
        let made = out.join("made").join(format!("{from} to {to}"));
        std::fs::create_dir_all(&made).unwrap();
        let target = made.join(&name);
        let work = out.join("work").join(format!("{from}-{to}"));
        let started = std::time::Instant::now();
        match run::convert(&kit, &steps, &input, &target, &work, &cancel).await {
            Ok(files) => {
                let size: u64 = files
                    .iter()
                    .map(|p| std::fs::metadata(p).map_or(0, |m| m.len()))
                    .sum();
                println!(
                    "{from} -> {to}: {} file(s), {size} bytes, {} ms ({steps:?})",
                    files.len(),
                    started.elapsed().as_millis()
                );
                if !out.join(&name).exists() {
                    std::fs::copy(&files[0], out.join(&name)).unwrap();
                }
                if let Some(want) = expect {
                    let text = std::fs::read_to_string(&files[0]).unwrap_or_default();
                    if !text.contains(want) {
                        failures.push(format!(
                            "{from} -> {to}: no {want:?} in {:?}",
                            &text[..text.len().min(300)]
                        ));
                    }
                }
            }
            Err(e) => failures.push(format!("{from} -> {to}: {e:#}")),
        }
    }
    if libre {
        for (from, to) in [
            ("report.docx", "pdf"),
            ("prices.xlsx", "ods"),
            ("report.pptx", "odp"),
        ] {
            let input = out.join(from);
            let target = out.join(format!(
                "libre-{}.{to}",
                input.file_stem().unwrap().to_string_lossy()
            ));
            let steps = [Step::Office {
                app: App::Libre,
                to,
            }];
            let started = std::time::Instant::now();
            match run::convert(
                &kit,
                &steps,
                &input,
                &target,
                &out.join("work").join("libre"),
                &cancel,
            )
            .await
            {
                Ok(files) => println!(
                    "LibreOffice {from} -> {to}: {:?} in {} ms",
                    files,
                    started.elapsed().as_millis()
                ),
                Err(e) => failures.push(format!("LibreOffice {from} -> {to}: {e:#}")),
            }
        }
    }
    // The workbook made from the CSV reads back with its numbers as numbers.
    let back = tables::read(&out.join("prices.xlsx"), "xlsx").unwrap();
    assert_eq!(back[0].rows[1][1], tables::Cell::Number(3.5));
    let _ = Have::default();
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
    let _ = Path::new("");
}

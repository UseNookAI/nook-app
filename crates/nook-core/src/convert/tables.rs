//! Tables, converted by Nook itself: workbooks (Excel's and OpenDocument's, read with calamine),
//! CSV and TSV, and JSON rows, written as CSV, TSV, JSON, an Excel workbook (rust_xlsxwriter),
//! a web page or Markdown.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{Map, Value};

/// A sheet: its name and its rows, as read.
#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    pub name: String,
    pub rows: Vec<Vec<Cell>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Empty,
    Text(String),
    Number(f64),
    Bool(bool),
    /// A date or time, written as ISO 8601.
    Date(String),
}

impl Cell {
    /// The cell as text: numbers without a needless ".0".
    pub fn text(&self) -> String {
        match self {
            Cell::Empty => String::new(),
            Cell::Text(t) | Cell::Date(t) => t.clone(),
            Cell::Number(n) => number(*n),
            Cell::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
        }
    }

    fn json(&self) -> Value {
        match self {
            Cell::Empty => Value::Null,
            Cell::Text(t) | Cell::Date(t) => Value::String(t.clone()),
            Cell::Number(n) => serde_json::Number::from_f64(*n).map_or(Value::Null, Value::Number),
            Cell::Bool(b) => Value::Bool(*b),
        }
    }
}

fn number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Text read from a CSV that is a number as a spreadsheet would take it: not one with a leading
/// zero (a postcode, a phone number) or a plus sign.
fn as_number(t: &str) -> Option<f64> {
    let s = t.trim();
    if s.is_empty() || s.starts_with('+') || s.len() > 15 {
        return None;
    }
    let digits = s.trim_start_matches('-');
    if digits.len() > 1 && digits.starts_with('0') && !digits.starts_with("0.") {
        return None;
    }
    s.parse::<f64>().ok().filter(|n| n.is_finite())
}

// ------------------------------------------------------------------ reading

/// The sheets of a table file (`format` its id: xlsx, xls, ods, csv, tsv or json).
pub fn read(path: &Path, format: &str) -> Result<Vec<Sheet>> {
    match format {
        "xlsx" | "xls" | "ods" => read_workbook(path),
        "csv" | "tsv" => {
            let text = read_text(path)?;
            let delimiter = if format == "tsv" { b'\t' } else { sniff(&text) };
            Ok(vec![Sheet {
                name: stem(path),
                rows: read_delimited(&text, delimiter)?,
            }])
        }
        "json" => read_json(path),
        other => bail!("{other} is not a table Nook reads"),
    }
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Sheet1".into())
}

/// A text file as UTF-8 (its byte-order mark dropped), or as Windows-1252 when it is not.
fn read_text(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("Could not read {}", path.display()))?;
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    Ok(match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    })
}

/// The delimiter of a CSV: a semicolon when its first line has more of them than commas, as
/// spreadsheets in many countries write it.
fn sniff(text: &str) -> u8 {
    let first = text.lines().next().unwrap_or("");
    let (mut commas, mut semis, mut quoted) = (0, 0, false);
    for c in first.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => commas += 1,
            ';' if !quoted => semis += 1,
            _ => {}
        }
    }
    if semis > commas {
        b';'
    } else {
        b','
    }
}

fn read_delimited(text: &str, delimiter: u8) -> Result<Vec<Vec<Cell>>> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.context("The file is not valid CSV")?;
        rows.push(
            record
                .iter()
                .map(|f| {
                    if f.is_empty() {
                        Cell::Empty
                    } else {
                        Cell::Text(f.to_string())
                    }
                })
                .collect(),
        );
    }
    Ok(rows)
}

fn read_workbook(path: &Path) -> Result<Vec<Sheet>> {
    use calamine::{open_workbook_auto, Data, Reader};
    let mut book =
        open_workbook_auto(path).map_err(|e| anyhow!("Could not open {}: {e}", path.display()))?;
    let mut out = Vec::new();
    for name in book.sheet_names() {
        let range = book
            .worksheet_range(&name)
            .map_err(|e| anyhow!("Could not read the sheet {name}: {e}"))?;
        let rows = range
            .rows()
            .map(|row| {
                row.iter()
                    .map(|d| match d {
                        Data::Empty => Cell::Empty,
                        Data::String(s) => Cell::Text(s.clone()),
                        Data::Int(i) => Cell::Number(*i as f64),
                        Data::Float(f) => Cell::Number(*f),
                        Data::Bool(b) => Cell::Bool(*b),
                        Data::DateTime(dt) => match dt.as_datetime() {
                            Some(t) if t.time() == chrono::NaiveTime::MIN => {
                                Cell::Date(t.date().to_string())
                            }
                            Some(t) => Cell::Date(t.format("%Y-%m-%dT%H:%M:%S").to_string()),
                            None => Cell::Number(dt.as_f64()),
                        },
                        Data::DateTimeIso(s) | Data::DurationIso(s) => Cell::Date(s.clone()),
                        Data::Error(e) => Cell::Text(format!("#{e:?}")),
                    })
                    .collect()
            })
            .collect();
        out.push(Sheet { name, rows });
    }
    Ok(out)
}

fn read_json(path: &Path) -> Result<Vec<Sheet>> {
    let value: Value = serde_json::from_str(&read_text(path)?).context("The file is not JSON")?;
    match value {
        Value::Object(sheets) if sheets.values().all(Value::is_array) && !sheets.is_empty() => {
            sheets
                .into_iter()
                .map(|(name, rows)| {
                    Ok(Sheet {
                        name,
                        rows: json_rows(&rows)?,
                    })
                })
                .collect()
        }
        rows @ Value::Array(_) => Ok(vec![Sheet {
            name: stem(path),
            rows: json_rows(&rows)?,
        }]),
        _ => bail!("The JSON is not rows: an array of objects or of arrays"),
    }
}

/// Rows of JSON: objects (their keys, in the order met, become the first row) or arrays.
fn json_rows(rows: &Value) -> Result<Vec<Vec<Cell>>> {
    let items = rows
        .as_array()
        .ok_or_else(|| anyhow!("The JSON is not rows"))?;
    let cell = |v: &Value| match v {
        Value::Null => Cell::Empty,
        Value::Bool(b) => Cell::Bool(*b),
        Value::Number(n) => Cell::Number(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => Cell::Text(s.clone()),
        other => Cell::Text(other.to_string()),
    };
    if items.iter().all(Value::is_object) && !items.is_empty() {
        let mut keys: Vec<String> = Vec::new();
        for o in items.iter().filter_map(Value::as_object) {
            for k in o.keys() {
                if !keys.contains(k) {
                    keys.push(k.clone());
                }
            }
        }
        let mut out = vec![keys.iter().map(|k| Cell::Text(k.clone())).collect()];
        for o in items.iter().filter_map(Value::as_object) {
            out.push(
                keys.iter()
                    .map(|k| o.get(k).map_or(Cell::Empty, cell))
                    .collect(),
            );
        }
        Ok(out)
    } else {
        Ok(items
            .iter()
            .map(|r| match r {
                Value::Array(cells) => cells.iter().map(cell).collect(),
                other => vec![cell(other)],
            })
            .collect())
    }
}

// ------------------------------------------------------------------ writing

/// Writes `sheets` as `to` at `out`. CSV, TSV and JSON hold one sheet a file: a workbook of
/// several becomes several files beside `out`, "<name> - <sheet>.csv". Returns what was written.
pub fn write(sheets: &[Sheet], to: &str, out: &Path) -> Result<Vec<PathBuf>> {
    if sheets.is_empty() {
        bail!("There is no table in the file");
    }
    match to {
        "xlsx" => {
            write_xlsx(sheets, out)?;
            Ok(vec![out.to_path_buf()])
        }
        "html" => {
            std::fs::write(out, html(sheets, &stem(out)))?;
            Ok(vec![out.to_path_buf()])
        }
        "md" => {
            std::fs::write(out, markdown(sheets))?;
            Ok(vec![out.to_path_buf()])
        }
        "csv" | "tsv" | "json" => {
            let mut written = Vec::new();
            for s in sheets {
                let path = if sheets.len() == 1 {
                    out.to_path_buf()
                } else {
                    out.with_file_name(format!("{} - {}.{to}", stem(out), safe_name(&s.name)))
                };
                let body = match to {
                    "json" => serde_json::to_string_pretty(&json(s))?,
                    "tsv" => delimited(s, b'\t', false)?,
                    _ => delimited(s, b',', true)?,
                };
                std::fs::write(&path, body)
                    .with_context(|| format!("Could not write {}", path.display()))?;
                written.push(path);
            }
            Ok(written)
        }
        other => bail!("{other} is not a table Nook writes"),
    }
}

fn safe_name(name: &str) -> String {
    name.chars()
        .map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// CSV (with a byte-order mark, so Excel reads it as UTF-8) or TSV.
fn delimited(s: &Sheet, delimiter: u8, bom: bool) -> Result<String> {
    let mut w = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .from_writer(Vec::new());
    for row in &s.rows {
        w.write_record(row.iter().map(Cell::text))?;
    }
    let body = String::from_utf8(w.into_inner().map_err(|e| anyhow!("{e}"))?)?;
    Ok(if bom { format!("\u{feff}{body}") } else { body })
}

/// Rows as objects keyed by the first row when it is a header (text, none empty, none twice),
/// else as arrays.
fn json(s: &Sheet) -> Value {
    let header: Option<Vec<String>> = s.rows.first().and_then(|h| {
        let names: Vec<String> = h
            .iter()
            .map(|c| match c {
                Cell::Text(t) if !t.trim().is_empty() => Some(t.trim().to_string()),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        let mut seen = std::collections::HashSet::new();
        names.iter().all(|n| seen.insert(n)).then_some(names)
    });
    match header {
        Some(keys) => Value::Array(
            s.rows[1..]
                .iter()
                .map(|r| {
                    let mut o = Map::new();
                    for (i, k) in keys.iter().enumerate() {
                        o.insert(k.clone(), r.get(i).map_or(Value::Null, Cell::json));
                    }
                    Value::Object(o)
                })
                .collect(),
        ),
        None => Value::Array(
            s.rows
                .iter()
                .map(|r| Value::Array(r.iter().map(Cell::json).collect()))
                .collect(),
        ),
    }
}

fn write_xlsx(sheets: &[Sheet], out: &Path) -> Result<()> {
    use rust_xlsxwriter::Workbook;
    let mut book = Workbook::new();
    let mut used: Vec<String> = Vec::new();
    for s in sheets {
        // Excel's rules for a sheet name: 31 characters, none of []:*?/\, not twice.
        let mut name: String = s
            .name
            .chars()
            .map(|c| if "[]:*?/\\".contains(c) { '_' } else { c })
            .take(31)
            .collect();
        if name.trim().is_empty() {
            name = format!("Sheet{}", used.len() + 1);
        }
        let base = name.clone();
        let mut n = 2;
        while used.iter().any(|u| u.eq_ignore_ascii_case(&name)) {
            let tail = format!(" ({n})");
            name = format!(
                "{}{tail}",
                base.chars().take(31 - tail.len()).collect::<String>()
            );
            n += 1;
        }
        used.push(name.clone());
        let sheet = book.add_worksheet();
        sheet.set_name(&name).map_err(|e| anyhow!("{e}"))?;
        for (r, row) in s.rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                let (r, c) = (r as u32, c as u16);
                let written = match cell {
                    Cell::Empty => continue,
                    Cell::Number(n) => sheet.write_number(r, c, *n).map(|_| ()),
                    Cell::Bool(b) => sheet.write_boolean(r, c, *b).map(|_| ()),
                    Cell::Text(t) => match as_number(t) {
                        Some(n) => sheet.write_number(r, c, n).map(|_| ()),
                        None => sheet.write_string(r, c, t).map(|_| ()),
                    },
                    Cell::Date(t) => sheet.write_string(r, c, t).map(|_| ()),
                };
                written.map_err(|e| anyhow!("{e}"))?;
            }
        }
        sheet.autofit();
    }
    book.save(out)
        .map_err(|e| anyhow!("Could not write {}: {e}", out.display()))
}

fn escape(t: &str) -> String {
    t.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A page of tables, a heading for each sheet when there are several, set for print too.
pub fn html(sheets: &[Sheet], title: &str) -> String {
    let mut body = String::new();
    for s in sheets {
        if sheets.len() > 1 {
            body.push_str(&format!("<h2>{}</h2>\n", escape(&s.name)));
        }
        body.push_str("<table>\n");
        for (i, row) in s.rows.iter().enumerate() {
            let tag = if i == 0 { "th" } else { "td" };
            body.push_str("<tr>");
            for c in row {
                let class = if matches!(c, Cell::Number(_)) || as_number(&c.text()).is_some() {
                    " class=\"n\""
                } else {
                    ""
                };
                body.push_str(&format!("<{tag}{class}>{}</{tag}>", escape(&c.text())));
            }
            body.push_str("</tr>\n");
        }
        body.push_str("</table>\n");
    }
    format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>{}</title><style>\n\
         body {{ font: 10pt/1.4 'Segoe UI', Arial, sans-serif; color: #1a1a1a; margin: 24px; }}\n\
         h2 {{ font-size: 13pt; margin: 24px 0 8px; }}\n\
         table {{ border-collapse: collapse; margin-bottom: 16px; }}\n\
         th, td {{ border: 1px solid #c8c8c8; padding: 3px 8px; text-align: left; vertical-align: top; }}\n\
         th {{ background: #f0f0f0; font-weight: 600; }}\n\
         td.n {{ text-align: right; font-variant-numeric: tabular-nums; }}\n\
         @page {{ margin: 14mm; }}\n\
         </style></head><body>\n{body}</body></html>\n",
        escape(title)
    )
}

fn markdown(sheets: &[Sheet]) -> String {
    let cell = |c: &Cell| c.text().replace('|', "\\|").replace('\n', " ");
    let mut out = String::new();
    for s in sheets {
        if sheets.len() > 1 {
            out.push_str(&format!("## {}\n\n", s.name));
        }
        let width = s.rows.iter().map(Vec::len).max().unwrap_or(0);
        if width == 0 {
            continue;
        }
        for (i, row) in s.rows.iter().enumerate() {
            let cells: Vec<String> = (0..width)
                .map(|c| row.get(c).map_or(String::new(), cell))
                .collect();
            out.push_str(&format!("| {} |\n", cells.join(" | ")));
            if i == 0 {
                out.push_str(&format!("|{}\n", " --- |".repeat(width)));
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_semicolon_csv_becomes_a_workbook_and_back() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("prices.csv");
        std::fs::write(
            &csv,
            "\u{feff}Item;Price;Code\n\"Tea; green\";3.5;007\nCoffee;12;42\n",
        )
        .unwrap();
        let sheets = read(&csv, "csv").unwrap();
        assert_eq!(sheets[0].rows[1][0], Cell::Text("Tea; green".into()));
        let xlsx = dir.path().join("prices.xlsx");
        assert_eq!(
            write(&sheets, "xlsx", &xlsx).unwrap(),
            std::slice::from_ref(&xlsx)
        );
        let back = read(&xlsx, "xlsx").unwrap();
        assert_eq!(back[0].name, "prices");
        assert_eq!(
            back[0].rows[1][1],
            Cell::Number(3.5),
            "a number is a number"
        );
        assert_eq!(
            back[0].rows[1][2],
            Cell::Text("007".into()),
            "a code with a leading zero stays text"
        );
        let json_path = dir.path().join("prices.json");
        write(&back, "json", &json_path).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
        assert_eq!(v[1]["Item"], "Coffee");
        assert_eq!(v[1]["Price"], 12.0);
        let again = read(&json_path, "json").unwrap();
        assert_eq!(again[0].rows[0][0], Cell::Text("Item".into()));
    }

    #[test]
    fn a_workbook_of_two_sheets_becomes_two_csvs_or_one_page() {
        let dir = tempfile::tempdir().unwrap();
        let sheets = vec![
            Sheet {
                name: "Q1".into(),
                rows: vec![vec![Cell::Text("a".into())], vec![Cell::Number(1.0)]],
            },
            Sheet {
                name: "Q2/Q3".into(),
                rows: vec![vec![Cell::Text("b<".into())]],
            },
        ];
        let out = dir.path().join("book.csv");
        let written = write(&sheets, "csv", &out).unwrap();
        assert_eq!(
            written,
            [
                dir.path().join("book - Q1.csv"),
                dir.path().join("book - Q2_Q3.csv")
            ]
        );
        assert_eq!(
            std::fs::read_to_string(&written[0]).unwrap(),
            "\u{feff}a\n1\n"
        );
        let page = html(&sheets, "book");
        assert!(page.contains("<h2>Q2/Q3</h2>") && page.contains("b&lt;"));
        assert!(markdown(&sheets).contains("| a |\n| --- |\n| 1 |"));
    }
}

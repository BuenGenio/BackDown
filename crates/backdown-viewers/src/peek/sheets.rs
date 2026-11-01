//! Spreadsheets, a page of one sheet at a time (calamine: xlsx/xlsb stream cell by cell, xls/ods load
//! the one sheet).

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use backdown_core::fmt;
use backdown_core::magic::read_at;
use serde_json::{Value, json};

use super::*;

// ---- spreadsheets (calamine: xlsx/xlsb stream cell by cell, xls/ods load the one sheet)

pub(super) const XLSX_CAP: u64 = 256 << 20;
pub(super) const XLS_CAP: u64 = 64 << 20;
/// Parts calamine holds in memory whole (unpacked sizes): shared strings, styles, and an ODS
/// file's content.xml (every sheet).
pub(super) const HELD_PARTS: [(&str, u64); 6] = [
    ("xl/sharedStrings.xml", 128 << 20),
    ("xl/sharedStrings.bin", 128 << 20),
    ("xl/styles.xml", 32 << 20),
    ("xl/styles.bin", 32 << 20),
    ("xl/workbook.xml", 32 << 20),
    ("content.xml", 64 << 20),
];

/// One page of a sheet: rows `[lo, hi)`, the first `MAX_COLS` columns.
pub(super) struct Page {
    pub(super) lo: u64,
    pub(super) hi: u64,
    pub(super) rows: Vec<Vec<Value>>,
    pub(super) width: usize,
    pub(super) more: bool,
    pub(super) cut: bool,
    pub(super) early: bool,
    pub(super) seen: u64,
    pub(super) until: Instant,
}

impl Page {
    /// Places one cell; false once the page is full (or the time budget spent).
    pub(super) fn put(&mut self, (r, c): (u32, u32), v: &calamine::Data) -> bool {
        self.seen += 1;
        if self.seen.is_multiple_of(4096) && Instant::now() > self.until {
            self.more = true;
            self.early = true;
            return false;
        }
        let (r, c) = (u64::from(r), c as usize);
        if r < self.lo {
            return true;
        }
        if r >= self.hi {
            self.more = true;
            return false;
        }
        if matches!(v, calamine::Data::Empty) {
            return true;
        }
        if c >= MAX_COLS {
            self.cut = true;
            return true;
        }
        let i = (r - self.lo) as usize;
        if self.rows.len() <= i {
            self.rows.resize(i + 1, Vec::new());
        }
        let row = &mut self.rows[i];
        if row.len() <= c {
            row.resize(c + 1, Value::Null);
        }
        row[c] = cell_json(v);
        self.width = self.width.max(c + 1);
        true
    }
}

pub(super) fn cell_json(d: &calamine::Data) -> Value {
    use calamine::Data;
    match d {
        Data::Empty => Value::Null,
        Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => json!(clip(s, 4000)),
        Data::Float(f) => json!(f),
        Data::Int(i) => json!(i),
        Data::Bool(b) => json!(b),
        Data::Error(e) => json!(e.to_string()),
        Data::DateTime(dt) => {
            let (y, mo, d, h, mi, s, _) = dt.to_ymd_hms_milli();
            let v = dt.as_f64();
            json!(if dt.is_duration() {
                let secs = (v * 86_400.0).round() as i64;
                format!("{}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
            } else if v < 1.0 {
                format!("{h:02}:{mi:02}:{s:02}")
            } else if (h, mi, s) == (0, 0, 0) {
                format!("{y:04}-{mo:02}-{d:02}")
            } else {
                format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
            })
        }
    }
}

/// Sheet names and one page of a sheet's rows (`offset` counts from the sheet's first row, 0-based).
pub fn sheet(path: &Path, want: Option<&str>, offset: u64, limit: u64) -> Result<Value> {
    use calamine::{Reader, SheetType, SheetVisible, Sheets, open_workbook};
    let size = std::fs::metadata(path)?.len();
    let magic = read_at(&File::open(path)?, 0, 4)?;
    let too_big = |cap: u64| -> Result<()> {
        if size > cap {
            bail!(
                "too large to open here ({}; the limit for this format is {})",
                fmt::bytes(size),
                fmt::bytes(cap)
            );
        }
        Ok(())
    };
    let mut book: Sheets<BufReader<File>> = if magic == [0xD0, 0xCF, 0x11, 0xE0] {
        too_big(XLS_CAP)?;
        Sheets::Xls(
            open_workbook(path).map_err(|e| anyhow::anyhow!("can't read this workbook: {e}"))?,
        )
    } else if magic.starts_with(b"PK") {
        too_big(XLSX_CAP)?;
        // look at the unpacked sizes first, so a zip bomb is refused rather than inflated
        let z = Zip::open(path)?;
        for (part, cap) in HELD_PARTS {
            if let Some(e) = z.get(part)
                && e.size > cap
            {
                bail!(
                    "too large to open here ({part} unpacks to {})",
                    fmt::bytes(e.size)
                );
            }
        }
        if let Ok(b) = open_workbook(path) {
            Sheets::Xlsx(b)
        } else if let Ok(b) = open_workbook(path) {
            Sheets::Xlsb(b)
        } else {
            Sheets::Ods(
                open_workbook(path)
                    .map_err(|e| anyhow::anyhow!("not a spreadsheet BackDown can read: {e}"))?,
            )
        }
    } else {
        bail!("not a spreadsheet BackDown can read");
    };
    let sheets: Vec<(String, bool)> = book
        .sheets_metadata()
        .iter()
        .filter(|s| s.typ == SheetType::WorkSheet)
        .map(|s| (s.name.clone(), s.visible != SheetVisible::Visible))
        .collect();
    let name = want
        .filter(|w| sheets.iter().any(|(n, _)| n == w))
        .map(str::to_string)
        .or_else(|| {
            sheets
                .iter()
                .find(|(_, hidden)| !hidden)
                .or(sheets.first())
                .map(|(n, _)| n.clone())
        })
        .context("this workbook has no worksheets")?;
    let limit = limit.clamp(1, SHEET_ROWS);
    let mut page = Page {
        lo: offset,
        hi: offset.saturating_add(limit),
        rows: Vec::new(),
        width: 0,
        more: false,
        cut: false,
        early: false,
        seen: 0,
        until: Instant::now() + LIST_BUDGET,
    };
    let mut dims = None;
    macro_rules! stream {
        ($cells:expr) => {{
            let mut cells = $cells;
            let d = cells.dimensions();
            if d.end != (0, 0) {
                dims = Some((u64::from(d.end.0) + 1, u64::from(d.end.1) + 1));
            }
            while let Some(c) = cells.next_cell()? {
                if !page.put(
                    c.get_position(),
                    &calamine::Data::from(c.get_value().clone()),
                ) {
                    break;
                }
            }
            true
        }};
    }
    let streamed = match &mut book {
        Sheets::Xlsx(x) => stream!(x.worksheet_cells_reader(&name)?),
        Sheets::Xlsb(x) => stream!(x.worksheet_cells_reader(&name)?),
        _ => false,
    };
    if !streamed {
        let range = book.worksheet_range(&name)?;
        if let (Some(s), Some(e)) = (range.start(), range.end()) {
            dims = Some((u64::from(e.0) + 1, u64::from(e.1) + 1));
            let last_col = e.1.min(MAX_COLS as u32 - 1);
            for r in page.lo.max(u64::from(s.0))..page.hi.min(u64::from(e.0) + 1) {
                for c in s.1..=last_col {
                    if let Some(v) = range.get_value((r as u32, c)) {
                        page.put((r as u32, c), v);
                    }
                }
            }
            page.more = u64::from(e.0) + 1 > page.hi;
            page.cut = e.1 as usize >= MAX_COLS;
        }
    }
    let width = dims
        .map_or(0, |(_, cols)| (cols as usize).min(MAX_COLS))
        .max(page.width);
    Ok(json!({
        "sheets": sheets.iter().map(|(n, h)| json!({ "name": n, "hidden": h })).collect::<Vec<_>>(),
        "sheet": name,
        "offset": offset,
        "rows": page.rows,
        "width": width,
        "total_rows": dims.map(|d| d.0),
        "total_cols": dims.map(|d| d.1),
        "more": page.more,
        "cut": page.cut,
        "early": page.early,
    }))
}

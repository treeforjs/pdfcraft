//! Export a PDF ▸ Excel (.xlsx, SpreadsheetML).
//!
//! One worksheet per page, named "Page N". Each page is written top to bottom in the same reading
//! order as the Word export: a paragraph is one cell in column A, and every table that
//! [`super::tables`] detects from the positioned text becomes a block of rows on the grid it
//! found, one spreadsheet column per detected table column (spanning cells merged), followed by
//! an empty row. Images are left out. Columns are sized from the widest detected table column.
//!
//! A cell whose whole text is an unambiguous number is stored as a number, with a number format
//! that shows it as it was printed (decimals, thousands separators, a trailing `%`, negatives in
//! parentheses). Numbers are read with `.` as the decimal point and `,` as the thousands
//! separator in groups of three; everything else (currency symbols, `1.234,56`, leading zeros
//! such as `007`, more than 15 significant digits, dates) stays text, so nothing is guessed.
//!
//! Bounds: at most [`MAX_SHEETS`] worksheets (the pages after that are appended to the last
//! sheet, each after a "Page N" row, so nothing is dropped), at most Excel's 1,048,576 rows per
//! sheet (later rows are left out) and 32,767 characters per cell (Excel's limit; the rest is
//! cut). Strings are written inline, so no text can become a formula.

use super::{Item, Page, Zip, esc, items};

/// More worksheets than this make a workbook slow to open; later pages share the last sheet.
pub const MAX_SHEETS: usize = 1024;
/// Excel's row limit per worksheet.
const MAX_ROWS: u32 = 1_048_576;
/// Excel's limit on characters in one cell.
const MAX_CELL_CHARS: usize = 32_767;
/// Excel's column limit (detected tables have at most 63 columns, so this is a guard).
const MAX_COLUMNS: usize = 16_384;

/// How a number was printed, so the cell shows it the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NumberLook {
    grouped: bool,
    decimals: u8,
    percent: bool,
    parens: bool,
}

impl NumberLook {
    /// The Excel number format, or `None` when General shows the value as printed.
    fn format_code(self) -> Option<String> {
        if !self.grouped && self.decimals == 0 && !self.percent && !self.parens {
            return None;
        }
        let mut base = String::from(if self.grouped { "#,##0" } else { "0" });
        if self.decimals > 0 {
            base.push('.');
            base.extend(std::iter::repeat_n('0', self.decimals as usize));
        }
        if self.percent {
            base.push('%');
        }
        Some(if self.parens { format!("{base}_);({base})") } else { base })
    }
}

/// The value of a cell whose whole text is an unambiguous number (see the module notes).
pub(crate) fn number(text: &str) -> Option<(f64, NumberLook)> {
    let mut s = text.trim();
    let percent = s.ends_with('%');
    if percent {
        s = s[..s.len() - 1].trim_end();
    }
    let parens = s.starts_with('(') && s.ends_with(')') && s.len() >= 2;
    let mut negative = parens;
    if parens {
        s = &s[1..s.len() - 1];
    } else if let Some(rest) = s.strip_prefix('-').or_else(|| s.strip_prefix('\u{2212}')) {
        negative = true;
        s = rest;
    }
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s, None),
    };
    let all_digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    if let Some(f) = frac
        && !all_digits(f)
    {
        return None;
    }
    let grouped = int.contains(',');
    let mut digits = String::new();
    if grouped {
        let mut groups = int.split(',');
        let first = groups.next()?;
        if !all_digits(first) || first.len() > 3 || first.starts_with('0') {
            return None;
        }
        digits.push_str(first);
        for g in groups {
            if g.len() != 3 || !all_digits(g) {
                return None;
            }
            digits.push_str(g);
        }
    } else {
        // "007" is an identifier, not seven.
        if !all_digits(int) || (int.len() > 1 && int.starts_with('0')) {
            return None;
        }
        digits.push_str(int);
    }
    let frac = frac.unwrap_or("");
    // Beyond 15 significant digits a double changes the value (account numbers, IDs).
    if digits.trim_start_matches('0').len() + frac.len() > 15 {
        return None;
    }
    // Parse the printed digits directly (scaled for a percentage) so the value is correctly rounded.
    let literal = format!("{digits}.{}e{}", if frac.is_empty() { "0" } else { frac }, if percent { -2 } else { 0 });
    let v: f64 = literal.parse().ok()?;
    if !v.is_finite() {
        return None;
    }
    let v = if negative && v != 0.0 { -v } else { v };
    Some((v, NumberLook { grouped, decimals: frac.len() as u8, percent, parens }))
}

/// The column's letters: 0 → A, 25 → Z, 26 → AA.
fn column_name(mut i: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (i % 26) as u8);
        if i < 26 {
            break;
        }
        i = i / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// Cell styles: font (bold/italic) × number format, each written once to `styles.xml`.
#[derive(Default)]
struct Styles {
    /// (font index 0..4, number format code); index 0 is the default.
    xfs: Vec<(u8, Option<String>)>,
    formats: Vec<String>,
}

impl Styles {
    fn id(&mut self, bold: bool, italic: bool, format: Option<String>) -> usize {
        let font = u8::from(bold) | u8::from(italic) << 1;
        if font == 0 && format.is_none() {
            return 0;
        }
        if let Some(f) = &format
            && !self.formats.contains(f)
        {
            self.formats.push(f.clone());
        }
        let key = (font, format);
        match self.xfs.iter().position(|x| *x == key) {
            Some(i) => i + 1,
            None => {
                self.xfs.push(key);
                self.xfs.len()
            }
        }
    }

    fn xml(&self) -> String {
        let mut s = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
        );
        if !self.formats.is_empty() {
            s.push_str(&format!("<numFmts count=\"{}\">", self.formats.len()));
            for (i, f) in self.formats.iter().enumerate() {
                s.push_str(&format!("<numFmt numFmtId=\"{}\" formatCode=\"{}\"/>", 164 + i, esc(f)));
            }
            s.push_str("</numFmts>");
        }
        s.push_str("<fonts count=\"4\">");
        for (b, i) in [("", ""), ("<b/>", ""), ("", "<i/>"), ("<b/>", "<i/>")] {
            s.push_str(&format!("<font>{b}{i}<sz val=\"11\"/><name val=\"Calibri\"/><family val=\"2\"/></font>"));
        }
        s.push_str(
            "</fonts><fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill></fills>\
<borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders>\
<cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>",
        );
        s.push_str(&format!("<cellXfs count=\"{}\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/>", self.xfs.len() + 1));
        for (font, format) in &self.xfs {
            let fmt = format.as_ref().and_then(|f| self.formats.iter().position(|e| e == f)).map_or(0, |i| 164 + i);
            let apply_fmt = if fmt != 0 { " applyNumberFormat=\"1\"" } else { "" };
            let apply_font = if *font != 0 { " applyFont=\"1\"" } else { "" };
            s.push_str(&format!("<xf numFmtId=\"{fmt}\" fontId=\"{font}\" fillId=\"0\" borderId=\"0\" xfId=\"0\"{apply_fmt}{apply_font}/>"));
        }
        s.push_str("</cellXfs><cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles></styleSheet>");
        s
    }
}

/// One worksheet being written.
#[derive(Default)]
struct Sheet {
    rows: String,
    merges: Vec<String>,
    /// Widths in points of the detected table columns, by spreadsheet column.
    widths: Vec<f64>,
    /// The next row number (1-based).
    next: u32,
    row: String,
}

impl Sheet {
    fn new() -> Sheet {
        Sheet { next: 1, ..Sheet::default() }
    }

    fn room(&self) -> bool {
        self.next <= MAX_ROWS
    }

    fn cell(&mut self, styles: &mut Styles, column: usize, text: &str, bold: bool, italic: bool) {
        let text = text.trim();
        if text.is_empty() || column >= MAX_COLUMNS || !self.room() {
            return;
        }
        let r = format!("{}{}", column_name(column), self.next);
        match number(text) {
            Some((v, look)) => {
                let s = styles.id(bold, italic, look.format_code());
                let s = if s == 0 { String::new() } else { format!(" s=\"{s}\"") };
                self.row.push_str(&format!("<c r=\"{r}\"{s}><v>{v}</v></c>"));
            }
            None => {
                let s = styles.id(bold, italic, None);
                let s = if s == 0 { String::new() } else { format!(" s=\"{s}\"") };
                let cut: String = text.chars().take(MAX_CELL_CHARS).collect();
                self.row.push_str(&format!("<c r=\"{r}\" t=\"inlineStr\"{s}><is><t xml:space=\"preserve\">{}</t></is></c>", esc(&cut)));
            }
        }
    }

    /// Close the current row (an empty row is just skipped over).
    fn end_row(&mut self) {
        if !self.room() {
            return;
        }
        if !self.row.is_empty() {
            self.rows.push_str(&format!("<row r=\"{}\">{}</row>", self.next, self.row));
            self.row.clear();
        }
        self.next += 1;
    }

    fn page(&mut self, styles: &mut Styles, items: Vec<Item<'_>>) {
        for it in items {
            match it {
                Item::Para(b, _) => {
                    self.cell(styles, 0, &b.text, b.bold, b.italic);
                    self.end_row();
                }
                Item::Table(t) => {
                    let mut edges = t.cols.clone();
                    edges.push(t.rect[2]);
                    for (i, w) in edges.windows(2).enumerate() {
                        let w = (w[1] - w[0]).max(0.0);
                        match self.widths.get_mut(i) {
                            Some(e) => *e = e.max(w),
                            None => self.widths.push(w),
                        }
                    }
                    for row in &t.rows {
                        let mut column = 0;
                        for c in row {
                            let span = c.span.max(1);
                            self.cell(styles, column, &c.text, c.bold, c.italic);
                            if span > 1 && !c.text.trim().is_empty() && self.room() && column + span <= MAX_COLUMNS {
                                self.merges.push(format!("{}{n}:{}{n}", column_name(column), column_name(column + span - 1), n = self.next));
                            }
                            column += span;
                        }
                        self.end_row();
                    }
                    // An empty row keeps the table apart from what follows.
                    self.end_row();
                }
                Item::Img(_) | Item::PageBreak => {}
            }
        }
    }

    fn xml(&self) -> String {
        let mut s = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">",
        );
        if !self.widths.is_empty() {
            s.push_str("<cols>");
            for (i, w) in self.widths.iter().enumerate() {
                // About 5.25 pt per character of Calibri 11's digit width, within sensible bounds.
                let chars = if w.is_finite() { (w / 5.25).clamp(8.43, 100.0) } else { 8.43 };
                s.push_str(&format!("<col min=\"{n}\" max=\"{n}\" width=\"{chars:.2}\" customWidth=\"1\"/>", n = i + 1));
            }
            s.push_str("</cols>");
        }
        if self.rows.is_empty() {
            s.push_str("<sheetData/>");
        } else {
            s.push_str(&format!("<sheetData>{}</sheetData>", self.rows));
        }
        if !self.merges.is_empty() {
            s.push_str(&format!("<mergeCells count=\"{}\">", self.merges.len()));
            for m in &self.merges {
                s.push_str(&format!("<mergeCell ref=\"{m}\"/>"));
            }
            s.push_str("</mergeCells>");
        }
        s.push_str("</worksheet>");
        s
    }
}

/// An Excel workbook (.xlsx, Office Open XML SpreadsheetML): one worksheet per page with the
/// page's detected tables as cell grids (see the module notes).
pub fn xlsx(pages: &[Page], title: &str) -> Vec<u8> {
    let mut styles = Styles::default();
    let mut sheets: Vec<(String, Sheet)> = Vec::new();
    for (i, p) in pages.iter().enumerate() {
        let page_items = items(std::slice::from_ref(p));
        if i < MAX_SHEETS {
            let mut sheet = Sheet::new();
            sheet.page(&mut styles, page_items);
            sheets.push((format!("Page {}", i + 1), sheet));
        } else if let Some((name, sheet)) = sheets.last_mut() {
            if i == MAX_SHEETS {
                *name = format!("Pages {MAX_SHEETS}-{}", pages.len());
            }
            sheet.end_row();
            sheet.cell(&mut styles, 0, &format!("Page {}", i + 1), true, false);
            sheet.end_row();
            sheet.page(&mut styles, page_items);
        }
    }
    if sheets.is_empty() {
        sheets.push(("Sheet1".into(), Sheet::new()));
    }

    let mut types = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
<Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/>\
<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>",
    );
    let mut workbook = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets>",
    );
    let mut wb_rels = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rIdStyles\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>",
    );
    for (i, (name, _)) in sheets.iter().enumerate() {
        let n = i + 1;
        types.push_str(&format!(
            "<Override PartName=\"/xl/worksheets/sheet{n}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"
        ));
        workbook.push_str(&format!("<sheet name=\"{}\" sheetId=\"{n}\" r:id=\"rId{n}\"/>", esc(name)));
        wb_rels.push_str(&format!(
            "<Relationship Id=\"rId{n}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{n}.xml\"/>"
        ));
    }
    types.push_str("</Types>");
    workbook.push_str("</sheets></workbook>");
    wb_rels.push_str("</Relationships>");
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/></Relationships>";
    let core = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>{}</dc:title></cp:coreProperties>",
        esc(title)
    );
    let mut z = Zip::default();
    z.add("[Content_Types].xml", types.as_bytes(), true);
    z.add("_rels/.rels", rels.as_bytes(), true);
    z.add("docProps/core.xml", core.as_bytes(), true);
    z.add("xl/workbook.xml", workbook.as_bytes(), true);
    z.add("xl/_rels/workbook.xml.rels", wb_rels.as_bytes(), true);
    z.add("xl/styles.xml", styles.xml().as_bytes(), true);
    for (i, (_, sheet)) in sheets.iter().enumerate() {
        z.add(&format!("xl/worksheets/sheet{}.xml", i + 1), sheet.xml().as_bytes(), true);
    }
    z.finish()
}

#[cfg(test)]
mod tests {
    use super::super::Block;
    use super::*;

    fn look(grouped: bool, decimals: u8, percent: bool, parens: bool) -> NumberLook {
        NumberLook { grouped, decimals, percent, parens }
    }

    #[test]
    fn only_unambiguous_numbers_become_numbers() {
        assert_eq!(number("42"), Some((42.0, look(false, 0, false, false))));
        assert_eq!(number(" 37414.00 "), Some((37414.0, look(false, 2, false, false))));
        assert_eq!(number("1,234,567.5"), Some((1_234_567.5, look(true, 1, false, false))));
        assert_eq!(number("-0.25"), Some((-0.25, look(false, 2, false, false))));
        assert_eq!(number("\u{2212}3"), Some((-3.0, look(false, 0, false, false))));
        assert_eq!(number("(1,200.00)"), Some((-1200.0, look(true, 2, false, true))));
        assert_eq!(number("12.5%"), Some((0.125, look(false, 1, true, false))));
        assert_eq!(number("14.3%").map(|n| n.0), Some(0.143), "scaled from the digits, not by dividing");
        assert_eq!(number("0"), Some((0.0, look(false, 0, false, false))));
        assert_eq!(number("-0").map(|n| n.0.is_sign_negative()), Some(false));
        for text in [
            "", "-", "%", "()", "007", "1.234,56", "1,23", "12,3456", ",123", "0,123", "1.2.3", "$12", "12 kg", "1e5", "+5", ".5", "5.",
            "2026-10-11", "1234567890123456", "NaN", "inf", "--1", "(-1)", "１２",
        ] {
            assert_eq!(number(text), None, "{text:?} must stay text");
        }
        assert_eq!(look(true, 2, false, true).format_code().as_deref(), Some("#,##0.00_);(#,##0.00)"));
        assert_eq!(look(false, 0, true, false).format_code().as_deref(), Some("0%"));
        assert_eq!(look(false, 0, false, false).format_code(), None);
        assert_eq!([0, 25, 26, 51, 52, 701, 702].map(column_name), ["A", "Z", "AA", "AZ", "BA", "ZZ", "AAA"]);
    }

    /// One part of a package written by [`Zip`], inflated.
    fn part(zip: &[u8], name: &str) -> Option<String> {
        let at = |i: usize, n: usize| zip[i..i + n].iter().rev().fold(0usize, |v, b| v << 8 | *b as usize);
        let mut i = 0;
        while zip[i..].starts_with(b"PK\x03\x04") {
            let (size, name_len) = (at(i + 18, 4), at(i + 26, 2));
            let data = i + 30 + name_len;
            if &zip[i + 30..data] == name.as_bytes() {
                let mut s = String::new();
                std::io::Read::read_to_string(&mut flate2::read::DeflateDecoder::new(&zip[data..data + size]), &mut s).ok()?;
                return Some(s);
            }
            i = data + size;
        }
        None
    }

    fn block(t: &str, x: f64, y: f64, w: f64, bold: bool) -> Block {
        Block { text: t.into(), rect: [x, y, x + w, y + 12.0], size: 11.0, bold, italic: false, color: [0.0; 3] }
    }

    fn statement() -> Page {
        Page {
            width: 612.0,
            height: 792.0,
            blocks: vec![
                block("Quarterly statement for the Example & Co. account, printed in points.", 300.0, 700.0, 280.0, true),
                block("Item", 72.0, 620.0, 100.0, true),
                block("Qty", 232.0, 620.0, 60.0, true),
                block("Amount", 392.0, 620.0, 80.0, true),
                block("Widgets <A>", 72.0, 600.0, 100.0, false),
                block("007", 232.0, 600.0, 30.0, false),
                block("1,234.50", 392.0, 600.0, 60.0, false),
                block("Refund", 72.0, 580.0, 100.0, false),
                block("2", 232.0, 580.0, 30.0, false),
                block("(12.00)", 392.0, 580.0, 60.0, false),
                block("Discount and fees", 72.0, 560.0, 220.0, false),
                block("12.5%", 392.0, 560.0, 60.0, false),
            ],
            images: Vec::new(),
        }
    }

    #[test]
    fn tables_become_cell_grids_with_numbers_one_sheet_per_page() {
        let x = xlsx(&[statement(), Page::default()], "Statement");
        assert!(x.starts_with(b"PK\x03\x04"));
        let workbook = part(&x, "xl/workbook.xml").unwrap_or_default();
        assert!(workbook.contains("<sheet name=\"Page 1\" sheetId=\"1\" r:id=\"rId1\"/><sheet name=\"Page 2\" sheetId=\"2\" r:id=\"rId2\"/>"), "{workbook}");
        let sheet = part(&x, "xl/worksheets/sheet1.xml").unwrap_or_default();
        // The heading paragraph is A1, then the table starts on row 2.
        assert!(sheet.contains("<c r=\"A1\" t=\"inlineStr\" s=\"1\"><is><t xml:space=\"preserve\">Quarterly statement for the Example &amp; Co. account"), "{sheet}");
        assert!(sheet.contains("<c r=\"C2\" t=\"inlineStr\" s=\"1\"><is><t xml:space=\"preserve\">Amount</t>"), "{sheet}");
        assert!(sheet.contains("<c r=\"A3\" t=\"inlineStr\"><is><t xml:space=\"preserve\">Widgets &lt;A&gt;</t>"), "{sheet}");
        assert!(sheet.contains("<c r=\"B3\" t=\"inlineStr\"><is><t xml:space=\"preserve\">007</t>"), "leading zeros stay text: {sheet}");
        assert!(sheet.contains("<c r=\"C3\" s=\"2\"><v>1234.5</v></c>"), "{sheet}");
        assert!(sheet.contains("<c r=\"B4\"><v>2</v></c>"), "{sheet}");
        assert!(sheet.contains("<c r=\"C4\" s=\"3\"><v>-12</v></c>"), "{sheet}");
        assert!(sheet.contains("<c r=\"C5\" s=\"4\"><v>0.125</v></c>"), "{sheet}");
        // "Discount and fees" spans the first two grid columns.
        assert!(sheet.contains("<mergeCell ref=\"A5:B5\"/>"), "{sheet}");
        assert!(sheet.contains("<cols><col min=\"1\" max=\"1\""), "{sheet}");
        let styles = part(&x, "xl/styles.xml").unwrap_or_default();
        for code in ["#,##0.00", "0.00_);(0.00)", "0.0%"] {
            assert!(styles.contains(&format!("formatCode=\"{code}\"")), "{code}: {styles}");
        }
        assert!(styles.contains("<cellXfs count=\"5\">"), "{styles}");
        assert_eq!(part(&x, "xl/worksheets/sheet2.xml").as_deref().map(|s| s.contains("<sheetData/>")), Some(true));
        let types = part(&x, "[Content_Types].xml").unwrap_or_default();
        assert!(types.contains("/xl/worksheets/sheet2.xml"), "{types}");
        assert!(part(&x, "docProps/core.xml").unwrap_or_default().contains("<dc:title>Statement</dc:title>"));
    }

    #[test]
    fn empty_documents_and_huge_page_counts_stay_valid_workbooks() {
        let x = xlsx(&[], "Empty");
        assert!(part(&x, "xl/workbook.xml").unwrap_or_default().contains("<sheet name=\"Sheet1\""));
        // Pages past the sheet limit share the last sheet, each after its "Page N" row.
        let one = |t: &str| Page { blocks: vec![block(t, 72.0, 700.0, 100.0, false)], ..Page::default() };
        let pages: Vec<Page> = (0..MAX_SHEETS + 2).map(|i| one(&format!("text {}", i + 1))).collect();
        let x = xlsx(&pages, "Long");
        let workbook = part(&x, "xl/workbook.xml").unwrap_or_default();
        assert_eq!(workbook.matches("<sheet ").count(), MAX_SHEETS);
        assert!(workbook.contains(&format!("<sheet name=\"Pages {MAX_SHEETS}-{}\"", MAX_SHEETS + 2)), "{workbook}");
        let last = part(&x, &format!("xl/worksheets/sheet{MAX_SHEETS}.xml")).unwrap_or_default();
        for t in [format!("text {MAX_SHEETS}"), format!("Page {}", MAX_SHEETS + 1), format!("text {}", MAX_SHEETS + 2)] {
            assert!(last.contains(&format!(">{t}</t>")), "{t}: {last}");
        }
    }

    #[test]
    fn hostile_text_is_escaped_and_bounded() {
        let long = "=CMD()".to_string() + &"x".repeat(MAX_CELL_CHARS + 10) + "\u{0}";
        let p = Page { blocks: vec![block(&long, 72.0, 700.0, 100.0, false)], ..Page::default() };
        let sheet = part(&xlsx(&[p], "T\u{1}"), "xl/worksheets/sheet1.xml").unwrap_or_default();
        let text = sheet.split("<t xml:space=\"preserve\">").nth(1).and_then(|s| s.split("</t>").next()).unwrap_or_default();
        assert_eq!(text.chars().count(), MAX_CELL_CHARS);
        assert!(text.starts_with("=CMD()") && !sheet.contains("<f>"), "inline strings are never formulas");
        assert!(!sheet.contains('\u{0}'));
    }
}

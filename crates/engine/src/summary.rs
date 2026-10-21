//! Summarize Comments layouts that show the page (the viewer's Summarize Comments ▸ Layout):
//!
//! - [`SummaryLayout::ConnectorLines`], "Document and comments with connector lines on single
//!   pages": a landscape page with the commented page on the left and the comments in a column on
//!   the right, each joined to its place on the page by a line from a numbered marker;
//! - [`SummaryLayout::SequenceNumbers`], "Document and comments with sequence numbers on separate
//!   pages": the page at full size with numbered markers, followed by the numbered comments.
//!
//! The page is drawn from a flattened copy of the document (so its comments show), imported as a
//! form XObject. Only pages with comments are included. Text is set in Helvetica (WinAnsi), as in
//! the comments-only layout; characters outside WinAnsi show as `?`. Comments too long for one
//! column continue on the next summary page.

use pdfcraft_cos::{Dict, Document, ObjRef, Object};
use pdfcraft_fonts::{helvetica_width, literal, win_ansi, wrap};
use pdfcraft_render::{Annotation, PageInfo};

use crate::{EditError, SummarySort, comment_kind};

/// How Summarize Comments lays out its PDF.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SummaryLayout {
    /// The comments as text, grouped by page.
    #[default]
    CommentsOnly,
    /// Page and comments side by side on one landscape page, joined by connector lines.
    ConnectorLines,
    /// The page with numbered markers, then its numbered comments on separate pages.
    SequenceNumbers,
}

impl SummaryLayout {
    pub const ALL: [SummaryLayout; 3] = [SummaryLayout::CommentsOnly, SummaryLayout::ConnectorLines, SummaryLayout::SequenceNumbers];
    /// The automation name.
    pub fn name(self) -> &'static str {
        match self {
            SummaryLayout::CommentsOnly => "comments-only",
            SummaryLayout::ConnectorLines => "connector-lines",
            SummaryLayout::SequenceNumbers => "sequence-numbers",
        }
    }
    pub fn parse(s: &str) -> Option<SummaryLayout> {
        Self::ALL.into_iter().find(|k| k.name().eq_ignore_ascii_case(s))
    }
}

/// Top-level comments (no replies, status or checkmark replies) numbered by their position on
/// their page, then ordered by `sort` (stable, so ties keep page order).
pub(crate) fn numbered(all: &[Annotation], sort: SummarySort) -> Vec<(usize, &Annotation)> {
    let mut out: Vec<(usize, &Annotation)> = Vec::new();
    let (mut last, mut n) = (usize::MAX, 0);
    for a in all.iter().filter(|a| a.in_reply_to.is_none() && a.state.is_none()) {
        if a.page != last {
            last = a.page;
            n = 0;
        }
        n += 1;
        out.push((n, a));
    }
    match sort {
        SummarySort::Page => {}
        SummarySort::Author => out.sort_by(|x, y| x.1.author.cmp(&y.1.author)),
        SummarySort::Date => out.sort_by(|x, y| x.1.modified.cmp(&y.1.modified)),
        SummarySort::Type => out.sort_by(|x, y| comment_kind(x.1).cmp(comment_kind(y.1))),
    }
    out
}

/// Replies to `a` (not status replies or checkmarks).
pub(crate) fn replies_of<'a>(all: &'a [Annotation], a: &Annotation) -> Vec<&'a Annotation> {
    if a.name.is_none() {
        return Vec::new();
    }
    all.iter().filter(|r| r.state.is_none() && r.in_reply_to.is_some() && r.in_reply_to == a.name).collect()
}

/// `D:YYYYMMDDHHmm…` as `YYYY-MM-DD HH:MM`; anything else as it is.
fn pretty_date(d: &str) -> String {
    let s = d.strip_prefix("D:").unwrap_or(d);
    let b = s.as_bytes();
    if b.len() >= 12 && b[..12].iter().all(u8::is_ascii_digit) {
        return format!("{}-{}-{} {}:{}", &s[0..4], &s[4..6], &s[6..8], &s[8..10], &s[10..12]);
    }
    if b.len() >= 8 && b[..8].iter().all(u8::is_ascii_digit) {
        return format!("{}-{}-{}", &s[0..4], &s[4..6], &s[6..8]);
    }
    d.to_string()
}

const SIZE: f64 = 8.0;
const LEAD: f64 = 10.0;
const PAD: f64 = 4.0;
const GAP: f64 = 6.0;
const INDENT: f64 = 10.0;
/// Largest number of lines kept for one comment (with its replies), so hostile contents can't
/// make an unbounded summary.
const MAX_LINES: usize = 2_000;

#[derive(Clone)]
struct Line {
    bold: bool,
    indent: f64,
    text: String,
}

struct Entry {
    n: usize,
    color: [f64; 3],
    /// Top-left of the comment on the page as displayed, in points from the bottom-left.
    anchor: [f64; 2],
    lines: Vec<Line>,
}

/// User-space point → displayed page point (y up) on a page drawn `size` points large.
fn to_display(info: &PageInfo, size: (f64, f64), x: f64, y: f64) -> [f64; 2] {
    let [cx0, cy0, cx1, cy1] = info.crop.map(f64::from);
    let span = |d: f64| if d.abs() < 1e-3 { 1.0 } else { d };
    let (u, v) = ((x - cx0) / span(cx1 - cx0), (cy1 - y) / span(cy1 - cy0));
    let (a, b) = match info.rotation {
        90 => (1.0 - v, u),
        180 => (1.0 - u, 1.0 - v),
        270 => (v, 1.0 - u),
        _ => (u, v),
    };
    let (a, b) = (if a.is_finite() { a.clamp(0.0, 1.0) } else { 0.0 }, if b.is_finite() { b.clamp(0.0, 1.0) } else { 0.0 });
    [a * size.0, (1.0 - b) * size.1]
}

fn lines_for(n: usize, a: &Annotation, replies: &[&Annotation], width: f64) -> Vec<Line> {
    let mut out = Vec::new();
    let mut push = |bold: bool, indent: f64, text: &str| {
        for l in wrap(text, SIZE, (width - indent).max(SIZE * 4.0)) {
            out.push(Line { bold, indent, text: l });
        }
    };
    push(true, 0.0, &format!("{n}  {}  {}", comment_kind(a), a.author.as_deref().unwrap_or("")));
    if let Some(d) = a.modified.as_deref() {
        push(false, 0.0, &pretty_date(d));
    }
    if let Some(c) = a.contents.as_deref().filter(|c| !c.trim().is_empty()) {
        push(false, 0.0, c);
    }
    for r in replies {
        push(true, INDENT, &format!("{}  {}", crate::reply_kind(r), r.author.as_deref().unwrap_or("")));
        if let Some(c) = r.contents.as_deref().filter(|c| !c.trim().is_empty()) {
            push(false, INDENT, c);
        }
    }
    if out.len() > MAX_LINES {
        out.truncate(MAX_LINES - 1);
        out.push(Line { bold: false, indent: 0.0, text: "\u{2026}".into() });
    }
    out
}

/// Number formatting for content streams (always finite).
fn f(v: f64) -> String {
    if v.is_finite() { format!("{v:.2}") } else { "0".into() }
}

fn rgb(c: [f64; 3]) -> String {
    format!("{} {} {}", f(c[0]), f(c[1]), f(c[2]))
}

/// A text run at (x, y).
fn text(c: &mut Vec<u8>, bold: bool, size: f64, x: f64, y: f64, s: &str) {
    c.extend(format!("BT /{} {} Tf {} {} Td ", if bold { "F2" } else { "F1" }, f(size), f(x), f(y)).into_bytes());
    c.extend(literal(&win_ansi(s)));
    c.extend_from_slice(b" Tj ET\n");
}

/// A numbered circle marker centred on (x, y).
fn marker(c: &mut Vec<u8>, n: usize, color: [f64; 3], x: f64, y: f64) {
    let r = 6.0;
    let k = 0.5523 * r;
    c.extend(
        format!(
            "q 1 g {col} RG 1 w {x0} {y} m {x0} {yk1} {xk0} {y1} {x} {y1} c {xk1} {y1} {x1} {yk1} {x1} {y} c {x1} {yk0} {xk1} {y0} {x} {y0} c {xk0} {y0} {x0} {yk0} {x0} {y} c b Q\n",
            col = rgb(color),
            x0 = f(x - r),
            x1 = f(x + r),
            y0 = f(y - r),
            y1 = f(y + r),
            xk0 = f(x - k),
            xk1 = f(x + k),
            yk0 = f(y - k),
            yk1 = f(y + k),
            x = f(x),
            y = f(y),
        )
        .into_bytes(),
    );
    let label = n.to_string();
    let size = if label.len() > 2 { 5.0 } else { 7.0 };
    c.extend_from_slice(b"0 g\n");
    text(c, true, size, x - helvetica_width(&label, size) / 2.0, y - size * 0.35, &label);
}

/// A comment box: `lines` set from `top` down in a column at `x` of `width`, outlined in the
/// comment's colour on a light tint of it. Returns its height.
fn draw_box(c: &mut Vec<u8>, x: f64, top: f64, width: f64, lines: &[Line], color: [f64; 3]) -> f64 {
    let h = lines.len() as f64 * LEAD + 2.0 * PAD;
    let tint = color.map(|v| 0.88 + 0.12 * v.clamp(0.0, 1.0));
    c.extend(format!("q {} rg {} RG 0.75 w {} {} {} {} re B Q\n", rgb(tint), rgb(color), f(x), f(top - h), f(width), f(h)).into_bytes());
    c.extend_from_slice(b"0 g\n");
    for (i, l) in lines.iter().enumerate() {
        text(c, l.bold, SIZE, x + PAD + l.indent, top - PAD - SIZE - i as f64 * LEAD + 1.0, &l.text);
    }
    h
}

fn resources(form: Option<ObjRef>, fonts: (ObjRef, ObjRef)) -> Dict {
    let mut f = Dict::new();
    f.set(b"F1".to_vec(), Object::Ref(fonts.0));
    f.set(b"F2".to_vec(), Object::Ref(fonts.1));
    let mut r = Dict::new();
    r.set(b"Font".to_vec(), Object::Dict(f));
    if let Some(form) = form {
        let mut x = Dict::new();
        x.set(b"P0".to_vec(), Object::Ref(form));
        r.set(b"XObject".to_vec(), Object::Dict(x));
    }
    r
}

fn font(doc: &mut Document, base: &str) -> ObjRef {
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("Font"));
    d.set(b"Subtype".to_vec(), Object::name("Type1"));
    d.set(b"BaseFont".to_vec(), Object::name(base));
    d.set(b"Encoding".to_vec(), Object::name("WinAnsiEncoding"));
    doc.add(d)
}

/// A usable displayed page size (degenerate boxes fall back to Letter).
fn sane(size: (f64, f64)) -> (f64, f64) {
    let ok = |v: f64| v.is_finite() && v >= 1.0;
    if ok(size.0) && ok(size.1) { (size.0.min(14_400.0), size.1.min(14_400.0)) } else { (612.0, 792.0) }
}

/// Lay out a comment summary with the pages of `src` (whose comments are `all`; `pages` is the
/// document's page information). `layout` must not be [`SummaryLayout::CommentsOnly`]. Returns
/// `None` when the document has no comments to list.
pub(crate) fn with_pages(
    src: &Document,
    pages: &[PageInfo],
    all: &[Annotation],
    name: &str,
    sort: SummarySort,
    layout: SummaryLayout,
) -> Result<Option<Document>, EditError> {
    let list = numbered(all, sort);
    if list.is_empty() {
        return Ok(None);
    }
    // The pages as displayed, comments drawn in.
    let mut flat = src.clone();
    let indices: Vec<usize> = (0..pages.len()).collect();
    pdfcraft_edit::flatten(&mut flat, &indices, true, false)?;

    let mut out = Document::new_empty();
    let fonts = (font(&mut out, "Helvetica"), font(&mut out, "Helvetica-Bold"));
    let mut commented: Vec<usize> = list.iter().map(|(_, a)| a.page).filter(|p| *p < pages.len()).collect();
    commented.sort_unstable();
    commented.dedup();
    let title = format!("Summary of Comments on {name}");
    for page in commented {
        let info = &pages[page];
        let (form, size) = pdfcraft_organize::page_as_form(&mut out, &flat, page)?;
        let size = sane(size);
        let column = match layout {
            SummaryLayout::ConnectorLines => 324.0,
            _ => 612.0 - 2.0 * 54.0,
        };
        let entries: Vec<Entry> = list
            .iter()
            .filter(|(_, a)| a.page == page)
            .map(|(n, a)| {
                let r = a.rect.map(f64::from);
                let p0 = to_display(info, size, r[0], r[1]);
                let p1 = to_display(info, size, r[2], r[3]);
                Entry {
                    n: *n,
                    color: a.color.map(|c| c.map(f64::from)).unwrap_or([0.45, 0.45, 0.45]),
                    anchor: [p0[0].min(p1[0]), p0[1].max(p1[1])],
                    lines: lines_for(*n, a, &replies_of(all, a), column - 2.0 * PAD),
                }
            })
            .collect();
        let label = if info.label.is_empty() { (page + 1).to_string() } else { info.label.clone() };
        match layout {
            SummaryLayout::ConnectorLines => connector_pages(&mut out, form, size, &entries, &title, &label, fonts)?,
            _ => sequence_pages(&mut out, form, size, &entries, &title, &label, fonts)?,
        }
    }
    pdfcraft_create::set_title(&mut out, &title);
    Ok(Some(out))
}

/// One placed piece of a comment box: entry `entry`, lines `from..to`, top at `top`.
struct Placed {
    entry: usize,
    from: usize,
    to: usize,
    top: f64,
}

/// Fill columns from `top` to `bottom` with the entries' boxes, splitting boxes that don't fit
/// across columns. Returns the boxes of each column.
fn columns(entries: &[Entry], top: f64, bottom: f64) -> Vec<Vec<Placed>> {
    let room = ((top - bottom - 2.0 * PAD) / LEAD).floor().max(1.0) as usize;
    let mut cols: Vec<Vec<Placed>> = vec![Vec::new()];
    let mut y = top;
    for (i, e) in entries.iter().enumerate() {
        let mut from = 0;
        while from < e.lines.len() {
            let left = e.lines.len() - from;
            // A continuation starts with an "N (continued)" line.
            let extra = usize::from(from > 0);
            let fits = (((y - bottom - 2.0 * PAD) / LEAD).floor().max(0.0) as usize).saturating_sub(extra);
            // Start a fresh column when nothing fits, or when the box would be split although a
            // fresh column could hold all of it (or at least a fair part of it).
            let fresh = cols.last().is_none_or(Vec::is_empty);
            if !fresh && (fits == 0 || (fits < left && (left <= room || fits < 3))) {
                cols.push(Vec::new());
                y = top;
                continue;
            }
            let take = left.min(fits.max(1)).min(room.saturating_sub(extra).max(1));
            if let Some(col) = cols.last_mut() {
                col.push(Placed { entry: i, from, to: from + take, top: y });
            }
            y -= (take + extra) as f64 * LEAD + 2.0 * PAD + GAP;
            from += take;
        }
    }
    cols
}

/// Lines `from..to` of an entry, with "(continued)" on a continuation.
fn piece(e: &Entry, from: usize, to: usize) -> Vec<Line> {
    let mut v = Vec::with_capacity(to - from + 1);
    if from > 0 {
        v.push(Line { bold: true, indent: 0.0, text: format!("{} (continued)", e.n) });
    }
    v.extend(e.lines.get(from..to).unwrap_or_default().iter().cloned());
    v
}

#[allow(clippy::too_many_arguments)]
fn connector_pages(
    out: &mut Document,
    form: ObjRef,
    size: (f64, f64),
    entries: &[Entry],
    title: &str,
    label: &str,
    fonts: (ObjRef, ObjRef),
) -> Result<(), EditError> {
    let (w, h) = (792.0, 612.0);
    let (left, area_w, area_h, bottom, top) = (36.0, 360.0, 612.0 - 36.0 - 48.0, 36.0, 612.0 - 48.0);
    let s = (area_w / size.0).min(area_h / size.1);
    let (tx, ty) = (left + (area_w - size.0 * s) / 2.0, top - size.1 * s);
    let (cx, cw) = (left + area_w + 36.0, w - 36.0 - (left + area_w + 36.0));
    for col in columns(entries, top, bottom) {
        let mut c = Vec::new();
        text(&mut c, true, 10.0, left, h - 30.0, title);
        let page_label = format!("Page {label}");
        text(&mut c, false, 10.0, w - 36.0 - helvetica_width(&page_label, 10.0), h - 30.0, &page_label);
        c.extend(format!("q {} 0 0 {} {} {} cm /P0 Do Q\n", f(s), f(s), f(tx), f(ty)).into_bytes());
        c.extend(format!("q 0.6 G 0.5 w {} {} {} {} re S Q\n", f(tx), f(ty), f(size.0 * s), f(size.1 * s)).into_bytes());
        // Connector lines under the markers and boxes.
        let mut boxes = Vec::new();
        for p in &col {
            let Some(e) = entries.get(p.entry) else { continue };
            let lines = piece(e, p.from, p.to);
            let (ax, ay) = (tx + e.anchor[0] * s, ty + e.anchor[1] * s);
            if p.from == 0 {
                c.extend(format!("q {} RG 0.6 w {} {} m {} {} l S Q\n", rgb(e.color), f(ax), f(ay), f(cx), f(p.top - PAD - SIZE / 2.0)).into_bytes());
            }
            boxes.push((e, lines, p.top, p.from == 0, (ax, ay)));
        }
        for (e, _, _, first, (ax, ay)) in &boxes {
            if *first {
                marker(&mut c, e.n, e.color, *ax, *ay);
            }
        }
        for (e, lines, top, ..) in &boxes {
            draw_box(&mut c, cx, *top, cw, lines, e.color);
        }
        pdfcraft_create::add_page(out, w, h, resources(Some(form), fonts), Some(c))?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn sequence_pages(
    out: &mut Document,
    form: ObjRef,
    size: (f64, f64),
    entries: &[Entry],
    title: &str,
    label: &str,
    fonts: (ObjRef, ObjRef),
) -> Result<(), EditError> {
    // The page itself, with a numbered marker at each comment.
    let mut c = b"q /P0 Do Q\n".to_vec();
    for e in entries {
        let x = (e.anchor[0]).clamp(6.0, (size.0 - 6.0).max(6.0));
        let y = (e.anchor[1]).clamp(6.0, (size.1 - 6.0).max(6.0));
        marker(&mut c, e.n, e.color, x, y);
    }
    pdfcraft_create::add_page(out, size.0, size.1, resources(Some(form), fonts), Some(c))?;
    // Then the comments, on Letter pages.
    let (w, h, m) = (612.0, 792.0, 54.0);
    for col in columns(entries, h - m - 24.0, m) {
        let mut c = Vec::new();
        text(&mut c, true, 10.0, m, h - m, &format!("{title}  Page {label}"));
        for p in &col {
            let Some(e) = entries.get(p.entry) else { continue };
            draw_box(&mut c, m, p.top, w - 2.0 * m, &piece(e, p.from, p.to), e.color);
            if p.from == 0 {
                marker(&mut c, e.n, e.color, m - 12.0, p.top - PAD - SIZE / 2.0);
            }
        }
        pdfcraft_create::add_page(out, w, h, resources(None, fonts), Some(c))?;
    }
    Ok(())
}

//! Add Printer Marks (PDF: Print Production ▸ Add Printer Marks): trim (crop) marks, bleed
//! marks, registration targets, colour bars and a page information line drawn outside the trim
//! box, with the media box enlarged to hold them.
//!
//! **Geometry.** The trim is the page's `/TrimBox`, else its crop box. Marks start `offset`
//! points outside the trim (pushed further out past any bleed, so they never print into the
//! bleed) and run [`MARK_LENGTH`] points; the media box grows to the trim plus
//! [`PrinterMarks::margin`] on every side (never shrinking). The crop box becomes the new media
//! box so the marks show, and the trim, bleed and art boxes are pinned to their old effective
//! rectangles so the finished size doesn't move. Everything is laid out in the page's display
//! frame (after `/Rotate`, in points whatever the `/UserUnit`): colour bars along the top and the
//! page information along the bottom as the page is shown.
//!
//! **Storage.** One content stream appended to the page's `/Contents` (tagged `/PCMark
//! /PrinterMarks`, after the original content is wrapped in q/Q; see the crate docs), drawn as
//! `q /OC /PCMarksOC BDC /Artifact <</Type /Page …>> BDC … EMC EMC Q`: page artifacts, which
//! ISO 32000-2 §14.8.2.2.2 names for "cut marks and colour bars", inside an optional content
//! group named "Printer marks" so viewers can hide them. Lines, targets and the information line
//! use the registration colour (`/Separation /All`, prints on every plate); colour bars use
//! DeviceCMYK. The information line is drawn as glyph outlines (Inter), not text, so text
//! extraction, search and accessible text never pick it up. Adding marks again replaces them.

use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString};

use crate::{EditError, check, contents, n, page_list, place_tagged, stream_tag};

/// How long trim, bleed and centre marks run, in points.
pub const MARK_LENGTH: f64 = 18.0;
/// Largest offset from the trim, in points (1 inch).
pub const MAX_OFFSET: f64 = 72.0;
/// Heaviest mark line, in points.
pub const MAX_LINE_WEIGHT: f64 = 4.0;
/// Longest file name the page information shows, in characters.
pub const MAX_FILE_NAME_CHARS: usize = 256;
/// The optional content group's name.
pub const LAYER_NAME: &str = "Printer marks";
/// The `/PCMark` tag of the marks stream.
pub const TAG: &str = "PrinterMarks";
/// Largest page side the enlarged media box may have, in user units (ISO 32000-2 Annex C).
const MAX_PAGE_SIDE: f64 = 14_400.0;
/// Eastern-style bleed lines when the page has no bleed: 3 mm, in points.
const EASTERN_BLEED: f64 = 3.0 * 72.0 / 25.4;
/// Largest information line size, in points.
const INFO_SIZE: f64 = 6.0;
/// Most pages one call marks.
pub const MAX_PAGES: usize = 10_000;

/// The viewer's mark styles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MarkStyle {
    /// Single crop lines at the corners, bleed lines beside them, round registration targets.
    #[default]
    Western,
    /// Japanese "tombo": double corner lines (trim and bleed, 3 mm bleed when the page has
    /// none) and centre crosses on each side in place of round targets.
    Eastern,
}

impl MarkStyle {
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "western" => Some(Self::Western),
            "eastern" | "tombo" | "japanese" => Some(Self::Eastern),
            _ => None,
        }
    }
}

/// Which printer marks to draw, and how.
#[derive(Clone, Debug, PartialEq)]
pub struct PrinterMarks {
    pub trim: bool,
    pub bleed: bool,
    pub registration: bool,
    pub color_bars: bool,
    pub page_info: bool,
    /// Mark line weight in points.
    pub line_weight: f64,
    /// Distance of the marks from the trim, in points.
    pub offset: f64,
    pub style: MarkStyle,
    /// The file name the page information shows (empty: page number and date only).
    pub file_name: String,
}

impl Default for PrinterMarks {
    /// All marks, 0.25 pt lines, 6 pt from the trim, Western.
    fn default() -> Self {
        Self {
            trim: true,
            bleed: true,
            registration: true,
            color_bars: true,
            page_info: true,
            line_weight: 0.25,
            offset: 6.0,
            style: MarkStyle::Western,
            file_name: String::new(),
        }
    }
}

impl PrinterMarks {
    /// Refuse settings that can't be drawn, saying what is allowed.
    pub fn validate(&self) -> Result<(), EditError> {
        let bad = |m: &str| Err(EditError::Invalid(m.into()));
        if !(self.trim || self.bleed || self.registration || self.color_bars || self.page_info) {
            return bad("choose at least one mark (trim, bleed, registration, color_bars, page_info)");
        }
        if !(self.line_weight.is_finite() && self.line_weight > 0.0 && self.line_weight <= MAX_LINE_WEIGHT) {
            return bad("the line weight must be more than 0 and at most 4 points");
        }
        if !(self.offset.is_finite() && (0.0..=MAX_OFFSET).contains(&self.offset)) {
            return bad("the offset must be between 0 and 72 points");
        }
        if self.file_name.chars().take(MAX_FILE_NAME_CHARS + 1).count() > MAX_FILE_NAME_CHARS {
            return bad("the file name is longer than 256 characters");
        }
        if self.file_name.chars().any(char::is_control) {
            return bad("the file name can't contain control characters");
        }
        Ok(())
    }

    /// Where the marks start, in points from the trim, for a page whose bleed reaches `bleed`
    /// points past the trim (its widest side).
    pub fn start(&self, bleed: f64) -> f64 {
        self.offset.max(self.bleed_extent(bleed))
    }

    /// The space added around the trim, in points: the start, the marks, and the line weight.
    pub fn margin(&self, bleed: f64) -> f64 {
        self.start(bleed) + MARK_LENGTH + self.line_weight
    }

    fn bleed_extent(&self, bleed: f64) -> f64 {
        if self.style == MarkStyle::Eastern && self.bleed && bleed <= 0.0 { EASTERN_BLEED } else { bleed.max(0.0) }
    }
}

/// What marking a page did, in user space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkedPage {
    /// 0-based.
    pub page: usize,
    pub trim: [f64; 4],
    pub bleed: [f64; 4],
    /// The enlarged media (and crop) box.
    pub media: [f64; 4],
    /// The margin added around the trim, in points.
    pub margin: f64,
}

fn rect(doc: &Document, o: Option<&Object>) -> Option<[f64; 4]> {
    let o = doc.resolve(o?);
    let v: Vec<f64> = o.as_array()?.iter().filter_map(|x| doc.resolve(x).as_f64()).collect();
    match v.as_slice() {
        [a, b, c, d] if v.iter().all(|x| x.is_finite()) => Some([a.min(*c), b.min(*d), a.max(*c), b.max(*d)]),
        _ => None,
    }
}

fn intersect(a: [f64; 4], b: [f64; 4]) -> Option<[f64; 4]> {
    let r = [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])];
    (r[2] > r[0] && r[3] > r[1]).then_some(r)
}

fn rect_obj(r: [f64; 4]) -> Object {
    Object::Array(r.iter().map(|v| Object::Real(*v)).collect())
}

/// The page's effective media, crop, bleed, trim and art boxes.
fn boxes(doc: &Document, page: &pdfcraft_model::Page) -> [[f64; 4]; 5] {
    let d = &page.dict;
    let media = rect(doc, d.get(b"MediaBox")).filter(|m| m[2] > m[0] && m[3] > m[1]).unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let crop = rect(doc, d.get(b"CropBox")).and_then(|c| intersect(c, media)).unwrap_or(media);
    let child = |k: &[u8]| rect(doc, d.get(k)).and_then(|c| intersect(c, media)).unwrap_or(crop);
    [media, crop, child(b"BleedBox"), child(b"TrimBox"), child(b"ArtBox")]
}

/// Add printer marks to `pages` (0-based), replacing marks added before. `date` is today
/// (year, month, day) for the page information. All or nothing: every page is checked first.
pub fn add_printer_marks(doc: &mut Document, pages: &[usize], marks: &PrinterMarks, date: (i64, u32, u32)) -> Result<Vec<MarkedPage>, EditError> {
    marks.validate()?;
    if pages.is_empty() {
        return Err(EditError::Invalid("choose at least one page".into()));
    }
    if pages.len() > MAX_PAGES {
        return Err(EditError::Invalid(format!("mark at most {MAX_PAGES} pages at a time")));
    }
    let all = page_list(doc);
    check(pages, all.len())?;
    let mut unique: Vec<usize> = pages.to_vec();
    unique.sort_unstable();
    unique.dedup();
    // Plan every page first, so a page that can't grow changes nothing.
    let mut plan = Vec::with_capacity(unique.len());
    for &i in &unique {
        let page = all.get(i).ok_or(EditError::NoSuchPage(i))?;
        let unit = page
            .dict
            .get(b"UserUnit")
            .map(|u| doc.resolve(u))
            .and_then(|u| u.as_f64())
            .filter(|u| u.is_finite() && *u > 0.0 && *u <= 75_000.0)
            .unwrap_or(1.0);
        let [media, _crop, bleed, trim, art] = boxes(doc, page);
        let user = [trim[0] - bleed[0], trim[1] - bleed[1], bleed[2] - trim[2], bleed[3] - trim[3]].map(|e| e.max(0.0) * unit);
        let widest = user.iter().cloned().fold(0.0, f64::max);
        let margin = marks.margin(widest);
        let m = margin / unit;
        let grown = [(trim[0] - m).min(media[0]), (trim[1] - m).min(media[1]), (trim[2] + m).max(media[2]), (trim[3] + m).max(media[3])];
        if !grown.iter().all(|v| v.is_finite()) || grown[2] - grown[0] > MAX_PAGE_SIDE || grown[3] - grown[1] > MAX_PAGE_SIDE {
            return Err(EditError::Invalid(format!("page {} would be larger than the 14400-unit page limit with printer marks", i + 1)));
        }
        let rotation = page.rotation(doc);
        // Bleed per side as displayed: left, bottom, right, top.
        let turn = usize::try_from(rotation / 90).unwrap_or(0);
        let shown: [f64; 4] = std::array::from_fn(|k| user.get((k + turn) % 4).copied().unwrap_or(0.0));
        let (w, h) = if rotation % 180 == 0 { (trim[2] - trim[0], trim[3] - trim[1]) } else { (trim[3] - trim[1], trim[2] - trim[0]) };
        let frame = Frame { w: w * unit, h: h * unit, bleed: shown, start: marks.start(widest) };
        let info = format!(
            "{}{}Page {}  {}-{:02}-{:02}",
            marks.file_name,
            if marks.file_name.is_empty() { "" } else { "  " },
            i + 1,
            date.0,
            date.1,
            date.2
        );
        let mut content = format!("q\n{} cm\n", matrix(pdfcraft_model::view_matrix_for(rotation, trim)));
        if (unit - 1.0).abs() > 1e-12 {
            content.push_str(&format!("{} 0 0 {} 0 0 cm\n", n(1.0 / unit), n(1.0 / unit)));
        }
        content.push_str("/OC /PCMarksOC BDC\n/Artifact <</Type /Page /PCMark /PrinterMarks>> BDC\n");
        content.push_str(&draw(&frame, marks, &info));
        content.push_str("EMC\nEMC\nQ\n");
        plan.push((page.obj, trim, bleed, art, grown, margin, content, i));
    }
    let layer = layer(doc)?;
    let mut done = Vec::with_capacity(plan.len());
    for (obj, trim, bleed, art, grown, margin, content, i) in plan {
        // Marks added before are replaced.
        let p = page_list(doc).into_iter().nth(i).ok_or(EditError::NoSuchPage(i))?;
        let list = contents(doc, &p)?;
        let kept: Vec<Object> = list.iter().filter(|o| stream_tag(doc, o).as_deref() != Some(TAG)).cloned().collect();
        if kept.len() != list.len() {
            doc.update_dict(obj, |d| d.set(b"Contents".to_vec(), Object::Array(kept)))?;
        }
        add_resources(doc, obj, layer)?;
        doc.update_dict(obj, |d| {
            d.set(b"MediaBox".to_vec(), rect_obj(grown));
            d.set(b"CropBox".to_vec(), rect_obj(grown));
            d.set(b"TrimBox".to_vec(), rect_obj(trim));
            d.set(b"BleedBox".to_vec(), rect_obj(bleed));
            d.set(b"ArtBox".to_vec(), rect_obj(art));
        })?;
        let p = page_list(doc).into_iter().nth(i).ok_or(EditError::NoSuchPage(i))?;
        place_tagged(doc, &p, TAG, content.into_bytes(), false)?;
        done.push(MarkedPage { page: i, trim, bleed, media: grown, margin });
    }
    Ok(done)
}

fn matrix(m: [f64; 6]) -> String {
    m.map(n).join(" ")
}

/// The registration colour space and the layer in the page's own resources.
fn add_resources(doc: &mut Document, page: ObjRef, layer: ObjRef) -> Result<(), EditError> {
    let p = doc.get(page).as_dict().cloned().unwrap_or_default();
    // Inherited resources are copied onto the page (shared dictionaries stay untouched).
    let inherited = pdfcraft_model::pages(doc).into_iter().find(|x| x.obj == page).and_then(|x| x.dict.get(b"Resources").cloned());
    let mut res = p.get(b"Resources").cloned().or(inherited).map(|r| doc.resolve(&r)).and_then(|r| r.as_dict().cloned()).unwrap_or_default();
    let sub = |res: &Dict, k: &[u8]| res.get(k).map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned()).unwrap_or_default();
    let mut spaces = sub(&res, b"ColorSpace");
    let mut tint = Dict::new();
    tint.set(b"FunctionType".to_vec(), Object::Int(2));
    tint.set(b"Domain".to_vec(), Object::Array(vec![Object::Int(0), Object::Int(1)]));
    tint.set(b"C0".to_vec(), Object::Array(vec![Object::Int(0); 4]));
    tint.set(b"C1".to_vec(), Object::Array(vec![Object::Int(1); 4]));
    tint.set(b"N".to_vec(), Object::Int(1));
    spaces
        .set(b"PCAll".to_vec(), Object::Array(vec![Object::name("Separation"), Object::name("All"), Object::name("DeviceCMYK"), Object::Dict(tint)]));
    res.set(b"ColorSpace".to_vec(), Object::Dict(spaces));
    let mut props = sub(&res, b"Properties");
    props.set(b"PCMarksOC".to_vec(), Object::Ref(layer));
    res.set(b"Properties".to_vec(), Object::Dict(props));
    doc.update_dict(page, |d| d.set(b"Resources".to_vec(), Object::Dict(res)))?;
    Ok(())
}

/// The "Printer marks" optional content group: the one added before, or a new one registered in
/// the catalog's `/OCProperties` (on by default, for viewing and printing).
fn layer(doc: &mut Document) -> Result<ObjRef, EditError> {
    let root = doc.root().ok_or_else(|| EditError::Invalid("the document has no catalog".into()))?;
    let catalog = doc.get(root).as_dict().cloned().ok_or_else(|| EditError::Invalid("the document has no catalog".into()))?;
    let (props_ref, mut props) = match catalog.get(b"OCProperties") {
        Some(Object::Ref(r)) => (Some(*r), doc.get(*r).as_dict().cloned().unwrap_or_default()),
        Some(Object::Dict(d)) => (None, d.clone()),
        _ => (None, Dict::new()),
    };
    let list = |d: &Dict, k: &[u8], doc: &Document| d.get(k).map(|v| doc.resolve(v)).and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let mut ocgs = list(&props, b"OCGs", doc);
    let ours = ocgs.iter().take(100_000).filter_map(Object::as_ref).find(|r| {
        let o = doc.get(*r);
        o.as_dict().is_some_and(|d| d.name(b"PCMark") == Some(TAG.as_bytes()) && d.name(b"Type") == Some(b"OCG".as_slice()))
    });
    if let Some(r) = ours {
        return Ok(r);
    }
    let mut ocg = Dict::new();
    ocg.set(b"Type".to_vec(), Object::name("OCG"));
    ocg.set(b"Name".to_vec(), Object::String(PdfString::text(LAYER_NAME)));
    ocg.set(b"PCMark".to_vec(), Object::name(TAG));
    let state = |k: &str, v: &str| {
        let mut d = Dict::new();
        d.set(k.as_bytes().to_vec(), Object::name(v));
        Object::Dict(d)
    };
    let mut usage = Dict::new();
    usage.set(b"View".to_vec(), state("ViewState", "ON"));
    usage.set(b"Print".to_vec(), state("PrintState", "ON"));
    ocg.set(b"Usage".to_vec(), Object::Dict(usage));
    let r = doc.add(Object::Dict(ocg));
    ocgs.push(Object::Ref(r));
    let mut config = props.get(b"D").map(|d| doc.resolve(d)).and_then(|d| d.as_dict().cloned()).unwrap_or_default();
    let mut order = list(&config, b"Order", doc);
    order.push(Object::Ref(r));
    config.set(b"Order".to_vec(), Object::Array(order));
    // A default configuration that hides everything not listed as on must list the layer.
    if config.name(b"BaseState") == Some(b"OFF".as_slice()) {
        let mut on = list(&config, b"ON", doc);
        on.push(Object::Ref(r));
        config.set(b"ON".to_vec(), Object::Array(on));
    }
    props.set(b"OCGs".to_vec(), Object::Array(ocgs));
    props.set(b"D".to_vec(), Object::Dict(config));
    match props_ref {
        Some(p) => doc.set(p, Object::Dict(props)),
        None => doc.update_dict(root, |c| c.set(b"OCProperties".to_vec(), Object::Dict(props)))?,
    }
    Ok(r)
}

/// The trim in display space: `[0, 0, w, h]` points, with the bleed per displayed side (left,
/// bottom, right, top) and where the marks start.
struct Frame {
    w: f64,
    h: f64,
    bleed: [f64; 4],
    start: f64,
}

fn line(out: &mut String, a: [f64; 2], b: [f64; 2]) {
    out.push_str(&format!("{} {} m {} {} l\n", n(a[0]), n(a[1]), n(b[0]), n(b[1])));
}

/// A circle as four Bézier arcs.
fn circle(out: &mut String, c: [f64; 2], r: f64) {
    let k = r * 0.552_284_75;
    let [x, y] = c;
    out.push_str(&format!(
        "{} {} m {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c h\n",
        n(x + r),
        n(y),
        n(x + r),
        n(y + k),
        n(x + k),
        n(y + r),
        n(x),
        n(y + r),
        n(x - k),
        n(y + r),
        n(x - r),
        n(y + k),
        n(x - r),
        n(y),
        n(x - r),
        n(y - k),
        n(x - k),
        n(y - r),
        n(x),
        n(y - r),
        n(x + k),
        n(y - r),
        n(x + r),
        n(y - k),
        n(x + r),
        n(y)
    ));
}

/// Radius of a registration target.
const TARGET: f64 = MARK_LENGTH * 0.3;

fn draw(f: &Frame, m: &PrinterMarks, info: &str) -> String {
    let mut out = format!("/PCAll CS /PCAll cs 1 SCN 1 scn {} w 0 J 0 j [] 0 d\n", n(m.line_weight));
    let (s, len) = (f.start, MARK_LENGTH);
    let corners = [(0.0, 0.0, -1.0, -1.0), (f.w, 0.0, 1.0, -1.0), (0.0, f.h, -1.0, 1.0), (f.w, f.h, 1.0, 1.0)];
    // Bleed past the trim on the side a corner's lines run along: x side, y side.
    let side = |sx: f64, sy: f64| -> (f64, f64) {
        let bx = if sx < 0.0 { f.bleed[0] } else { f.bleed[2] };
        let by = if sy < 0.0 { f.bleed[1] } else { f.bleed[3] };
        (bx, by)
    };
    let mut lines = String::new();
    for &(cx, cy, sx, sy) in &corners {
        if m.trim {
            line(&mut lines, [cx + sx * s, cy], [cx + sx * (s + len), cy]);
            line(&mut lines, [cx, cy + sy * s], [cx, cy + sy * (s + len)]);
        }
        if m.bleed {
            let (bx, by) = side(sx, sy);
            match m.style {
                MarkStyle::Western => {
                    // Short lines on the bleed edges, beside the trim marks.
                    if by > 0.0 {
                        line(&mut lines, [cx + sx * s, cy + sy * by], [cx + sx * (s + len / 2.0), cy + sy * by]);
                    }
                    if bx > 0.0 {
                        line(&mut lines, [cx + sx * bx, cy + sy * s], [cx + sx * bx, cy + sy * (s + len / 2.0)]);
                    }
                }
                MarkStyle::Eastern => {
                    // The outer line of the double tombo, full length.
                    let bx = if bx > 0.0 { bx } else { EASTERN_BLEED };
                    let by = if by > 0.0 { by } else { EASTERN_BLEED };
                    line(&mut lines, [cx + sx * s, cy + sy * by], [cx + sx * (s + len), cy + sy * by]);
                    line(&mut lines, [cx + sx * bx, cy + sy * s], [cx + sx * bx, cy + sy * (s + len)]);
                }
            }
        }
    }
    if !lines.is_empty() {
        out.push_str(&lines);
        out.push_str("S\n");
    }
    let mid = s + len / 2.0;
    let centres = [[f.w / 2.0, -mid], [f.w / 2.0, f.h + mid], [-mid, f.h / 2.0], [f.w + mid, f.h / 2.0]];
    if m.registration {
        match m.style {
            MarkStyle::Western => {
                for c in centres {
                    target(&mut out, c);
                }
            }
            MarkStyle::Eastern => {
                // Centre marks: a line out from the middle of each side, crossed halfway.
                let mut l = String::new();
                for (k, c) in centres.iter().enumerate() {
                    let horizontal = k >= 2;
                    let (a, b, p, q) = if horizontal {
                        let dir = if k == 2 { -1.0 } else { 1.0 };
                        ([c[0] - dir * len / 2.0, c[1]], [c[0] + dir * len / 2.0, c[1]], [c[0], c[1] - len / 2.0], [c[0], c[1] + len / 2.0])
                    } else {
                        let dir = if k == 0 { -1.0 } else { 1.0 };
                        ([c[0], c[1] - dir * len / 2.0], [c[0], c[1] + dir * len / 2.0], [c[0] - len / 2.0, c[1]], [c[0] + len / 2.0, c[1]])
                    };
                    line(&mut l, a, b);
                    line(&mut l, p, q);
                }
                out.push_str(&l);
                out.push_str("S\n");
            }
        }
    }
    // Space the bars and the information line keep clear of the corner and centre marks.
    let gap = 3.0;
    let centre_clear = if m.registration { TARGET * 1.5 + gap } else { 0.0 };
    if m.color_bars {
        bars(&mut out, f, centre_clear, gap);
    }
    if m.page_info {
        info_line(&mut out, f, info, centre_clear, gap);
    }
    out
}

/// A registration target: a ring with a cross through it and a solid centre with a knocked-out
/// cross.
fn target(out: &mut String, c: [f64; 2]) {
    let r = TARGET;
    let mut l = String::new();
    circle(&mut l, c, r);
    line(&mut l, [c[0] - r * 1.5, c[1]], [c[0] + r * 1.5, c[1]]);
    line(&mut l, [c[0], c[1] - r * 1.5], [c[0], c[1] + r * 1.5]);
    out.push_str(&l);
    out.push_str("S\n");
    let mut dot = String::new();
    circle(&mut dot, c, r / 2.0);
    out.push_str(&dot);
    out.push_str("f\nq 0 SCN\n");
    let mut x = String::new();
    line(&mut x, [c[0] - r / 2.0, c[1]], [c[0] + r / 2.0, c[1]]);
    line(&mut x, [c[0], c[1] - r / 2.0], [c[0], c[1] + r / 2.0]);
    out.push_str(&x);
    out.push_str("S Q\n");
}

/// Process colour patches (C, M, Y, K and their overprints) left of the top centre, and a black
/// tint bar (10–100%) right of it, in the band above the trim. Groups that don't fit at two
/// points a patch are left out.
fn bars(out: &mut String, f: &Frame, centre_clear: f64, gap: f64) {
    const SOLIDS: [[f64; 4]; 8] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
        [1.0, 1.0, 0.0, 0.0],
        [1.0, 0.0, 1.0, 0.0],
        [0.0, 1.0, 1.0, 0.0],
        [1.0, 1.0, 1.0, 0.0],
    ];
    let tints: Vec<[f64; 4]> = (1..=10).map(|t| [0.0, 0.0, 0.0, f64::from(t) / 10.0]).collect();
    let size = MARK_LENGTH * 0.6;
    let y = f.h + f.start + (MARK_LENGTH - size) / 2.0;
    let mut groups = vec![(gap, f.w / 2.0 - centre_clear, SOLIDS.to_vec()), (f.w / 2.0 + centre_clear, f.w - gap, tints)];
    if centre_clear == 0.0 {
        groups = vec![(gap, f.w - gap, SOLIDS.iter().copied().chain((1..=10).map(|t| [0.0, 0.0, 0.0, f64::from(t) / 10.0])).collect())];
    }
    for (x0, x1, colours) in groups {
        let count = colours.len() as f64;
        let s = size.min((x1 - x0) / count);
        if !s.is_finite() || s < 2.0 {
            continue;
        }
        for (k, c) in colours.iter().enumerate() {
            out.push_str(&format!("{} {} {} {} k {} {} {} {} re f\n", n(c[0]), n(c[1]), n(c[2]), n(c[3]), n(x0 + k as f64 * s), n(y), n(s), n(size)));
        }
        // A thin registration-colour frame round the group.
        out.push_str(&format!("{} {} {} {} re S\n", n(x0), n(y), n(s * count), n(size)));
    }
}

/// The page information under the trim, as outlines; shrunk to fit (left out below 2 pt).
fn info_line(out: &mut String, f: &Frame, info: &str, centre_clear: f64, gap: f64) {
    let outline = pdfcraft_fonts::text_outline(info).or_else(|| {
        // Characters no bundled face has become "?" rather than losing the whole line.
        let plain: String = info.chars().map(|c| if c.is_ascii() { c } else { '?' }).collect();
        pdfcraft_fonts::text_outline(&plain)
    });
    let Some(o) = outline else { return };
    let avail = if centre_clear > 0.0 { f.w / 2.0 - centre_clear - gap } else { f.w - 2.0 * gap };
    if !(o.width > 0.0 && avail > 0.0) {
        return;
    }
    let size = INFO_SIZE.min(avail / o.width);
    if !size.is_finite() || size < 2.0 {
        return;
    }
    let (x0, y0) = (gap, -(f.start + MARK_LENGTH / 2.0) - size * 0.35);
    let mut path = String::new();
    for contour in &o.contours {
        for (k, p) in contour.iter().enumerate() {
            path.push_str(&format!("{} {} {} ", n(x0 + p[0] * size), n(y0 + p[1] * size), if k == 0 { "m" } else { "l" }));
        }
        if !contour.is_empty() {
            path.push_str("h\n");
        }
    }
    if !path.is_empty() {
        out.push_str(&path);
        out.push_str("f\n");
    }
}

#[cfg(test)]
mod tests;

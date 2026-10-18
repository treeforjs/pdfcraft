//! Export a PDF ▸ PowerPoint (.pptx, PresentationML, ECMA-376 Part 1 §13 and §19).
//!
//! One slide per page. The engine hands over each page as a [`Slide`]: its displayed size, the
//! paragraphs as positioned text boxes ([`SlideText`]), the images as pictures
//! ([`SlidePicture`]) and, optionally, a rendered background picture of everything else on the
//! page (vector art, shadings, inline images, annotation appearances). Positions are in points
//! on the displayed page (origin top left, y down), so rotated pages need no special case here.
//!
//! The presentation has one slide size: the first slide's, within PowerPoint's 1 to 56 inch
//! range. A page of another size (or one outside that range) is scaled uniformly to fit its
//! slide and centred, text sizes included.
//!
//! Z-order on a slide is background, then pictures, then text boxes, so text stays on top and
//! editable. The package is the smallest one PowerPoint and LibreOffice open without repair:
//! content types, package and part relationships, `presentation.xml`, one slide master with a
//! blank layout, a theme, presentation/view properties, a table style list, the slides and
//! their media (identical images are stored once).
//!
//! Bounds: at most [`MAX_SLIDES`] slides, [`MAX_SHAPES`] text boxes and pictures per slide,
//! [`MAX_MEDIA`] media files and [`MAX_MEDIA_BYTES`] of media in all (the zip writer has no
//! zip64); what doesn't fit is left out and counted in [`PptxStats::omitted`].

use super::{Zip, esc, rgb8};

/// More slides than this are left out (each slide is two zip entries; the writer has no zip64).
pub const MAX_SLIDES: usize = 4000;
/// Text boxes plus pictures on one slide; the rest of a page's content is left out.
pub const MAX_SHAPES: usize = 10_000;
/// Media files in the package (zip entries are counted in 16 bits).
pub const MAX_MEDIA: usize = 40_000;
/// Media bytes in the package (zip offsets are 32 bits).
pub const MAX_MEDIA_BYTES: usize = 2 << 30;
/// Characters in one text box.
const MAX_TEXT_CHARS: usize = 100_000;
/// EMU per point (ECMA-376 §20.1.2.1: 914 400 per inch).
const EMU: f64 = 12_700.0;
/// PowerPoint's slide size range, in points (1 to 56 inches).
const MIN_SLIDE: f64 = 72.0;
const MAX_SLIDE: f64 = 4032.0;

/// Where a shape sits: the unrotated box (top-left corner and size, points, slide space y
/// down), turned clockwise by `rot` degrees about its centre and mirrored top to bottom when
/// `flip_v` (DrawingML `a:xfrm`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Placement {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub rot: f64,
    pub flip_v: bool,
}

/// A paragraph as an editable text box.
#[derive(Clone, Debug, PartialEq)]
pub struct SlideText {
    pub text: String,
    /// The font family (see [`font_family`]); empty leaves the theme's.
    pub font: String,
    /// Points.
    pub size: f64,
    pub bold: bool,
    pub italic: bool,
    /// sRGB components in 0..=1.
    pub color: [f64; 3],
    /// Visual lines the paragraph had on the page: more than one sets the line pitch.
    pub lines: usize,
    pub at: Placement,
}

/// An image as a picture shape.
#[derive(Clone, Debug, PartialEq)]
pub struct SlidePicture {
    /// PNG or JPEG data (told apart by its signature; anything else is left out).
    pub bytes: Vec<u8>,
    pub at: Placement,
}

/// One page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Slide {
    /// The displayed page size in points.
    pub width: f64,
    pub height: f64,
    /// A PNG covering the whole page, drawn first.
    pub background: Option<Vec<u8>>,
    pub pictures: Vec<SlidePicture>,
    pub texts: Vec<SlideText>,
}

/// What went into the package.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PptxStats {
    pub slides: usize,
    pub text_boxes: usize,
    pub pictures: usize,
    pub backgrounds: usize,
    /// Slides, shapes or media left out (limits, or image data that is not PNG/JPEG).
    pub omitted: usize,
}

/// A PDF font name as a family name PowerPoint can match: the subset tag (`ABCDEF+`) and style
/// suffixes (`-Bold`, `,Italic`, `MT`, `PSMT`) are dropped, the standard 14 names become the
/// fonts Office ships (Helvetica ▸ Arial, Times ▸ Times New Roman, Courier ▸ Courier New), and
/// a run-together name is split into words (`SourceSansPro` ▸ `Source Sans Pro`).
pub fn font_family(base_font: &str) -> String {
    let mut s = base_font.trim();
    if let Some((tag, rest)) = s.split_once('+')
        && tag.len() == 6
        && tag.bytes().all(|b| b.is_ascii_uppercase())
    {
        s = rest;
    }
    let s = s.split([',', '-']).next().unwrap_or("").trim();
    let mut s = s.to_string();
    for suffix in ["PSMT", "MT", "PS"] {
        if s.len() > suffix.len() + 1 && s.ends_with(suffix) {
            s.truncate(s.len() - suffix.len());
            break;
        }
    }
    // Split CamelCase into words: a capital after a lower-case letter or digit starts one.
    let mut words: Vec<String> = Vec::new();
    let mut prev: Option<char> = None;
    for c in s.chars().filter(|c| !c.is_control()) {
        if c == ' ' || c == '_' {
            prev = None;
            words.push(String::new());
            continue;
        }
        if words.is_empty() || (c.is_uppercase() && prev.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit())) {
            words.push(String::new());
        }
        if let Some(w) = words.last_mut() {
            w.push(c);
        }
        prev = Some(c);
    }
    let style = ["Bold", "Italic", "Oblique", "Regular", "Light", "Medium", "Semibold", "Black", "Heavy"];
    while words.len() > 1 && words.last().is_some_and(|w| w.is_empty() || style.contains(&w.as_str())) {
        words.pop();
    }
    words.retain(|w| !w.is_empty());
    let name = words.join(" ");
    match name.as_str() {
        "Helvetica" | "Arial" | "Helvetica Neue" => "Arial".into(),
        "Times" | "Times New Roman" => "Times New Roman".into(),
        "Courier" | "Courier New" => "Courier New".into(),
        "Zapf Dingbats" => "ZapfDingbats".into(),
        _ => name.chars().take(64).collect(),
    }
}

const NS: &str = "xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\"";
const XML: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const GROUP: &str = "<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>\
<p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>";

fn emu(pt: f64) -> i64 {
    // Bounded well inside i64 and inside what PowerPoint accepts for a coordinate.
    if pt.is_finite() { (pt * EMU).round().clamp(-2.0e10, 2.0e10) as i64 } else { 0 }
}

/// How a page maps onto its slide: scale, then offset (points).
#[derive(Clone, Copy)]
struct Fit {
    k: f64,
    ox: f64,
    oy: f64,
}

fn fit(slide: &Slide, sw: f64, sh: f64) -> Fit {
    let ok = |v: f64| v.is_finite() && v > 0.0;
    if !ok(slide.width) || !ok(slide.height) {
        return Fit { k: 1.0, ox: 0.0, oy: 0.0 };
    }
    let k = (sw / slide.width).min(sh / slide.height);
    let k = if ok(k) { k } else { 1.0 };
    Fit { k, ox: (sw - slide.width * k) / 2.0, oy: (sh - slide.height * k) / 2.0 }
}

/// The `a:xfrm` of a placement, or `None` when it is degenerate or not finite.
fn xfrm(at: &Placement, f: Fit) -> Option<String> {
    let vals = [at.x, at.y, at.w, at.h, at.rot];
    if vals.iter().any(|v| !v.is_finite()) || at.w <= 0.0 || at.h <= 0.0 {
        return None;
    }
    let rot = ((at.rot.rem_euclid(360.0) * 60_000.0).round() as i64).rem_euclid(21_600_000);
    let mut attrs = String::new();
    if rot != 0 {
        attrs.push_str(&format!(" rot=\"{rot}\""));
    }
    if at.flip_v {
        attrs.push_str(" flipV=\"1\"");
    }
    let (cx, cy) = (emu(at.w * f.k).max(1), emu(at.h * f.k).max(1));
    Some(format!(
        "<a:xfrm{attrs}><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm>",
        emu(f.ox + at.x * f.k),
        emu(f.oy + at.y * f.k)
    ))
}

fn text_xml(t: &SlideText, id: usize, f: Fit) -> Option<String> {
    // A little extra width so a substituted, slightly wider font doesn't wrap a line early.
    let mut at = t.at;
    let slack = (at.w * 0.03).max(1.0);
    at.w += slack;
    at.x -= if at.rot == 0.0 { 0.0 } else { slack / 2.0 };
    let xf = xfrm(&at, f)?;
    let size = if t.size.is_finite() && t.size > 0.0 { t.size } else { 12.0 };
    let sz = ((size * f.k * 100.0).round() as i64).clamp(100, 400_000);
    let [r, g, b] = rgb8(t.color);
    let mut rpr = format!("<a:rPr lang=\"en-US\" sz=\"{sz}\"");
    if t.bold {
        rpr.push_str(" b=\"1\"");
    }
    if t.italic {
        rpr.push_str(" i=\"1\"");
    }
    rpr.push_str(&format!(" dirty=\"0\"><a:solidFill><a:srgbClr val=\"{r:02X}{g:02X}{b:02X}\"/></a:solidFill>"));
    let font = esc(&t.font);
    if !font.is_empty() {
        rpr.push_str(&format!("<a:latin typeface=\"{font}\"/><a:cs typeface=\"{font}\"/>"));
    }
    rpr.push_str("</a:rPr>");
    // The line pitch of a multi-line paragraph, so it fills its box as it did the page.
    let ppr = if t.lines > 1 && t.at.h.is_finite() {
        let pitch = ((t.at.h * f.k / t.lines as f64) * 100.0).round() as i64;
        if pitch > 0 { format!("<a:pPr><a:lnSpc><a:spcPts val=\"{}\"/></a:lnSpc></a:pPr>", pitch.min(158_400)) } else { String::new() }
    } else {
        String::new()
    };
    let text: String = t.text.chars().take(MAX_TEXT_CHARS).collect();
    let mut paras = String::new();
    for line in text.split('\n') {
        let line = esc(line.trim_end_matches('\r'));
        if line.is_empty() {
            paras.push_str(&format!("<a:p>{ppr}<a:endParaRPr lang=\"en-US\" sz=\"{sz}\" dirty=\"0\"/></a:p>"));
        } else {
            paras.push_str(&format!("<a:p>{ppr}<a:r>{rpr}<a:t>{line}</a:t></a:r></a:p>"));
        }
    }
    Some(format!(
        "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"Text {id}\"/><p:cNvSpPr txBox=\"1\"/><p:nvPr/></p:nvSpPr>\
<p:spPr>{xf}<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom><a:noFill/></p:spPr>\
<p:txBody><a:bodyPr wrap=\"square\" lIns=\"0\" tIns=\"0\" rIns=\"0\" bIns=\"0\" rtlCol=\"0\" anchor=\"t\"><a:noAutofit/></a:bodyPr><a:lstStyle/>{paras}</p:txBody></p:sp>"
    ))
}

fn picture_xml(id: usize, name: &str, rid: &str, xf: &str) -> String {
    format!(
        "<p:pic><p:nvPicPr><p:cNvPr id=\"{id}\" name=\"{name}\"/><p:cNvPicPr><a:picLocks noChangeAspect=\"1\"/></p:cNvPicPr><p:nvPr/></p:nvPicPr>\
<p:blipFill><a:blip r:embed=\"{rid}\"/><a:stretch><a:fillRect/></a:stretch></p:blipFill>\
<p:spPr>{xf}<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></p:spPr></p:pic>"
    )
}

/// Image data the package can carry: a PNG or JPEG by its signature.
fn media_ext(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else {
        None
    }
}

/// The media files, stored once per distinct content.
#[derive(Default)]
struct Media<'a> {
    files: Vec<(String, &'a [u8])>,
    index: std::collections::HashMap<&'a [u8], usize>,
    bytes: usize,
}

impl<'a> Media<'a> {
    /// The part name of `bytes` (added if new), or `None` past the limits or for data that
    /// is not PNG/JPEG.
    fn add(&mut self, bytes: &'a [u8]) -> Option<String> {
        let ext = media_ext(bytes)?;
        if let Some(&i) = self.index.get(bytes) {
            return self.files.get(i).map(|f| f.0.clone());
        }
        let total = self.bytes.checked_add(bytes.len())?;
        if self.files.len() >= MAX_MEDIA || total > MAX_MEDIA_BYTES {
            return None;
        }
        self.bytes = total;
        let name = format!("image{}.{ext}", self.files.len() + 1);
        self.index.insert(bytes, self.files.len());
        self.files.push((name.clone(), bytes));
        Some(name)
    }
}

/// The presentation: one slide per entry of `slides` (see the module notes), and what went in.
pub fn pptx(slides: &[Slide], title: &str) -> (Vec<u8>, PptxStats) {
    let mut stats = PptxStats { omitted: slides.len().saturating_sub(MAX_SLIDES), ..Default::default() };
    let slides = slides.get(..slides.len().min(MAX_SLIDES)).unwrap_or(&[]);
    let size = |v: Option<f64>, default: f64| v.filter(|v| v.is_finite() && *v > 0.0).map_or(default, |v| v.clamp(MIN_SLIDE, MAX_SLIDE));
    let sw = size(slides.first().map(|s| s.width), 720.0);
    let sh = size(slides.first().map(|s| s.height), 540.0);
    let mut media = Media::default();
    let mut parts: Vec<(String, String)> = Vec::new(); // (slide xml, its relationships)
    for slide in slides {
        let f = fit(slide, sw, sh);
        let mut shapes = String::new();
        let mut rels = format!(
            "{XML}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"{REL}/slideLayout\" Target=\"../slideLayouts/slideLayout1.xml\"/>"
        );
        let mut next_rel = 2usize;
        let mut id = 2usize;
        let mut used: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut rel_for = |name: String, rels: &mut String, next_rel: &mut usize| -> String {
            if let Some(r) = used.get(&name) {
                return r.clone();
            }
            let rid = format!("rId{next_rel}");
            *next_rel += 1;
            rels.push_str(&format!("<Relationship Id=\"{rid}\" Type=\"{REL}/image\" Target=\"../media/{name}\"/>"));
            used.insert(name, rid.clone());
            rid
        };
        if let Some(bg) = &slide.background {
            let at = Placement { x: 0.0, y: 0.0, w: slide.width, h: slide.height, rot: 0.0, flip_v: false };
            match (media.add(bg), xfrm(&at, f)) {
                (Some(name), Some(xf)) => {
                    let rid = rel_for(name, &mut rels, &mut next_rel);
                    shapes.push_str(&picture_xml(id, "Page background", &rid, &xf));
                    id += 1;
                    stats.backgrounds += 1;
                }
                _ => stats.omitted += 1,
            }
        }
        let mut budget = MAX_SHAPES;
        for p in &slide.pictures {
            let placed = (budget > 0).then(|| xfrm(&p.at, f)).flatten().and_then(|xf| Some((xf, media.add(&p.bytes)?)));
            let Some((xf, name)) = placed else {
                stats.omitted += 1;
                continue;
            };
            let rid = rel_for(name, &mut rels, &mut next_rel);
            shapes.push_str(&picture_xml(id, &format!("Picture {id}"), &rid, &xf));
            id += 1;
            budget -= 1;
            stats.pictures += 1;
        }
        for t in &slide.texts {
            if t.text.trim().is_empty() {
                continue;
            }
            let Some(x) = (budget > 0).then(|| text_xml(t, id, f)).flatten() else {
                stats.omitted += 1;
                continue;
            };
            shapes.push_str(&x);
            id += 1;
            budget -= 1;
            stats.text_boxes += 1;
        }
        rels.push_str("</Relationships>");
        let xml =
            format!("{XML}<p:sld {NS}><p:cSld><p:spTree>{GROUP}{shapes}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>");
        parts.push((xml, rels));
        stats.slides += 1;
    }

    let n = parts.len();
    let mut types = format!(
        "{XML}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Default Extension=\"png\" ContentType=\"image/png\"/><Default Extension=\"jpg\" ContentType=\"image/jpeg\"/>\
<Override PartName=\"/ppt/presentation.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml\"/>\
<Override PartName=\"/ppt/slideMasters/slideMaster1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml\"/>\
<Override PartName=\"/ppt/slideLayouts/slideLayout1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml\"/>\
<Override PartName=\"/ppt/theme/theme1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.theme+xml\"/>\
<Override PartName=\"/ppt/presProps.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.presProps+xml\"/>\
<Override PartName=\"/ppt/viewProps.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.viewProps+xml\"/>\
<Override PartName=\"/ppt/tableStyles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.tableStyles+xml\"/>\
<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>\
<Override PartName=\"/docProps/app.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.extended-properties+xml\"/>"
    );
    for i in 1..=n {
        types.push_str(&format!(
            "<Override PartName=\"/ppt/slides/slide{i}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slide+xml\"/>"
        ));
    }
    types.push_str("</Types>");
    let root_rels = format!(
        "{XML}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"{REL}/officeDocument\" Target=\"ppt/presentation.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>\
<Relationship Id=\"rId3\" Type=\"{REL}/extended-properties\" Target=\"docProps/app.xml\"/></Relationships>"
    );
    let mut pres_rels = format!(
        "{XML}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"{REL}/slideMaster\" Target=\"slideMasters/slideMaster1.xml\"/>\
<Relationship Id=\"rId2\" Type=\"{REL}/theme\" Target=\"theme/theme1.xml\"/>\
<Relationship Id=\"rId3\" Type=\"{REL}/presProps\" Target=\"presProps.xml\"/>\
<Relationship Id=\"rId4\" Type=\"{REL}/viewProps\" Target=\"viewProps.xml\"/>\
<Relationship Id=\"rId5\" Type=\"{REL}/tableStyles\" Target=\"tableStyles.xml\"/>"
    );
    let mut ids = String::new();
    for i in 1..=n {
        pres_rels.push_str(&format!("<Relationship Id=\"rId{}\" Type=\"{REL}/slide\" Target=\"slides/slide{i}.xml\"/>", i + 5));
        // Slide ids start at 256 (§19.2.1.33).
        ids.push_str(&format!("<p:sldId id=\"{}\" r:id=\"rId{}\"/>", 255 + i, i + 5));
    }
    pres_rels.push_str("</Relationships>");
    let ids = if ids.is_empty() { String::new() } else { format!("<p:sldIdLst>{ids}</p:sldIdLst>") };
    let presentation = format!(
        "{XML}<p:presentation {NS} saveSubsetFonts=\"1\"><p:sldMasterIdLst><p:sldMasterId id=\"2147483648\" r:id=\"rId1\"/></p:sldMasterIdLst>\
{ids}<p:sldSz cx=\"{}\" cy=\"{}\"/><p:notesSz cx=\"6858000\" cy=\"9144000\"/>\
<p:defaultTextStyle><a:defPPr><a:defRPr lang=\"en-US\"/></a:defPPr></p:defaultTextStyle></p:presentation>",
        emu(sw),
        emu(sh)
    );
    let master = format!(
        "{XML}<p:sldMaster {NS}><p:cSld><p:bg><p:bgRef idx=\"1001\"><a:schemeClr val=\"bg1\"/></p:bgRef></p:bg><p:spTree>{GROUP}</p:spTree></p:cSld>\
<p:clrMap bg1=\"lt1\" tx1=\"dk1\" bg2=\"lt2\" tx2=\"dk2\" accent1=\"accent1\" accent2=\"accent2\" accent3=\"accent3\" accent4=\"accent4\" accent5=\"accent5\" accent6=\"accent6\" hlink=\"hlink\" folHlink=\"folHlink\"/>\
<p:sldLayoutIdLst><p:sldLayoutId id=\"2147483649\" r:id=\"rId1\"/></p:sldLayoutIdLst>\
<p:txStyles><p:titleStyle><a:lvl1pPr><a:defRPr sz=\"4400\"><a:solidFill><a:schemeClr val=\"tx1\"/></a:solidFill><a:latin typeface=\"+mj-lt\"/></a:defRPr></a:lvl1pPr></p:titleStyle>\
<p:bodyStyle><a:lvl1pPr><a:defRPr sz=\"1800\"><a:solidFill><a:schemeClr val=\"tx1\"/></a:solidFill><a:latin typeface=\"+mn-lt\"/></a:defRPr></a:lvl1pPr></p:bodyStyle>\
<p:otherStyle><a:lvl1pPr><a:defRPr sz=\"1800\"><a:solidFill><a:schemeClr val=\"tx1\"/></a:solidFill><a:latin typeface=\"+mn-lt\"/></a:defRPr></a:lvl1pPr></p:otherStyle></p:txStyles></p:sldMaster>"
    );
    let master_rels = format!(
        "{XML}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"{REL}/slideLayout\" Target=\"../slideLayouts/slideLayout1.xml\"/>\
<Relationship Id=\"rId2\" Type=\"{REL}/theme\" Target=\"../theme/theme1.xml\"/></Relationships>"
    );
    let layout = format!(
        "{XML}<p:sldLayout {NS} type=\"blank\" preserve=\"1\"><p:cSld name=\"Blank\"><p:spTree>{GROUP}</p:spTree></p:cSld>\
<p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"
    );
    let layout_rels = format!(
        "{XML}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"{REL}/slideMaster\" Target=\"../slideMasters/slideMaster1.xml\"/></Relationships>"
    );
    let pres_props = format!("{XML}<p:presentationPr {NS}/>");
    let view_props = format!(
        "{XML}<p:viewPr {NS}><p:normalViewPr><p:restoredLeft sz=\"15620\"/><p:restoredTop sz=\"94660\"/></p:normalViewPr><p:gridSpacing cx=\"76200\" cy=\"76200\"/></p:viewPr>"
    );
    let table_styles = format!(
        "{XML}<a:tblStyleLst xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" def=\"{{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}}\"/>"
    );
    let core = format!(
        "{XML}<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>{}</dc:title></cp:coreProperties>",
        esc(title)
    );
    let app = format!(
        "{XML}<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\"><Application>PDFCraft</Application><Slides>{n}</Slides></Properties>"
    );

    let mut z = Zip::default();
    z.add("[Content_Types].xml", types.as_bytes(), true);
    z.add("_rels/.rels", root_rels.as_bytes(), true);
    z.add("docProps/core.xml", core.as_bytes(), true);
    z.add("docProps/app.xml", app.as_bytes(), true);
    z.add("ppt/presentation.xml", presentation.as_bytes(), true);
    z.add("ppt/_rels/presentation.xml.rels", pres_rels.as_bytes(), true);
    z.add("ppt/slideMasters/slideMaster1.xml", master.as_bytes(), true);
    z.add("ppt/slideMasters/_rels/slideMaster1.xml.rels", master_rels.as_bytes(), true);
    z.add("ppt/slideLayouts/slideLayout1.xml", layout.as_bytes(), true);
    z.add("ppt/slideLayouts/_rels/slideLayout1.xml.rels", layout_rels.as_bytes(), true);
    z.add("ppt/theme/theme1.xml", THEME.as_bytes(), true);
    z.add("ppt/presProps.xml", pres_props.as_bytes(), true);
    z.add("ppt/viewProps.xml", view_props.as_bytes(), true);
    z.add("ppt/tableStyles.xml", table_styles.as_bytes(), true);
    for (i, (xml, rels)) in parts.iter().enumerate() {
        z.add(&format!("ppt/slides/slide{}.xml", i + 1), xml.as_bytes(), true);
        z.add(&format!("ppt/slides/_rels/slide{}.xml.rels", i + 1), rels.as_bytes(), true);
    }
    for (name, bytes) in &media.files {
        z.add(&format!("ppt/media/{name}"), bytes, false);
    }
    (z.finish(), stats)
}

/// A complete theme (§20.1.6): every scheme a master's references can resolve against. The
/// colours and fonts are plain defaults, not taken from any Office template.
const THEME: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<a:theme xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" name=\"PDFCraft\"><a:themeElements>\
<a:clrScheme name=\"PDFCraft\"><a:dk1><a:srgbClr val=\"000000\"/></a:dk1><a:lt1><a:srgbClr val=\"FFFFFF\"/></a:lt1>\
<a:dk2><a:srgbClr val=\"1F2937\"/></a:dk2><a:lt2><a:srgbClr val=\"F3F4F6\"/></a:lt2>\
<a:accent1><a:srgbClr val=\"2563EB\"/></a:accent1><a:accent2><a:srgbClr val=\"DC2626\"/></a:accent2>\
<a:accent3><a:srgbClr val=\"16A34A\"/></a:accent3><a:accent4><a:srgbClr val=\"9333EA\"/></a:accent4>\
<a:accent5><a:srgbClr val=\"0891B2\"/></a:accent5><a:accent6><a:srgbClr val=\"EA580C\"/></a:accent6>\
<a:hlink><a:srgbClr val=\"1D4ED8\"/></a:hlink><a:folHlink><a:srgbClr val=\"7C3AED\"/></a:folHlink></a:clrScheme>\
<a:fontScheme name=\"PDFCraft\"><a:majorFont><a:latin typeface=\"Arial\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:majorFont>\
<a:minorFont><a:latin typeface=\"Arial\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:minorFont></a:fontScheme>\
<a:fmtScheme name=\"PDFCraft\"><a:fillStyleLst><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>\
<a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:fillStyleLst>\
<a:lnStyleLst><a:ln w=\"6350\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln>\
<a:ln w=\"12700\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln>\
<a:ln w=\"19050\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln></a:lnStyleLst>\
<a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst>\
<a:bgFillStyleLst><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>\
<a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:bgFillStyleLst></a:fmtScheme></a:themeElements>\
<a:objectDefaults/><a:extraClrSchemeLst/></a:theme>";

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Every entry of a zip written by [`Zip`], inflated, read through the central directory
    /// (as a consumer does) and checked against the local headers and CRCs.
    fn unzip(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
        let n16 = |i: usize| u16::from_le_bytes([zip[i], zip[i + 1]]) as usize;
        let n32 = |i: usize| u32::from_le_bytes([zip[i], zip[i + 1], zip[i + 2], zip[i + 3]]) as usize;
        let end = zip.len() - 22;
        assert_eq!(&zip[end..end + 4], b"PK\x05\x06");
        let (count, mut at) = (n16(end + 10), n32(end + 16));
        let mut out = Vec::new();
        for _ in 0..count {
            assert_eq!(&zip[at..at + 4], b"PK\x01\x02");
            let (method, crc, csize, name_len, local) = (n16(at + 10), n32(at + 16), n32(at + 20), n16(at + 28), n32(at + 42));
            let name = String::from_utf8(zip[at + 46..at + 46 + name_len].to_vec()).unwrap();
            assert_eq!(&zip[local..local + 4], b"PK\x03\x04", "{name}");
            let data = local + 30 + n16(local + 26) + n16(local + 28);
            let raw = &zip[data..data + csize];
            let body = if method == 8 {
                let mut v = Vec::new();
                std::io::Read::read_to_end(&mut flate2::read::DeflateDecoder::new(raw), &mut v).unwrap();
                v
            } else {
                raw.to_vec()
            };
            assert_eq!(crate::zip::crc32(&body) as usize, crc, "{name}");
            out.push((name, body));
            at += 46 + name_len + n16(at + 30) + n16(at + 32);
        }
        out
    }

    struct Package(Vec<(String, Vec<u8>)>);

    impl Package {
        fn new(bytes: &[u8]) -> Self {
            let p = Package(unzip(bytes));
            p.check();
            p
        }
        fn names(&self) -> Vec<&str> {
            self.0.iter().map(|p| p.0.as_str()).collect()
        }
        fn text(&self, name: &str) -> String {
            let b = self.0.iter().find(|p| p.0 == name).unwrap_or_else(|| panic!("{name} missing: {:?}", self.names()));
            String::from_utf8(b.1.clone()).unwrap()
        }
        /// Every XML part parses; every relationship target exists and every `r:id`/`r:embed`
        /// a part uses is one of its relationships; every part has a content type.
        fn check(&self) {
            let types = self.text("[Content_Types].xml");
            let tdoc = roxmltree::Document::parse(&types).unwrap();
            let defaults: Vec<String> =
                tdoc.descendants().filter(|n| n.has_tag_name("Default")).map(|n| n.attribute("Extension").unwrap().to_string()).collect();
            let overrides: Vec<String> =
                tdoc.descendants().filter(|n| n.has_tag_name("Override")).map(|n| n.attribute("PartName").unwrap().to_string()).collect();
            for (name, body) in &self.0 {
                if name == "[Content_Types].xml" {
                    continue;
                }
                let ext = name.rsplit('.').next().unwrap();
                assert!(overrides.contains(&format!("/{name}")) || defaults.iter().any(|d| d == ext), "no content type for {name}");
                if ext == "xml" || ext == "rels" {
                    let s = std::str::from_utf8(body).unwrap();
                    roxmltree::Document::parse(s).unwrap_or_else(|e| panic!("{name}: {e}"));
                }
            }
            for o in &overrides {
                assert!(self.names().contains(&&o[1..]), "override for missing part {o}");
            }
            for (name, body) in &self.0 {
                if !name.ends_with(".xml") || name.starts_with('[') {
                    continue;
                }
                let (dir, file) = name.rsplit_once('/').map_or(("", name.as_str()), |(d, f)| (d, f));
                let rels_name = if dir.is_empty() { format!("_rels/{file}.rels") } else { format!("{dir}/_rels/{file}.rels") };
                let mut ids = HashMap::new();
                if self.names().contains(&rels_name.as_str()) {
                    let rels = self.text(&rels_name);
                    for r in roxmltree::Document::parse(&rels).unwrap().descendants().filter(|n| n.has_tag_name("Relationship")) {
                        let target = r.attribute("Target").unwrap();
                        let resolved = resolve(dir, target);
                        assert!(self.names().contains(&resolved.as_str()), "{rels_name}: {target} → {resolved} missing");
                        assert!(ids.insert(r.attribute("Id").unwrap().to_string(), resolved).is_none(), "duplicate id in {rels_name}");
                    }
                }
                let s = std::str::from_utf8(body).unwrap();
                let doc = roxmltree::Document::parse(s).unwrap();
                for n in doc.descendants() {
                    for a in n.attributes() {
                        if a.namespace() == Some(REL) {
                            assert!(ids.contains_key(a.value()), "{name}: {} = {} has no relationship", a.name(), a.value());
                        }
                    }
                }
            }
            let root = self.text("_rels/.rels");
            assert!(root.contains("Target=\"ppt/presentation.xml\""));
        }
    }

    fn resolve(dir: &str, target: &str) -> String {
        let mut parts: Vec<&str> = if dir.is_empty() { Vec::new() } else { dir.split('/').collect() };
        for seg in target.split('/') {
            match seg {
                ".." => {
                    parts.pop();
                }
                "." => {}
                s => parts.push(s),
            }
        }
        parts.join("/")
    }

    fn png() -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend([0u8; 16]);
        v
    }

    fn text(t: &str, x: f64, y: f64) -> SlideText {
        SlideText {
            text: t.into(),
            font: "Arial".into(),
            size: 12.0,
            bold: true,
            italic: false,
            color: [1.0, 0.0, 0.0],
            lines: 1,
            at: Placement { x, y, w: 200.0, h: 14.0, rot: 0.0, flip_v: false },
        }
    }

    fn slide() -> Slide {
        Slide {
            width: 612.0,
            height: 792.0,
            background: Some(png()),
            pictures: vec![SlidePicture {
                bytes: vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2],
                at: Placement { x: 72.0, y: 300.0, w: 100.0, h: 50.0, rot: 0.0, flip_v: false },
            }],
            texts: vec![text("Hello & <world>", 72.0, 72.0)],
        }
    }

    #[test]
    fn package_has_every_part_and_consistent_relationships() {
        let (bytes, stats) = pptx(&[slide(), Slide { width: 612.0, height: 792.0, ..Default::default() }], "Deck & co");
        assert_eq!(stats, PptxStats { slides: 2, text_boxes: 1, pictures: 1, backgrounds: 1, omitted: 0 });
        let p = Package::new(&bytes);
        for n in [
            "[Content_Types].xml",
            "_rels/.rels",
            "docProps/core.xml",
            "docProps/app.xml",
            "ppt/presentation.xml",
            "ppt/_rels/presentation.xml.rels",
            "ppt/slideMasters/slideMaster1.xml",
            "ppt/slideLayouts/slideLayout1.xml",
            "ppt/theme/theme1.xml",
            "ppt/slides/slide1.xml",
            "ppt/slides/slide2.xml",
            "ppt/slides/_rels/slide1.xml.rels",
            "ppt/media/image1.png",
            "ppt/media/image2.jpg",
        ] {
            assert!(p.names().contains(&n), "{n}: {:?}", p.names());
        }
        let pres = p.text("ppt/presentation.xml");
        // 612 × 792 pt in EMU.
        assert!(pres.contains("<p:sldSz cx=\"7772400\" cy=\"10058400\"/>"), "{pres}");
        assert!(pres.contains("<p:sldId id=\"256\" r:id=\"rId6\"/><p:sldId id=\"257\" r:id=\"rId7\"/>"));
        assert!(p.text("docProps/core.xml").contains("Deck &amp; co"));
    }

    #[test]
    fn text_boxes_carry_position_font_and_style() {
        let (bytes, _) = pptx(&[slide()], "t");
        let s = Package::new(&bytes).text("ppt/slides/slide1.xml");
        let doc = roxmltree::Document::parse(&s).unwrap();
        let sp = doc.descendants().find(|n| n.has_tag_name("sp")).unwrap();
        let off = sp.descendants().find(|n| n.has_tag_name("off")).unwrap();
        assert_eq!(off.attribute("x"), Some("914400"));
        assert_eq!(off.attribute("y"), Some("914400"));
        let rpr = sp.descendants().find(|n| n.has_tag_name("rPr")).unwrap();
        assert_eq!(rpr.attribute("sz"), Some("1200"));
        assert_eq!(rpr.attribute("b"), Some("1"));
        assert_eq!(rpr.attribute("i"), None);
        assert!(sp.descendants().any(|n| n.has_tag_name("srgbClr") && n.attribute("val") == Some("FF0000")));
        assert!(sp.descendants().any(|n| n.has_tag_name("latin") && n.attribute("typeface") == Some("Arial")));
        let t: String = sp.descendants().filter(|n| n.has_tag_name("t")).filter_map(|n| n.text()).collect();
        assert_eq!(t, "Hello & <world>");
        // Background first, then the picture, then the text.
        let order: Vec<&str> = doc.descendants().filter(|n| n.has_tag_name("cNvPr")).map(|n| n.attribute("name").unwrap()).collect();
        assert_eq!(order, ["", "Page background", "Picture 3", "Text 4"]);
        let ids: Vec<&str> = doc.descendants().filter(|n| n.has_tag_name("cNvPr")).map(|n| n.attribute("id").unwrap()).collect();
        assert_eq!(ids, ["1", "2", "3", "4"]);
    }

    #[test]
    fn rotation_flip_and_line_pitch() {
        let mut s = slide();
        s.texts[0].at.rot = 90.0;
        s.texts[0].lines = 2;
        s.texts[0].at.h = 30.0;
        s.pictures[0].at.flip_v = true;
        s.pictures[0].at.rot = -90.0;
        let (bytes, _) = pptx(&[s], "t");
        let x = Package::new(&bytes).text("ppt/slides/slide1.xml");
        assert!(x.contains("<a:xfrm rot=\"5400000\">"), "{x}");
        assert!(x.contains("<a:xfrm rot=\"16200000\" flipV=\"1\">"), "{x}");
        assert!(x.contains("<a:spcPts val=\"1500\"/>"), "{x}");
    }

    #[test]
    fn other_page_sizes_are_scaled_and_centred() {
        let first = Slide { width: 720.0, height: 540.0, ..Default::default() };
        let mut second = slide(); // 612 × 792 into 720 × 540: k = 540 / 792
        second.background = None;
        second.pictures.clear();
        second.texts[0].at = Placement { x: 0.0, y: 0.0, w: 612.0, h: 792.0, rot: 0.0, flip_v: false };
        let (bytes, _) = pptx(&[first, second], "t");
        let x = Package::new(&bytes).text("ppt/slides/slide2.xml");
        let k = 540.0 / 792.0;
        let ox = (720.0 - 612.0 * k) / 2.0;
        assert!(x.contains(&format!("<a:off x=\"{}\" y=\"0\"/>", emu(ox))), "{x}");
        assert!(x.contains(&format!("sz=\"{}\"", (12.0 * k * 100.0_f64).round() as i64)), "{x}");
    }

    #[test]
    fn identical_images_are_stored_once() {
        let (bytes, stats) = pptx(&[slide(), slide(), slide()], "t");
        assert_eq!(stats.pictures, 3);
        let p = Package::new(&bytes);
        assert_eq!(p.names().iter().filter(|n| n.starts_with("ppt/media/")).count(), 2);
        assert!(p.text("ppt/slides/_rels/slide3.xml.rels").contains("../media/image1.png"));
    }

    #[test]
    fn empty_presentation_is_valid() {
        let (bytes, stats) = pptx(&[], "");
        assert_eq!(stats.slides, 0);
        let p = Package::new(&bytes);
        let pres = p.text("ppt/presentation.xml");
        assert!(!pres.contains("sldIdLst"));
        assert!(pres.contains("cx=\"9144000\" cy=\"6858000\""), "{pres}");
    }

    #[test]
    fn hostile_values_neither_panic_nor_break_the_package() {
        let bad = Placement { x: f64::NAN, y: f64::INFINITY, w: -5.0, h: 1e308, rot: f64::NAN, flip_v: false };
        let huge = Placement { x: 1e300, y: -1e300, w: 1e300, h: 1e300, rot: 1e300, flip_v: true };
        let s = Slide {
            width: f64::NAN,
            height: -1.0,
            background: Some(b"not a png".to_vec()),
            pictures: vec![
                SlidePicture { bytes: Vec::new(), at: huge },
                SlidePicture { bytes: png(), at: bad },
                SlidePicture { bytes: png(), at: huge },
            ],
            texts: vec![
                SlideText {
                    text: "\u{0}\u{1}x\u{FFFF}\n\n]]>".into(),
                    font: "\u{7}<\"&".into(),
                    size: f64::NAN,
                    bold: false,
                    italic: true,
                    color: [f64::NAN, 9.0, -3.0],
                    lines: usize::MAX,
                    at: huge,
                },
                SlideText { text: "y".into(), font: String::new(), size: 1e300, bold: false, italic: false, color: [0.0; 3], lines: 0, at: bad },
                SlideText { text: "   ".into(), font: String::new(), size: 0.0, bold: false, italic: false, color: [0.0; 3], lines: 0, at: huge },
            ],
        };
        let tiny = Slide { width: 1e-300, height: 1e300, ..Default::default() };
        let (bytes, stats) = pptx(&[s, tiny], "\u{0}");
        assert_eq!(stats.slides, 2);
        assert_eq!(stats.pictures, 1);
        assert_eq!(stats.text_boxes, 1);
        assert_eq!(stats.omitted, 4); // the background, two pictures and one text box
        let p = Package::new(&bytes);
        let x = p.text("ppt/slides/slide1.xml");
        assert!(x.contains("<a:t>x</a:t>") && x.contains("<a:t>]]&gt;</a:t>"), "{x}");
        assert!(x.contains("sz=\"1200\""), "a NaN size falls back to 12 pt: {x}");
    }

    #[test]
    fn slide_and_shape_limits_hold() {
        let many: Vec<Slide> = (0..MAX_SLIDES + 3).map(|_| Slide { width: 100.0, height: 100.0, ..Default::default() }).collect();
        let (_, stats) = pptx(&many, "t");
        assert_eq!((stats.slides, stats.omitted), (MAX_SLIDES, 3));
        let crowded = Slide {
            width: 100.0,
            height: 100.0,
            texts: (0..MAX_SHAPES + 2).map(|i| text("a", i as f64 % 50.0, 0.0)).collect(),
            ..Default::default()
        };
        let (bytes, stats) = pptx(&[crowded], "t");
        assert_eq!((stats.text_boxes, stats.omitted), (MAX_SHAPES, 2));
        Package::new(&bytes);
    }

    #[test]
    fn font_names_become_families() {
        for (pdf, family) in [
            ("ABCDEF+ArialMT", "Arial"),
            ("Arial-BoldMT", "Arial"),
            ("Helvetica-Oblique", "Arial"),
            ("Times-Roman", "Times New Roman"),
            ("TimesNewRomanPS-BoldItalicMT", "Times New Roman"),
            ("TimesNewRomanPSMT", "Times New Roman"),
            ("CourierNewPSMT", "Courier New"),
            ("Courier", "Courier New"),
            ("SourceSansPro-Regular", "Source Sans Pro"),
            ("Calibri,Bold", "Calibri"),
            ("SegoeUI", "Segoe UI"),
            ("Georgia Bold", "Georgia"),
            ("", ""),
            ("abc+", "abc+"),
        ] {
            assert_eq!(font_family(pdf), family, "{pdf}");
        }
        assert!(font_family(&"X".repeat(10_000)).chars().count() <= 64);
    }
}

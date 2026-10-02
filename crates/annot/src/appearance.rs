//! Appearance streams (§12.5.5) for the comment types PrintCraft creates, execution plan M5.2.
//!
//! [`build`] draws a normal appearance (`/AP /N`) from the annotation dictionary alone, so the
//! same code serves new comments and restyled ones. Drawings are in page space with
//! `/BBox = /Rect` (identity matrix), except note icons, which are drawn in a 20 × 20 box.
//! It returns `None` for anything it cannot draw faithfully (cloudy borders, unknown line
//! endings, indirect geometry), so callers never replace an appearance with a worse one.
//!
//! The note icons are PrintCraft's own drawings (AGENTS.md §1). Text boxes use the standard
//! Helvetica font with WinAnsi encoding; line breaking uses [`text_width`], an approximation of
//! Helvetica's proportions by character class (no font program or metrics file is bundled).

use printcraft_cos::{Dict, Object, PdfString, Stream};
pub use printcraft_fonts::{helvetica_width as text_width, wrap};
use printcraft_fonts::{literal, win_ansi};

use crate::{NOTE_SIZE, Rgb, n};

/// Length of an arrowhead for a line of width `w`.
pub fn arrow_size(w: f64) -> f64 {
    6.0 + 3.0 * w.max(0.0)
}

fn nums(d: &Dict, key: &[u8]) -> Option<Vec<f64>> {
    d.get(key)?.as_array()?.iter().map(|o| o.as_f64()).collect()
}

/// `/C`-style colour arrays: none (transparent), gray, RGB or CMYK.
fn color(d: &Dict, key: &[u8]) -> Option<Option<Rgb>> {
    let c = match d.get(key) {
        None => return Some(None),
        Some(o) => o.as_array()?.iter().map(|o| o.as_f64()).collect::<Option<Vec<f64>>>()?,
    };
    Some(match c.as_slice() {
        [] => None,
        [g] => Some([*g; 3]),
        [r, g, b] => Some([*r, *g, *b]),
        [c, m, y, k] => Some([(1.0 - c) * (1.0 - k), (1.0 - m) * (1.0 - k), (1.0 - y) * (1.0 - k)]),
        _ => return None,
    })
}

fn border_width(d: &Dict) -> f64 {
    if let Some(w) = d.get(b"BS").and_then(|b| b.as_dict()).and_then(|b| b.get(b"W")).and_then(|w| w.as_f64()) {
        return w.max(0.0);
    }
    match nums(d, b"Border") {
        Some(b) if b.len() >= 3 => b[2].max(0.0),
        _ => 1.0,
    }
}

/// Dash pattern operator for a dashed border, if any.
fn dash(d: &Dict) -> String {
    let Some(bs) = d.get(b"BS").and_then(|b| b.as_dict()) else { return String::new() };
    if bs.name(b"S") != Some(b"D") {
        return String::new();
    }
    let arr = nums(bs, b"D").filter(|a| !a.is_empty() && a.iter().all(|x| *x >= 0.0)).unwrap_or_else(|| vec![3.0]);
    format!("[{}] 0 d\n", arr.iter().map(|x| n(*x)).collect::<Vec<_>>().join(" "))
}

fn rg(c: Rgb) -> String {
    format!("{} {} {} rg\n", n(c[0]), n(c[1]), n(c[2]))
}

fn rg_stroke(c: Rgb) -> String {
    format!("{} {} {} RG\n", n(c[0]), n(c[1]), n(c[2]))
}

/// `/DA` of a text box: (text colour, font size). Defaults to black 12 pt.
pub fn parse_da(d: &Dict) -> (Rgb, f64) {
    let da = d.get(b"DA").and_then(|o| o.as_string()).map(|s| String::from_utf8_lossy(&s.bytes).into_owned()).unwrap_or_default();
    let toks: Vec<&str> = da.split_whitespace().collect();
    let mut col = [0.0; 3];
    let mut size = 12.0;
    let num = |i: usize| toks.get(i).and_then(|t| t.parse::<f64>().ok());
    for (i, t) in toks.iter().enumerate() {
        match *t {
            "rg" if i >= 3 => {
                if let (Some(r), Some(g), Some(b)) = (num(i - 3), num(i - 2), num(i - 1)) {
                    col = [r, g, b];
                }
            }
            "g" if i >= 1 => {
                if let Some(g) = num(i - 1) {
                    col = [g; 3];
                }
            }
            "Tf" if i >= 1 => {
                if let Some(s) = num(i - 1).filter(|s| *s > 0.0) {
                    size = s;
                }
            }
            _ => {}
        }
    }
    (col.map(|x| x.clamp(0.0, 1.0)), size.min(400.0))
}

fn ext_gstate(opacity: f64, multiply: bool) -> Option<Dict> {
    if opacity >= 1.0 && !multiply {
        return None;
    }
    let mut gs = Dict::new();
    gs.set(b"Type".to_vec(), Object::name("ExtGState"));
    if multiply {
        gs.set(b"BM".to_vec(), Object::name("Multiply"));
    }
    gs.set(b"CA".to_vec(), Object::Real(opacity));
    gs.set(b"ca".to_vec(), Object::Real(opacity));
    let mut res = Dict::new();
    res.set(b"GS0".to_vec(), Object::Dict(gs));
    Some(res)
}

fn form(bbox: [f64; 4], content: &[u8], resources: Dict) -> Stream {
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("XObject"));
    d.set(b"Subtype".to_vec(), Object::name("Form"));
    d.set(b"FormType".to_vec(), Object::Int(1));
    d.set(b"BBox".to_vec(), Object::Array(bbox.iter().map(|x| Object::Real(*x)).collect()));
    d.set(b"Resources".to_vec(), Object::Dict(resources));
    Stream::flate(d, content)
}

/// Draw the normal appearance of an annotation, or `None` if this subtype/variant isn't supported.
pub fn build(d: &Dict) -> Option<Stream> {
    let subtype = d.name(b"Subtype")?.to_vec();
    let rect = nums(d, b"Rect").filter(|r| r.len() == 4)?;
    let rect = [rect[0].min(rect[2]), rect[1].min(rect[3]), rect[0].max(rect[2]), rect[1].max(rect[3])];
    // Cloudy borders (§12.5.4) are not drawn yet.
    if d.get(b"BE").and_then(|b| b.as_dict()).is_some_and(|b| b.name(b"S") == Some(b"C")) {
        return None;
    }
    let opacity = d.get(b"CA").and_then(|o| o.as_f64()).unwrap_or(1.0).clamp(0.0, 1.0);
    let stroke = color(d, b"C")?;
    let w = border_width(d);
    let mut res = Dict::new();
    let mut c = String::new();
    let markup = matches!(subtype.as_slice(), b"Highlight" | b"Underline" | b"StrikeOut" | b"Squiggly");
    if let Some(gs) = ext_gstate(opacity, subtype == b"Highlight") {
        res.set(b"ExtGState".to_vec(), Object::Dict(gs));
        c.push_str("/GS0 gs\n");
    }
    match subtype.as_slice() {
        b"Text" => {
            let col = stroke.unwrap_or([1.0, 0.82, 0.0]);
            let icon = d.name(b"Name").map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_else(|| "Note".into());
            c.push_str(&note_icon(&icon, col));
            return Some(form([0.0, 0.0, NOTE_SIZE, NOTE_SIZE], c.as_bytes(), res));
        }
        _ if markup => {
            let q = nums(d, b"QuadPoints").filter(|q| !q.is_empty() && q.len() % 8 == 0)?;
            let col = stroke?;
            for quad in q.chunks_exact(8) {
                let p = |i: usize| (quad[2 * i], quad[2 * i + 1]);
                let (p1, p2, p3, p4) = (p(0), p(1), p(2), p(3));
                let h = (p1.0 - p3.0).hypot(p1.1 - p3.1);
                // A point a fraction `t` of the way from the bottom edge to the top edge.
                let at = |bottom: (f64, f64), top: (f64, f64), t: f64| (bottom.0 + (top.0 - bottom.0) * t, bottom.1 + (top.1 - bottom.1) * t);
                match subtype.as_slice() {
                    b"Highlight" => {
                        c.push_str(&rg(col));
                        c.push_str(&format!(
                            "{} {} m {} {} l {} {} l {} {} l h f\n",
                            n(p1.0),
                            n(p1.1),
                            n(p2.0),
                            n(p2.1),
                            n(p4.0),
                            n(p4.1),
                            n(p3.0),
                            n(p3.1)
                        ));
                    }
                    b"Underline" | b"StrikeOut" => {
                        let lw = (h * 0.07).clamp(0.5, 3.0);
                        let t = if subtype == b"Underline" { 0.08 } else { 0.42 };
                        let (a, b) = (at(p3, p1, t), at(p4, p2, t));
                        c.push_str(&format!("{}{} w\n{} {} m {} {} l S\n", rg_stroke(col), n(lw), n(a.0), n(a.1), n(b.0), n(b.1)));
                    }
                    _ => {
                        // Squiggly: a zigzag along the bottom of the quad.
                        let lw = (h * 0.05).clamp(0.5, 2.0);
                        let amp = (h * 0.06).max(0.75);
                        let len = (p4.0 - p3.0).hypot(p4.1 - p3.1);
                        let step = (h / 4.0).max(1.5);
                        let steps = ((len / step).ceil() as usize).clamp(1, 10_000);
                        c.push_str(&format!("{}{} w 1 j\n", rg_stroke(col), n(lw)));
                        for i in 0..=steps {
                            let t = i as f64 / steps as f64;
                            let base = (p3.0 + (p4.0 - p3.0) * t, p3.1 + (p4.1 - p3.1) * t);
                            let up = if i % 2 == 0 { amp * 2.0 } else { 0.0 };
                            let (ux, uy) = if h > 0.0 { ((p1.0 - p3.0) / h, (p1.1 - p3.1) / h) } else { (0.0, 1.0) };
                            let (x, y) = (base.0 + ux * up, base.1 + uy * up);
                            c.push_str(&format!("{} {} {}\n", n(x), n(y), if i == 0 { "m" } else { "l" }));
                        }
                        c.push_str("S\n");
                    }
                }
            }
        }
        b"Redact" => {
            // While marked: the outline of each area (the fill comes when applied).
            let q = nums(d, b"QuadPoints").filter(|q| !q.is_empty() && q.len() % 8 == 0)?;
            let col = stroke.unwrap_or([0.89, 0.13, 0.13]);
            c.push_str(&format!("{}1 w\n", rg_stroke(col)));
            for quad in q.chunks_exact(8) {
                c.push_str(&format!(
                    "{} {} m {} {} l {} {} l {} {} l h S\n",
                    n(quad[0]),
                    n(quad[1]),
                    n(quad[2]),
                    n(quad[3]),
                    n(quad[6]),
                    n(quad[7]),
                    n(quad[4]),
                    n(quad[5])
                ));
            }
        }
        b"Square" | b"Circle" => {
            let fill = color(d, b"IC")?;
            if stroke.is_none() && fill.is_none() {
                return Some(form(rect, c.as_bytes(), res));
            }
            let inset = w / 2.0;
            let [x0, y0, x1, y1] = [rect[0] + inset, rect[1] + inset, rect[2] - inset, rect[3] - inset];
            if let Some(f) = fill {
                c.push_str(&rg(f));
            }
            if let Some(s) = stroke {
                c.push_str(&format!("{}{} w\n{}", rg_stroke(s), n(w), dash(d)));
            }
            if subtype == b"Square" {
                c.push_str(&format!("{} {} {} {} re\n", n(x0), n(y0), n(x1 - x0), n(y1 - y0)));
            } else {
                c.push_str(&ellipse(x0, y0, x1, y1));
            }
            c.push_str(match (fill.is_some(), stroke.is_some() && w > 0.0) {
                (true, true) => "B\n",
                (true, false) => "f\n",
                (false, true) => "S\n",
                (false, false) => "n\n",
            });
        }
        b"Line" => {
            let l = nums(d, b"L").filter(|l| l.len() == 4)?;
            let col = stroke?;
            let ends: Vec<Vec<u8>> = match d.get(b"LE") {
                None => vec![b"None".to_vec(), b"None".to_vec()],
                Some(o) => o.as_array()?.iter().map(|e| e.as_name().map(<[u8]>::to_vec)).collect::<Option<_>>()?,
            };
            if ends.len() != 2 || ends.iter().any(|e| !matches!(e.as_slice(), b"None" | b"OpenArrow" | b"ClosedArrow")) {
                return None;
            }
            let fill = color(d, b"IC")?.unwrap_or(col);
            c.push_str(&format!("{}{}{} w 1 J 1 j\n{}", rg_stroke(col), rg(fill), n(w), dash(d)));
            c.push_str(&format!("{} {} m {} {} l S\n[] 0 d\n", n(l[0]), n(l[1]), n(l[2]), n(l[3])));
            for (end, (tip, from)) in ends.iter().zip([((l[0], l[1]), (l[2], l[3])), ((l[2], l[3]), (l[0], l[1]))]) {
                if end.as_slice() == b"None" {
                    continue;
                }
                let (dx, dy) = (from.0 - tip.0, from.1 - tip.1);
                let len = dx.hypot(dy);
                if len == 0.0 {
                    continue;
                }
                let (ux, uy) = (dx / len, dy / len);
                let s = arrow_size(w);
                let (cos, sin) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
                let a = (tip.0 + s * (ux * cos - uy * sin), tip.1 + s * (ux * sin + uy * cos));
                let b = (tip.0 + s * (ux * cos + uy * sin), tip.1 + s * (-ux * sin + uy * cos));
                let op = if end.as_slice() == b"ClosedArrow" { "h B" } else { "S" };
                c.push_str(&format!("{} {} m {} {} l {} {} l {op}\n", n(a.0), n(a.1), n(tip.0), n(tip.1), n(b.0), n(b.1)));
            }
        }
        b"Ink" => {
            let col = stroke?;
            let list = d.get(b"InkList")?.as_array()?;
            c.push_str(&format!("{}{} w 1 J 1 j\n", rg_stroke(col), n(w)));
            for s in list {
                let pts: Vec<f64> = s.as_array()?.iter().map(|o| o.as_f64()).collect::<Option<_>>()?;
                let pts: Vec<(f64, f64)> = pts.chunks_exact(2).map(|p| (p[0], p[1])).collect();
                let Some(first) = pts.first() else { continue };
                c.push_str(&format!("{} {} m\n", n(first.0), n(first.1)));
                if pts.len() == 1 {
                    c.push_str(&format!("{} {} l\n", n(first.0 + 0.01), n(first.1)));
                }
                for p in &pts[1..] {
                    c.push_str(&format!("{} {} l\n", n(p.0), n(p.1)));
                }
                c.push_str("S\n");
            }
        }
        b"Stamp" => {
            // Only PrintCraft's own Fill & Sign marks are drawn here.
            let name = d.name(b"Name")?;
            let col = stroke.unwrap_or([0.0; 3]);
            let [x0, y0, x1, y1] = rect;
            let (w, h) = (x1 - x0, y1 - y0);
            let lw = w.min(h) * 0.12;
            match name {
                b"PCCheck" => c.push_str(&format!(
                    "{}{} w 1 J 1 j\n{} {} m {} {} l {} {} l S\n",
                    rg_stroke(col),
                    n(lw),
                    n(x0 + w * 0.15),
                    n(y0 + h * 0.5),
                    n(x0 + w * 0.4),
                    n(y0 + h * 0.2),
                    n(x0 + w * 0.88),
                    n(y0 + h * 0.85)
                )),
                b"PCCross" => c.push_str(&format!(
                    "{}{} w 1 J\n{} {} m {} {} l {} {} m {} {} l S\n",
                    rg_stroke(col),
                    n(lw),
                    n(x0 + w * 0.18),
                    n(y0 + h * 0.18),
                    n(x1 - w * 0.18),
                    n(y1 - h * 0.18),
                    n(x0 + w * 0.18),
                    n(y1 - h * 0.18),
                    n(x1 - w * 0.18),
                    n(y0 + h * 0.18)
                )),
                b"PCDot" => {
                    let r = w.min(h) * 0.3;
                    c.push_str(&rg(col));
                    c.push_str(&ellipse(x0 + w / 2.0 - r, y0 + h / 2.0 - r, x0 + w / 2.0 + r, y0 + h / 2.0 + r));
                    c.push_str("f\n");
                }
                b"PCLine" => c.push_str(&format!(
                    "{}{} w 1 J\n{} {} m {} {} l S\n",
                    rg_stroke(col),
                    n(h.clamp(0.5, 2.0)),
                    n(x0),
                    n(y0 + h / 2.0),
                    n(x1),
                    n(y0 + h / 2.0)
                )),
                _ => return None,
            }
        }
        b"FreeText" => {
            // Callouts (`/CL`) and rich text without plain contents aren't drawn yet.
            if d.contains(b"CL") {
                return None;
            }
            let (text_color, size) = parse_da(d);
            let bg = stroke;
            let bw = if d.contains(b"BS") || d.contains(b"Border") { border_width(d) } else { 0.0 };
            if let Some(bg) = bg {
                c.push_str(&format!("{}{} {} {} {} re f\n", rg(bg), n(rect[0]), n(rect[1]), n(rect[2] - rect[0]), n(rect[3] - rect[1])));
            }
            if bw > 0.0 {
                let h = bw / 2.0;
                c.push_str(&format!(
                    "{}{} w\n{}{} {} {} {} re S\n[] 0 d\n",
                    rg_stroke(text_color),
                    n(bw),
                    dash(d),
                    n(rect[0] + h),
                    n(rect[1] + h),
                    n(rect[2] - rect[0] - bw),
                    n(rect[3] - rect[1] - bw)
                ));
            }
            let text = d.get(b"Contents").and_then(|o| o.as_string()).map(PdfString::to_text).unwrap_or_default();
            let pad = 2.0 + bw;
            let width = (rect[2] - rect[0] - 2.0 * pad).max(1.0);
            let q = d.int(b"Q").unwrap_or(0);
            c.push_str(&format!(
                "{} {} {} {} re W n\nBT\n/Helv {} Tf\n{}",
                n(rect[0]),
                n(rect[1]),
                n(rect[2] - rect[0]),
                n(rect[3] - rect[1]),
                n(size),
                rg(text_color)
            ));
            let mut out = c.into_bytes();
            let mut y = rect[3] - pad - size * 0.9;
            for line in wrap(&text, size, width) {
                if y < rect[1] - size {
                    break;
                }
                let lw = text_width(&line, size);
                let x = match q {
                    1 => rect[0] + pad + (width - lw) / 2.0,
                    2 => rect[2] - pad - lw,
                    _ => rect[0] + pad,
                };
                out.extend(format!("1 0 0 1 {} {} Tm ", n(x), n(y)).bytes());
                out.extend(literal(&win_ansi(&line)));
                out.extend_from_slice(b" Tj\n");
                y -= size * 1.2;
            }
            out.extend_from_slice(b"ET\n");
            let mut font = Dict::new();
            font.set(b"Type".to_vec(), Object::name("Font"));
            font.set(b"Subtype".to_vec(), Object::name("Type1"));
            font.set(b"BaseFont".to_vec(), Object::name("Helvetica"));
            font.set(b"Encoding".to_vec(), Object::name("WinAnsiEncoding"));
            let mut fonts = Dict::new();
            fonts.set(b"Helv".to_vec(), Object::Dict(font));
            res.set(b"Font".to_vec(), Object::Dict(fonts));
            return Some(form(rect, &out, res));
        }
        _ => return None,
    }
    Some(form(rect, c.as_bytes(), res))
}

/// An ellipse inscribed in a rectangle, as four Bézier arcs.
fn ellipse(x0: f64, y0: f64, x1: f64, y1: f64) -> String {
    let k = 0.552_284_75;
    let (cx, cy, rx, ry) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0) / 2.0, (y1 - y0) / 2.0);
    let (ox, oy) = (rx * k, ry * k);
    let mut s = format!("{} {} m\n", n(cx + rx), n(cy));
    for [a, b, c, d, e, f] in [
        [cx + rx, cy + oy, cx + ox, cy + ry, cx, cy + ry],
        [cx - ox, cy + ry, cx - rx, cy + oy, cx - rx, cy],
        [cx - rx, cy - oy, cx - ox, cy - ry, cx, cy - ry],
        [cx + ox, cy - ry, cx + rx, cy - oy, cx + rx, cy],
    ] {
        s.push_str(&format!("{} {} {} {} {} {} c\n", n(a), n(b), n(c), n(d), n(e), n(f)));
    }
    s.push_str("h\n");
    s
}

/// PrintCraft's note icons, drawn in a 20 × 20 box: a speech bubble for `/Comment`, a page
/// with a folded corner for everything else, both filled with the note colour.
fn note_icon(name: &str, col: Rgb) -> String {
    let mut s = format!("{}0.25 0.25 0.25 RG 0.8 w 1 j 1 J\n", rg(col));
    if name == "Comment" {
        s.push_str("3 18.5 m 17 18.5 l 18.5 18.5 18.5 17 18.5 17 c 18.5 8 l 18.5 6.5 17 6.5 17 6.5 c 9.5 6.5 l 5 2 l 5.5 6.5 l 3 6.5 l 1.5 6.5 1.5 8 1.5 8 c 1.5 17 l 1.5 18.5 3 18.5 3 18.5 c h B\n");
        s.push_str("4.5 15 m 15.5 15 l 4.5 12.5 m 15.5 12.5 l 4.5 10 m 11.5 10 l S\n");
    } else {
        s.push_str("3 19 m 13 19 l 17 15 l 17 1 l 3 1 l h B\n13 19 m 13 15 l 17 15 l S\n");
        s.push_str("5.5 12 m 14.5 12 l 5.5 9 m 14.5 9 l 5.5 6 m 11.5 6 l S\n");
    }
    s
}

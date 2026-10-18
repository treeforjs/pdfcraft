//! The Object Inspector's hook into the ink rasterizer: while a page is separated, test each
//! painted object against one point and describe the ones that cover it (see
//! [`crate::inspector`]).

use std::collections::VecDeque;

use pdfcraft_content::Matrix;
use pdfcraft_cos::{Dict, Document, Object};

use super::{Font, Gs, P, Paint, Space, Target, Walker, family};
use crate::inspector::{
    FormInfo, ImageInfo, Inspected, MAX_OBJECTS, ObjectColour, ObjectKind, Overprint, ShadingInfo, TOLERANCE, TextInfo, Transparency,
};

/// Colorant names listed in a DeviceN label, at most.
const MAX_LABEL_NAMES: usize = 8;
/// Colour space nesting followed for labels.
const MAX_LABEL_DEPTH: usize = 4;
/// Filters listed per image, at most.
const MAX_FILTERS: usize = 8;

/// The point being inspected and the objects found there, bottom first.
pub(crate) struct Probe {
    user: (f64, f64),
    /// The point in grid coordinates.
    at: (f64, f64),
    /// Grid coordinates back to user space.
    from_grid: Matrix,
    /// [`TOLERANCE`] in grid units.
    tolerance: f64,
    /// Points to grid units.
    per_point: f64,
    /// The form XObjects being run, outermost first.
    pub(super) forms: Vec<String>,
    /// The objects found, in painting order (bottom first), the last [`MAX_OBJECTS`].
    pub(crate) objects: VecDeque<Inspected>,
    pub(crate) truncated: bool,
    order: usize,
}

impl Probe {
    pub(crate) fn new(user: (f64, f64)) -> Self {
        Self {
            user,
            at: user,
            from_grid: Matrix::IDENTITY,
            tolerance: TOLERANCE,
            per_point: 1.0,
            forms: Vec::new(),
            objects: VecDeque::new(),
            truncated: false,
            order: 0,
        }
    }

    /// Place the point on the grid (`to_grid` maps user space to cells of `cell` points).
    pub(super) fn place(&mut self, to_grid: &Matrix, cell: f64) {
        self.at = to_grid.apply(self.user.0, self.user.1);
        self.from_grid = to_grid.invert().unwrap_or(Matrix::IDENTITY);
        self.per_point = if cell.is_finite() && cell > 0.0 { 1.0 / cell } else { 1.0 };
        self.tolerance = TOLERANCE * self.per_point;
    }

    /// The next object's place in the painting order.
    fn next(&mut self) -> usize {
        self.order = self.order.saturating_add(1);
        self.order
    }

    fn record(&mut self, mut object: Inspected) {
        object.forms = self.forms.clone();
        self.objects.push_back(object);
        if self.objects.len() > MAX_OBJECTS {
            self.objects.pop_front();
            self.truncated = true;
        }
    }

    /// A grid-space box in user space, rounded.
    fn user_box(&self, b: [f64; 4]) -> [f64; 4] {
        self.from_grid.bbox(b).map(round2)
    }
}

/// Transparency and rendering intent in the graphics state.
#[derive(Clone, Debug)]
pub(super) struct Look {
    pub(super) transparency: Transparency,
    pub(super) intent: String,
    /// The `Tf` font resource name.
    pub(super) font_resource: String,
}

impl Default for Look {
    fn default() -> Self {
        Self { transparency: Transparency::default(), intent: "RelativeColorimetric".into(), font_resource: String::new() }
    }
}

fn round2(v: f64) -> f64 {
    if v.is_finite() { (v * 100.0).round() / 100.0 } else { 0.0 }
}

fn round4(v: f64) -> f64 {
    if v.is_finite() { (v * 10_000.0).round() / 10_000.0 } else { 0.0 }
}

fn lossy(n: &[u8]) -> String {
    String::from_utf8_lossy(n).chars().take(256).collect()
}

/// Read the transparency and rendering intent entries of ExtGState `d` into `look`.
pub(super) fn apply_ext_gstate(doc: &Document, d: &Dict, look: &mut Look) {
    let num = |k: &[u8]| d.get(k).and_then(|v| doc.resolve(v).as_f64()).filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0));
    if let Some(a) = num(b"CA") {
        look.transparency.stroke_alpha = round4(a);
    }
    if let Some(a) = num(b"ca") {
        look.transparency.fill_alpha = round4(a);
    }
    if let Some(bm) = d.get(b"BM").map(|b| doc.resolve(b)) {
        let name = match bm.as_ref() {
            Object::Name(n) => Some(lossy(n)),
            Object::Array(a) => a.first().and_then(Object::as_name).map(lossy),
            _ => None,
        };
        if let Some(name) = name {
            look.transparency.blend_mode = name;
        }
    }
    if let Some(mask) = d.get(b"SMask").map(|m| doc.resolve(m)) {
        look.transparency.soft_mask = match mask.as_ref() {
            Object::Name(n) if n.as_slice() == b"None" => None,
            Object::Dict(m) => Some(m.name(b"S").map_or_else(|| "soft mask".to_string(), lossy)),
            _ => Some("soft mask".into()),
        };
    }
    if let Some(Object::Bool(b)) = d.get(b"AIS").map(|v| doc.resolve(v)).as_deref() {
        look.transparency.alpha_is_shape = *b;
    }
    if let Some(ri) = d.get(b"RI").map(|v| doc.resolve(v)).as_deref().and_then(Object::as_name) {
        look.intent = lossy(ri);
    }
}

/// A colour space as the Object Inspector names it: `DeviceCMYK`, `ICCBased (4)`,
/// `Separation (Gold)`, `DeviceN (Cyan, Teal)`, `Indexed (DeviceRGB)`, `Pattern` …
pub(super) fn space_label(doc: &Document, o: &Object, resources: &Dict, depth: usize) -> String {
    if depth > MAX_LABEL_DEPTH {
        return "unknown".into();
    }
    let o = doc.resolve(o);
    match o.as_ref() {
        Object::Name(n) => match n.as_slice() {
            b"DeviceGray" | b"G" => "DeviceGray".into(),
            b"DeviceRGB" | b"RGB" => "DeviceRGB".into(),
            b"DeviceCMYK" | b"CMYK" => "DeviceCMYK".into(),
            b"Pattern" => "Pattern".into(),
            other => {
                let named = resources.get(b"ColorSpace").map(|d| doc.resolve(d)).and_then(|d| d.as_dict().and_then(|d| d.get(other).cloned()));
                match named {
                    Some(v) => space_label(doc, &v, &Dict::new(), depth + 1),
                    None => lossy(other),
                }
            }
        },
        Object::Array(a) => {
            let name = a.first().map(|f| doc.resolve(f)).and_then(|f| f.as_name().map(<[u8]>::to_vec)).unwrap_or_default();
            let arg = |i: usize| a.get(i).map(|o| doc.resolve(o));
            match name.as_slice() {
                b"Separation" => {
                    let colorant = arg(1).and_then(|n| n.as_name().map(lossy)).unwrap_or_default();
                    format!("Separation ({colorant})")
                }
                b"DeviceN" => {
                    let names: Vec<String> = arg(1)
                        .and_then(|n| {
                            n.as_array()
                                .map(|n| n.iter().take(MAX_LABEL_NAMES).map(|v| doc.resolve(v).as_name().map(lossy).unwrap_or_default()).collect())
                        })
                        .unwrap_or_default();
                    let more = arg(1).and_then(|n| n.as_array().map(Vec::len)).unwrap_or(0) > MAX_LABEL_NAMES;
                    format!("DeviceN ({}{})", names.join(", "), if more { ", …" } else { "" })
                }
                b"Indexed" | b"I" => {
                    let base = a.get(1).map_or_else(|| "unknown".into(), |b| space_label(doc, b, resources, depth + 1));
                    format!("Indexed ({base})")
                }
                b"Pattern" => match a.get(1) {
                    Some(under) => format!("Pattern ({})", space_label(doc, under, resources, depth + 1)),
                    None => "Pattern".into(),
                },
                b"ICCBased" | b"CalGray" | b"CalRGB" | b"Lab" | b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK" => family(doc, &o).0,
                _ if name.is_empty() => "unknown".into(),
                other => lossy(other),
            }
        }
        _ => "unknown".into(),
    }
}

/// A colour space's name when it wasn't set with `cs` (`g`, `rg`, `k` and the defaults).
fn default_label(space: &Space) -> String {
    match space {
        Space::Gray => "DeviceGray",
        Space::Rgb => "DeviceRGB",
        Space::Cmyk { device: true } => "DeviceCMYK",
        Space::Cmyk { device: false } => "ICCBased (4)",
        Space::Lab => "Lab",
        Space::Separation(_) => "Separation",
        Space::DeviceN(_) => "DeviceN",
        Space::Indexed { .. } => "Indexed",
        Space::Pattern => "Pattern",
        Space::Unknown(_) => "unknown",
    }
    .into()
}

fn shading_kind(t: i64) -> &'static str {
    match t {
        1 => "function-based",
        2 => "axial",
        3 => "radial",
        4 => "free-form triangle mesh",
        5 => "lattice-form triangle mesh",
        6 => "Coons patch mesh",
        7 => "tensor-product patch mesh",
        _ => "unknown",
    }
}

/// Whether grid point `p` is inside clip box `c`.
fn in_box(c: [f64; 4], p: P) -> bool {
    p.0 >= c[0] && p.0 <= c[2] && p.1 >= c[1] && p.1 <= c[3]
}

/// Whether `p` is inside the (implicitly closed) polygons `path`.
fn inside(path: &[Vec<P>], even_odd: bool, p: P) -> bool {
    let mut winding = 0i64;
    let mut crossings = 0u64;
    for sp in path {
        let n = sp.len();
        if n < 3 {
            continue;
        }
        for i in 0..n {
            let (Some(&a), Some(&b)) = (sp.get(i), sp.get((i + 1) % n)) else { continue };
            if (a.1 <= p.1) == (b.1 <= p.1) {
                continue;
            }
            let t = (p.1 - a.1) / (b.1 - a.1);
            let x = a.0 + t * (b.0 - a.0);
            if x.is_finite() && x > p.0 {
                crossings = crossings.saturating_add(1);
                winding = winding.saturating_add(if b.1 > a.1 { 1 } else { -1 });
            }
        }
    }
    if even_odd { crossings % 2 == 1 } else { winding != 0 }
}

/// The distance from `p` to segment `a`–`b`.
fn distance(p: P, a: P, b: P) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 1e-18 && len2.is_finite() { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    let (x, y) = (a.0 + t * dx, a.1 + t * dy);
    ((p.0 - x).powi(2) + (p.1 - y).powi(2)).sqrt()
}

/// Whether `p` is within `half` of the stroke of `path`.
fn on_stroke(path: &[Vec<P>], closed: &[bool], p: P, half: f64) -> bool {
    for (i, sp) in path.iter().enumerate() {
        let n = sp.len();
        if n == 1 && sp.first().is_some_and(|&a| distance(p, a, a) <= half) {
            return true;
        }
        let segments = if closed.get(i).copied().unwrap_or(false) && n > 2 { n } else { n.saturating_sub(1) };
        for s in 0..segments {
            let (Some(&a), Some(&b)) = (sp.get(s), sp.get((s + 1) % n)) else { continue };
            if distance(p, a, b) <= half {
                return true;
            }
        }
    }
    false
}

fn bounds(points: impl Iterator<Item = P>) -> [f64; 4] {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for (x, y) in points {
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    }
    if b.iter().all(|v| v.is_finite()) && b[0] <= b[2] { b } else { [0.0; 4] }
}

/// How much a matrix scales lengths (the square root of its determinant).
fn scale(m: &Matrix) -> f64 {
    let [a, b, c, d, ..] = m.0;
    let s = (a * d - b * c).abs().sqrt();
    if s.is_finite() { s } else { 0.0 }
}

fn blank(kind: ObjectKind, order: usize, bbox: [f64; 4], gs: &Gs) -> Inspected {
    Inspected {
        kind,
        order,
        bbox,
        painted: Vec::new(),
        fill: None,
        stroke: None,
        overprint: Overprint { fill: gs.op_fill, stroke: gs.op_stroke, mode: gs.opm },
        transparency: gs.look.transparency.clone(),
        rendering_intent: gs.look.intent.clone(),
        line_width: None,
        fill_rule: None,
        text: None,
        image: None,
        shading: None,
        form: None,
        annotation: None,
        forms: Vec::new(),
    }
}

impl Walker<'_> {
    /// The point in grid coordinates and the next painting-order number, when probing.
    fn probe_next(&mut self) -> Option<(P, usize)> {
        let p = self.probe.as_mut()?;
        Some((p.at, p.next()))
    }

    fn probe_record(&mut self, object: Inspected) {
        if let Some(p) = self.probe.as_mut() {
            p.record(object);
        }
    }

    fn probe_box(&self, b: [f64; 4]) -> [f64; 4] {
        self.probe.as_ref().map_or([0.0; 4], |p| p.user_box(b))
    }

    /// Describe colour `paint` (patterns looked up in `resources`).
    fn colour_info(&self, paint: &Paint, resources: Option<&Dict>) -> ObjectColour {
        let mut values = Vec::new();
        let kind = if paint.pattern { None } else { paint.space.ink_into(&paint.comps, self.conversion, &self.spots, &mut values) };
        let inks: Vec<(String, f64)> = if kind.is_some() {
            values
                .iter()
                .filter_map(|&(t, v)| {
                    let name = match t {
                        Target::Plate(i) => self.inks.get(i).map(|i| i.name.clone())?,
                        Target::All => "All".into(),
                        Target::Untracked => "(untracked ink)".into(),
                        Target::Nothing | Target::Spot(_) => return None,
                    };
                    Some((name, round2(v * 100.0)))
                })
                .collect()
        } else {
            Vec::new()
        };
        let pattern = paint.pattern_name.as_ref().map(|name| {
            let entry = resources
                .and_then(|r| r.get(b"Pattern"))
                .map(|d| self.doc.resolve(d))
                .and_then(|d| d.as_dict().and_then(|d| d.get(name).cloned()))
                .map(|e| self.doc.resolve(&e));
            let what = entry.as_deref().and_then(Object::as_dict).map(|d| match d.get(b"PatternType").and_then(|t| self.doc.resolve(t).as_int()) {
                Some(1) => {
                    let uncoloured = d.get(b"PaintType").and_then(|t| self.doc.resolve(t).as_int()) == Some(2);
                    if uncoloured { "uncoloured tiling pattern".to_string() } else { "tiling pattern".to_string() }
                }
                Some(2) => {
                    let shading = d.get(b"Shading").map(|s| self.doc.resolve(s));
                    let sd = shading.as_deref().and_then(Object::as_dict);
                    let t = sd.and_then(|s| s.get(b"ShadingType")).and_then(|t| self.doc.resolve(t).as_int()).unwrap_or(0);
                    let cs = sd
                        .and_then(|s| s.get(b"ColorSpace"))
                        .map_or_else(|| "unknown".into(), |c| space_label(self.doc, c, resources.unwrap_or(&Dict::new()), 0));
                    format!("shading pattern, {}, {cs}", shading_kind(t))
                }
                _ => "unknown pattern".to_string(),
            });
            match what {
                Some(w) => format!("{}: {w}", lossy(name)),
                None => lossy(name),
            }
        });
        ObjectColour {
            space: paint.label.as_deref().map_or_else(|| default_label(&paint.space), str::to_string),
            components: paint.comps.iter().map(|v| round4(*v)).collect(),
            inks,
            converted: kind.is_some_and(|k| k.converted),
            measured: kind.is_some(),
            pattern,
        }
    }

    /// A path painted with operator `op`.
    pub(super) fn probe_path(&mut self, path: &[Vec<P>], closed: &[bool], op: &[u8], gs: &Gs, resources: &Dict) {
        let Some((at, order)) = self.probe_next() else { return };
        if !in_box(gs.clip, at) {
            return;
        }
        let fills = matches!(op, b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*");
        let strokes = matches!(op, b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*");
        let even_odd = matches!(op, b"f*" | b"B*" | b"b*");
        let tolerance = self.probe.as_ref().map_or(TOLERANCE, |p| p.tolerance);
        let width = gs.line_width * scale(&gs.ctm);
        let half = (width / self.cell / 2.0).max(tolerance);
        let hit_fill = fills && inside(path, even_odd, at);
        let hit_stroke = strokes && on_stroke(path, closed, at, half);
        if !hit_fill && !hit_stroke {
            return;
        }
        let mut b = bounds(path.iter().flatten().copied());
        if strokes {
            let h = width / self.cell / 2.0;
            b = [b[0] - h, b[1] - h, b[2] + h, b[3] + h];
        }
        let mut o = blank(ObjectKind::Path, order, self.probe_box(b), gs);
        if hit_fill {
            o.painted.push("fill");
        }
        if hit_stroke {
            o.painted.push("stroke");
        }
        if fills {
            o.fill = Some(self.colour_info(&gs.fill, Some(resources)));
            o.fill_rule = Some(if even_odd { "even-odd" } else { "nonzero" });
        }
        if strokes {
            o.stroke = Some(self.colour_info(&gs.stroke, Some(resources)));
            o.line_width = Some(round2(width));
        }
        self.probe_record(o);
    }

    /// A shown string whose box is `quad` (grid coordinates); `trm` maps text space to user space.
    pub(super) fn probe_text(&mut self, quad: &[P], gs: &Gs, font: &Font, trm: Matrix, paints: (bool, bool), resources: &Dict) {
        let Some((at, order)) = self.probe_next() else { return };
        if !in_box(gs.clip, at) || !inside(&[quad.to_vec()], false, at) {
            return;
        }
        let mut o = blank(ObjectKind::Text, order, self.probe_box(bounds(quad.iter().copied())), gs);
        if paints.0 {
            o.painted.push("fill");
            o.fill = Some(self.colour_info(&gs.fill, Some(resources)));
        }
        if paints.1 {
            o.painted.push("stroke");
            o.stroke = Some(self.colour_info(&gs.stroke, Some(resources)));
            o.line_width = Some(round2(gs.line_width * scale(&gs.ctm)));
        }
        let [_, _, c, d, ..] = trm.0;
        let vertical = (c * c + d * d).sqrt();
        let name = font.base_font.clone();
        let subset = name.len() > 7 && name.as_bytes().get(6) == Some(&b'+') && name.bytes().take(6).all(|b| b.is_ascii_uppercase());
        o.text = Some(TextInfo {
            subset,
            font: name,
            resource: gs.look.font_resource.clone(),
            font_type: font.subtype.clone(),
            embedded: font.embedded,
            size: round2(gs.size),
            effective_size: round2(gs.size.abs() * if vertical.is_finite() { vertical } else { 0.0 }),
            render_mode: gs.render.rem_euclid(8),
            invisible: !paints.0 && !paints.1,
        });
        self.probe_record(o);
    }

    /// An image XObject (`name`) or inline image (`None`) with dictionary `dict`.
    pub(super) fn probe_image(&mut self, name: Option<&[u8]>, dict: &Dict, resources: &Dict, gs: &Gs) {
        let Some((at, order)) = self.probe_next() else { return };
        let device = gs.ctm.then(&self.to_grid);
        let Some(inverse) = device.invert() else { return };
        let (u, v) = inverse.apply(at.0, at.1);
        if !in_box(gs.clip, at) || !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return;
        }
        let doc = self.doc;
        let get = |k: &[u8]| dict.get(k).map(|o| doc.resolve(o));
        let int = |k: &[u8]| get(k).and_then(|o| o.as_int());
        let flag = |k: &[u8]| matches!(get(k).as_deref(), Some(Object::Bool(true)));
        let stencil = flag(b"ImageMask");
        let filters: Vec<String> = match get(b"Filter").as_deref() {
            Some(Object::Name(n)) => vec![lossy(n)],
            Some(Object::Array(a)) => a.iter().take(MAX_FILTERS).filter_map(|f| doc.resolve(f).as_name().map(lossy)).collect(),
            _ => Vec::new(),
        };
        let colour_space = if stencil {
            "stencil mask".to_string()
        } else {
            match dict.get(b"ColorSpace") {
                Some(cs) => space_label(doc, cs, resources, 0),
                None if filters.iter().any(|f| f == "JPXDecode") => "from the JPX data".into(),
                None => "missing".into(),
            }
        };
        let (width, height) = (int(b"Width").unwrap_or(0), int(b"Height").unwrap_or(0));
        let [a, b, c, d, ..] = gs.ctm.0;
        let ppi = |pixels: i64, x: f64, y: f64| {
            let len = (x * x + y * y).sqrt();
            if len > 1e-9 && len.is_finite() { (pixels.max(0) as f64 * 72.0 / len * 10.0).round() / 10.0 } else { 0.0 }
        };
        let mask = match get(b"Mask").as_deref() {
            Some(Object::Array(_)) => Some("colour key".to_string()),
            Some(Object::Stream(_)) => Some("stencil".to_string()),
            _ => None,
        };
        let mut o = blank(ObjectKind::Image, order, self.probe_box(device.bbox([0.0, 0.0, 1.0, 1.0])), gs);
        o.painted.push("fill");
        if stencil {
            o.fill = Some(self.colour_info(&gs.fill, Some(resources)));
        }
        if o.transparency.soft_mask.is_none() {
            if matches!(get(b"SMask").as_deref(), Some(Object::Stream(_))) {
                o.transparency.soft_mask = Some("image".into());
            } else if int(b"SMaskInData").is_some_and(|v| v > 0) {
                o.transparency.soft_mask = Some("JPX data".into());
            }
        }
        let intent = get(b"Intent").and_then(|i| i.as_name().map(lossy));
        if let Some(i) = &intent {
            o.rendering_intent = i.clone();
        }
        o.image = Some(ImageInfo {
            name: name.map(lossy),
            width,
            height,
            bits_per_component: if stencil { 1 } else { int(b"BitsPerComponent").unwrap_or(0) },
            colour_space,
            stencil,
            filters,
            ppi: [ppi(width, a, b), ppi(height, c, d)],
            mask,
            interpolate: flag(b"Interpolate"),
            intent,
        });
        self.probe_record(o);
    }

    /// A shading painted with `sh` (it fills the clip, within its own `BBox`).
    pub(super) fn probe_shading(&mut self, name: &[u8], resources: &Dict, gs: &Gs) {
        let Some((at, order)) = self.probe_next() else { return };
        if !in_box(gs.clip, at) {
            return;
        }
        let doc = self.doc;
        let entry = resources.get(b"Shading").map(|d| doc.resolve(d)).and_then(|d| d.as_dict().and_then(|d| d.get(name).cloned()));
        let shading = entry.map(|e| doc.resolve(&e));
        let sd = shading.as_deref().and_then(Object::as_dict);
        let bbox = sd.and_then(|s| s.get(b"BBox")).map(|b| doc.resolve(b)).and_then(|b| {
            let v: Vec<f64> = b.as_array()?.iter().filter_map(Object::as_f64).filter(|v| v.is_finite()).collect();
            match v.as_slice() {
                [a, b, c, d] => Some([a.min(*c), b.min(*d), a.max(*c), b.max(*d)]),
                _ => None,
            }
        });
        let device = gs.ctm.then(&self.to_grid);
        if let Some(bb) = bbox {
            let Some(inverse) = device.invert() else { return };
            let (x, y) = inverse.apply(at.0, at.1);
            if !in_box(bb, (x, y)) {
                return;
            }
        }
        let area = match bbox {
            Some(bb) => super::intersect(gs.clip, device.bbox(bb)),
            None => gs.clip,
        };
        let shading_type = sd.and_then(|s| s.get(b"ShadingType")).and_then(|t| doc.resolve(t).as_int());
        let colour_space = sd.and_then(|s| s.get(b"ColorSpace")).map_or_else(|| "missing".into(), |c| space_label(doc, c, resources, 0));
        let mut o = blank(ObjectKind::Shading, order, self.probe_box(area), gs);
        o.painted.push("fill");
        o.shading = Some(ShadingInfo { name: lossy(name), shading_type, description: shading_kind(shading_type.unwrap_or(0)).into(), colour_space });
        self.probe_record(o);
    }

    /// A form XObject about to run with `inner` (its matrix and clip applied; `bbox` in form
    /// space). Its name joins the form chain (pop it after the run); it is listed when its box
    /// holds the point.
    pub(super) fn probe_form(&mut self, name: &[u8], dict: &Dict, inner: &Gs, bbox: Option<[f64; 4]>) -> bool {
        let Some((at, order)) = self.probe_next() else { return false };
        let device = inner.ctm.then(&self.to_grid);
        let in_bbox = match bbox {
            Some(bb) => device.invert().is_some_and(|inv| in_box(bb, inv.apply(at.0, at.1))),
            None => true,
        };
        if in_box(inner.clip, at) && in_bbox {
            let doc = self.doc;
            let group = dict.get(b"Group").map(|g| doc.resolve(g)).and_then(|g| {
                let g = g.as_dict()?;
                let flag = |k: &[u8]| matches!(g.get(k).map(|v| doc.resolve(v)).as_deref(), Some(Object::Bool(true)));
                let mut parts = vec![g.name(b"S").map_or_else(|| "group".to_string(), |s| format!("{} group", lossy(s)))];
                if flag(b"I") {
                    parts.push("isolated".into());
                }
                if flag(b"K") {
                    parts.push("knockout".into());
                }
                if let Some(cs) = g.get(b"CS") {
                    parts.push(space_label(doc, cs, &Dict::new(), 0));
                }
                Some(parts.join(", "))
            });
            let area = match bbox {
                Some(bb) => super::intersect(inner.clip, device.bbox(bb)),
                None => inner.clip,
            };
            let mut o = blank(ObjectKind::Form, order, self.probe_box(area), inner);
            o.form = Some(FormInfo { name: lossy(name), group });
            self.probe_record(o);
        }
        if let Some(p) = self.probe.as_mut() {
            p.forms.push(lossy(name));
            return true;
        }
        false
    }
}

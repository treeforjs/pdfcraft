//! Flatten transparency (Acrobat: Print Production ▸ Flattener Preview, PDF Optimizer ▸
//! Transparency): give pages that use transparency an opaque equivalent.
//!
//! What counts as transparency is exactly what the PDF/X transparency rule
//! ([`crate::verify_pdfx`]) looks for: soft masks, constant alpha (`CA`/`ca`) below 1, blend
//! modes other than Normal, transparency groups, and images with soft masks. It is found through
//! the page's resources: graphics states, form XObjects (and their own resources), images,
//! tiling and shading patterns and Type 3 fonts, with seen-sets and a depth limit.
//!
//! How a page is flattened:
//!
//! 1. Its resources are replaced by copies without transparency (soft masks set to `/None`,
//!    alpha to 1, blend modes to Normal, transparency groups and image soft masks removed) and
//!    its `/Group` is removed. Shared objects are copied, never changed, so pages that are not
//!    flattened keep them.
//! 2. The page is rendered before and after that change (at 72 pixels per user unit). Where the
//!    two differ is where transparency changed the look: those pixels are gathered into
//!    rectangular regions (8-pixel cells, 8-connected, 2-pixel margin), and nearby regions are
//!    merged (the lower the raster/vector balance, the farther apart they may be).
//! 3. Each region is rendered from the original page at the chosen resolution and drawn on top
//!    of the page content as an opaque DeviceRGB image (tiles of at most 2048 pixels). Outside
//!    the regions every object stays as it was: vectors, text, images. Inside them the objects
//!    stay too, hidden under the image (text there is still searchable and selectable).
//! 4. With a balance of 0, or when the regions cover more of the page than the balance allows
//!    (more than `balance`% of it), or when the content can't be extended safely, the whole page
//!    becomes one image (in tiles) and its content is replaced; its text is then gone.
//!
//! Annotations are not flattened (flatten them into the page first), optional content is
//! rendered as it is shown by default, and the images are DeviceRGB (a PDF/X-1a file still needs
//! its colour converted to CMYK). Regions are rectangles, not the outlines of the transparent
//! objects, so some opaque artwork next to them is rasterized too.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};
use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest, RenderedPage, Tile};

use crate::pdfx::{gstate_alpha, gstate_blend, gstate_soft_mask, image_soft_mask, image_soft_mask_in_data, is_transparency_group};

/// The raster resolutions accepted, in pixels per inch.
pub const MIN_RESOLUTION: f64 = 36.0;
pub const MAX_RESOLUTION: f64 = 1200.0;
/// The most pixels the images of one page may have; above it the resolution is lowered.
pub const MAX_PAGE_PIXELS: f64 = 64.0e6;
/// Pixels per user unit of the before/after comparison.
const DIFF_SCALE: f64 = 1.0;
/// The longest side of the comparison raster.
const MAX_DIFF_SIDE: f64 = 4096.0;
/// The largest side of one image.
const TILE: u32 = 2048;
/// Comparison cells, in pixels.
const CELL: usize = 8;
/// Pixels added round each region.
const MARGIN: usize = 2;
/// Regions beyond this many are merged into one.
const MAX_REGIONS: usize = 256;
/// Nesting of resources, forms, patterns and Type 3 fonts.
const MAX_DEPTH: usize = 32;
/// Resource entries looked at per dictionary.
const MAX_ENTRIES: usize = 10_000;
/// Objects looked at per page before giving up and rasterizing it whole.
const MAX_VISITS: usize = 200_000;
/// Content decoded per run to find the resources used (beyond it, every resource counts).
const MAX_SCANNED: usize = 256 << 20;
/// Decoded page content looked at to extend it.
const MAX_CONTENT: usize = 64 << 20;
/// Graphics-state nesting tolerated in content that is extended.
const MAX_NESTING: i64 = 4_096;

/// Acrobat's flattener presets, loosely: they choose the raster resolution.
///
/// One image holds whatever a region contains (text, line art, gradients), so there is one
/// resolution rather than separate line-art and gradient resolutions: 150 ppi for screen and
/// office printing (the PDF Optimizer's downsampling target), 300 ppi (the usual resolution of
/// images for commercial print) and 600 ppi, sharp enough for small text and fine lines while
/// staying inside [`MAX_PAGE_PIXELS`] for most regions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FlattenPreset {
    Low,
    #[default]
    Medium,
    High,
}

impl FlattenPreset {
    pub const ALL: [FlattenPreset; 3] = [FlattenPreset::Low, FlattenPreset::Medium, FlattenPreset::High];

    pub fn id(self) -> &'static str {
        match self {
            FlattenPreset::Low => "low",
            FlattenPreset::Medium => "medium",
            FlattenPreset::High => "high",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }

    /// Pixels per inch.
    pub fn resolution(self) -> f64 {
        match self {
            FlattenPreset::Low => 150.0,
            FlattenPreset::Medium => 300.0,
            FlattenPreset::High => 600.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlattenSettings {
    /// Raster resolution in pixels per inch ([`MIN_RESOLUTION`]..=[`MAX_RESOLUTION`]).
    pub resolution: f64,
    /// Raster/vector balance, 0 to 100: 0 rasterizes every page that uses transparency whole;
    /// otherwise a page is rasterized whole when its regions cover more than `balance`% of it,
    /// and regions closer than `(100 - balance)`% of 18 pt are merged. 100 keeps vectors
    /// wherever transparency did not change the look.
    pub balance: u8,
}

impl Default for FlattenSettings {
    fn default() -> Self {
        Self { resolution: FlattenPreset::Medium.resolution(), balance: 75 }
    }
}

impl FlattenSettings {
    pub fn preset(p: FlattenPreset) -> Self {
        Self { resolution: p.resolution(), ..Self::default() }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(self.resolution.is_finite() && (MIN_RESOLUTION..=MAX_RESOLUTION).contains(&self.resolution)) {
            return Err(format!("the resolution must be between {MIN_RESOLUTION} and {MAX_RESOLUTION} pixels per inch"));
        }
        if self.balance > 100 {
            return Err("the raster/vector balance must be between 0 and 100".into());
        }
        Ok(())
    }
}

/// What flattening did to a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageAction {
    /// Only the page's transparency group was removed (nothing it draws is transparent).
    GroupRemoved,
    /// Transparent resources were made opaque; the page looks the same, so no image was added.
    Cleaned,
    /// The transparent regions were replaced by images; everything else stays vector.
    Patched,
    /// The whole page became an image.
    Rasterized,
}

impl PageAction {
    pub fn id(self) -> &'static str {
        match self {
            PageAction::GroupRemoved => "group_removed",
            PageAction::Cleaned => "cleaned",
            PageAction::Patched => "patched",
            PageAction::Rasterized => "rasterized",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PageFlatten {
    /// 0-based page.
    pub page: usize,
    pub action: PageAction,
    /// Rectangular regions rasterized (1 for a whole page).
    pub regions: usize,
    /// Images added.
    pub images: usize,
    /// The share of the page area rasterized, 0 to 1.
    pub raster_fraction: f64,
    /// The resolution the images were made at (lower than asked when the page's pixel budget
    /// required it); 0 when no image was made.
    pub resolution: f64,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct FlattenReport {
    /// Pages that used transparency, in order.
    pub pages: Vec<PageFlatten>,
    /// Requested pages that use no transparency (left untouched), 0-based.
    pub untouched: Vec<usize>,
}

impl FlattenReport {
    pub fn changed(&self) -> bool {
        !self.pages.is_empty()
    }
}

/// The kind of object a resource entry names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    Resources,
    GState,
    XObject,
    Pattern,
    Font,
}

const SUBDICTS: [(&[u8], Kind); 4] = [(b"ExtGState", Kind::GState), (b"XObject", Kind::XObject), (b"Pattern", Kind::Pattern), (b"Font", Kind::Font)];

/// Finds and copies transparent resources. Results are remembered per object, except those
/// that met an object still being looked at (a cycle), which are worked out again when asked.
#[derive(Default)]
struct Scan {
    dirty: HashMap<(Kind, ObjRef), bool>,
    active: HashSet<(Kind, ObjRef)>,
    cycles: usize,
    visits: usize,
    /// Content bytes decoded to find the resources used.
    scanned: usize,
    /// The limits were reached: the page can't be cleaned reliably.
    overflow: bool,
    copies: HashMap<(Kind, ObjRef), ObjRef>,
}

impl Scan {
    fn is_dirty(&mut self, doc: &Document, kind: Kind, o: &Object, depth: usize) -> bool {
        self.visits = self.visits.saturating_add(1);
        if depth > MAX_DEPTH || self.visits > MAX_VISITS {
            self.overflow = true;
            return true;
        }
        let Some(r) = o.as_ref() else { return self.value_dirty(doc, kind, o, depth) };
        if let Some(&v) = self.dirty.get(&(kind, r)) {
            return v;
        }
        if !self.active.insert((kind, r)) {
            self.cycles = self.cycles.saturating_add(1);
            return false;
        }
        let before = self.cycles;
        let resolved = doc.resolve(o);
        let v = self.value_dirty(doc, kind, &resolved, depth);
        self.active.remove(&(kind, r));
        if self.cycles == before || v {
            self.dirty.insert((kind, r), v);
        }
        v
    }

    fn value_dirty(&mut self, doc: &Document, kind: Kind, o: &Object, depth: usize) -> bool {
        let dict = match o {
            Object::Dict(d) => d,
            Object::Stream(s) => &s.dict,
            _ => return false,
        };
        match kind {
            Kind::Resources => {
                for (key, sub) in SUBDICTS {
                    let Some(entries) = dict.get(key).and_then(|e| doc.dict(e)) else { continue };
                    if entries.len() > MAX_ENTRIES {
                        self.overflow = true;
                        return true;
                    }
                    for (_, v) in entries.iter() {
                        if self.is_dirty(doc, sub, v, depth + 1) {
                            return true;
                        }
                    }
                }
                false
            }
            Kind::GState => gstate_soft_mask(doc, dict) || gstate_alpha(doc, dict) || gstate_blend(doc, dict).is_some(),
            Kind::XObject => match dict.name(b"Subtype") {
                Some(b"Image") => image_soft_mask(doc, dict) || image_soft_mask_in_data(doc, dict),
                Some(b"Form") => self.form_dirty(doc, o, depth),
                _ => false,
            },
            Kind::Pattern => match dict.get(b"PatternType").and_then(|t| doc.resolve(t).as_int()) {
                Some(1) => self.form_dirty(doc, o, depth),
                Some(2) => dict.get(b"ExtGState").is_some_and(|g| self.is_dirty(doc, Kind::GState, g, depth + 1)),
                _ => false,
            },
            Kind::Font => {
                dict.name(b"Subtype") == Some(b"Type3") && dict.get(b"Resources").is_some_and(|r| self.is_dirty(doc, Kind::Resources, r, depth + 1))
            }
        }
    }

    /// A form XObject or tiling pattern: its group, and the resources its content uses. One
    /// without resources reads its caller's, which the caller checks in full.
    fn form_dirty(&mut self, doc: &Document, o: &Object, depth: usize) -> bool {
        let Object::Stream(s) = o else { return false };
        if s.dict.get(b"Group").and_then(|g| doc.dict(g)).is_some_and(|g| is_transparency_group(doc, &g)) {
            return true;
        }
        let Some(res) = s.dict.get(b"Resources") else { return false };
        let used = self.used(std::slice::from_ref(o), doc);
        self.resources_dirty(doc, res, used.as_ref(), depth + 1)
    }

    /// The resource names `content` (content streams) uses; `None` when it can't be read.
    fn used(&mut self, content: &[Object], doc: &Document) -> Option<HashSet<(Kind, Vec<u8>)>> {
        let mut data = Vec::new();
        for c in content {
            let Object::Stream(s) = &*doc.resolve(c) else { continue };
            let room = MAX_SCANNED.checked_sub(self.scanned)?;
            let d = s.decoded_within(room.min(MAX_CONTENT)).ok()?;
            self.scanned = self.scanned.saturating_add(d.len());
            data.extend_from_slice(&d);
            data.push(b'\n');
        }
        let mut out = HashSet::new();
        for op in pdfcraft_content::parse(&data).ops {
            let kind = match op.op.as_slice() {
                b"gs" => Kind::GState,
                b"Do" => Kind::XObject,
                b"Tf" => Kind::Font,
                b"scn" | b"SCN" => Kind::Pattern,
                _ => continue,
            };
            let name = if kind == Kind::Pattern { op.operands.last().and_then(Object::as_name) } else { op.name(0) };
            if let Some(n) = name {
                out.insert((kind, n.to_vec()));
            }
        }
        Some(out)
    }

    /// Whether the entries of resources `res` that `used` names (all when `None`) hold
    /// transparency. Content that draws a form, pattern or Type 3 font without resources of its
    /// own makes every entry count, since that content reads these too.
    fn resources_dirty(&mut self, doc: &Document, res: &Object, used: Option<&HashSet<(Kind, Vec<u8>)>>, depth: usize) -> bool {
        let Some(d) = doc.dict(res) else { return false };
        let borrows = |kind: Kind, name: &[u8]| -> bool {
            let Some((key, _)) = SUBDICTS.iter().find(|(_, k)| *k == kind) else { return false };
            let Some(target) = d.get(key).and_then(|e| doc.dict(e)).and_then(|e| e.get(name).cloned()) else { return false };
            let t = doc.resolve(&target);
            let td = match &*t {
                Object::Stream(s) => &s.dict,
                Object::Dict(d) => d,
                _ => return false,
            };
            let draws = match kind {
                Kind::XObject => td.name(b"Subtype") == Some(b"Form"),
                Kind::Pattern => matches!(&*t, Object::Stream(_)),
                Kind::Font => td.name(b"Subtype") == Some(b"Type3"),
                _ => false,
            };
            draws && !td.contains(b"Resources")
        };
        let used = used.filter(|u| !u.iter().any(|(k, n)| borrows(*k, n)));
        let Some(used) = used else {
            return self.is_dirty(doc, Kind::Resources, res, depth);
        };
        for (key, kind) in SUBDICTS {
            let Some(entries) = d.get(key).and_then(|e| doc.dict(e)) else { continue };
            for (name, v) in entries.iter().take(MAX_ENTRIES) {
                if used.contains(&(kind, name.to_vec())) && self.is_dirty(doc, kind, v, depth + 1) {
                    return true;
                }
            }
        }
        false
    }

    /// An opaque replacement for `o` (a reference to a new copy, or a changed direct object),
    /// or `None` when it has no transparency.
    fn clean(&mut self, doc: &mut Document, kind: Kind, o: &Object, depth: usize) -> Option<Object> {
        if depth > MAX_DEPTH {
            self.overflow = true;
            return None;
        }
        self.active.clear();
        if !self.is_dirty(doc, kind, o, depth) {
            return None;
        }
        let Some(r) = o.as_ref() else { return self.clean_value(doc, kind, o, depth) };
        if let Some(n) = self.copies.get(&(kind, r)) {
            return Some(Object::Ref(*n));
        }
        // Reserve the copy's number first, so a cycle back to `o` points at the copy.
        let n = doc.add(Object::Null);
        self.copies.insert((kind, r), n);
        let resolved = doc.resolve(o);
        let cleaned = self.clean_value(doc, kind, &resolved, depth).unwrap_or_else(|| (*resolved).clone());
        doc.set(n, cleaned);
        Some(Object::Ref(n))
    }

    fn clean_value(&mut self, doc: &mut Document, kind: Kind, o: &Object, depth: usize) -> Option<Object> {
        match (kind, o) {
            (Kind::Resources, Object::Dict(d)) => Some(Object::Dict(self.clean_resources(doc, d, depth))),
            (Kind::GState, Object::Dict(d)) => {
                let mut g = d.clone();
                if gstate_soft_mask(doc, d) {
                    g.set(b"SMask".to_vec(), Object::name("None"));
                }
                for key in [b"CA".as_slice(), b"ca"] {
                    if d.get(key).and_then(|v| doc.resolve(v).as_f64()).is_some_and(|v| v < 1.0) {
                        g.set(key.to_vec(), Object::Real(1.0));
                    }
                }
                if gstate_blend(doc, d).is_some() {
                    g.set(b"BM".to_vec(), Object::name("Normal"));
                }
                Some(Object::Dict(g))
            }
            (Kind::XObject | Kind::Pattern, Object::Stream(s)) => {
                let mut s = s.clone();
                if s.dict.name(b"Subtype") == Some(b"Image") {
                    s.dict.remove(b"SMask");
                    s.dict.remove(b"SMaskInData");
                } else {
                    if s.dict.get(b"Group").and_then(|g| doc.dict(g)).is_some_and(|g| is_transparency_group(doc, &g)) {
                        s.dict.remove(b"Group");
                    }
                    if let Some(res) = s.dict.get(b"Resources").cloned()
                        && let Some(c) = self.clean(doc, Kind::Resources, &res, depth + 1)
                    {
                        s.dict.set(b"Resources".to_vec(), c);
                    }
                }
                Some(Object::Stream(s))
            }
            (Kind::Pattern, Object::Dict(d)) => {
                let mut p = d.clone();
                if let Some(g) = d.get(b"ExtGState")
                    && let Some(c) = self.clean(doc, Kind::GState, g, depth + 1)
                {
                    p.set(b"ExtGState".to_vec(), c);
                }
                Some(Object::Dict(p))
            }
            (Kind::Font, Object::Dict(d)) => {
                let mut f = d.clone();
                if let Some(res) = d.get(b"Resources")
                    && let Some(c) = self.clean(doc, Kind::Resources, res, depth + 1)
                {
                    f.set(b"Resources".to_vec(), c);
                }
                Some(Object::Dict(f))
            }
            _ => None,
        }
    }

    fn clean_resources(&mut self, doc: &mut Document, d: &Dict, depth: usize) -> Dict {
        let mut out = d.clone();
        for (key, kind) in SUBDICTS {
            let Some(entries) = d.get(key).and_then(|e| doc.dict(e)) else { continue };
            let mut changed = entries.clone();
            let mut any = false;
            for (name, v) in entries.iter().take(MAX_ENTRIES) {
                if let Some(c) = self.clean(doc, kind, v, depth + 1) {
                    changed.set(name.clone(), c);
                    any = true;
                }
            }
            if any {
                out.set(key.to_vec(), Object::Dict(changed));
            }
        }
        out
    }
}

/// A page's own transparency group and whether its resources hold transparency.
fn page_transparency(doc: &Document, scan: &mut Scan, page: &pdfcraft_model::Page) -> (bool, bool) {
    let group = page.dict.get(b"Group").and_then(|g| doc.dict(g)).is_some_and(|g| is_transparency_group(doc, &g));
    scan.active.clear();
    scan.visits = 0;
    let used = scan.used(&content_items(doc, page), doc);
    let res = page.dict.get(b"Resources").is_some_and(|r| scan.resources_dirty(doc, r, used.as_ref(), 0));
    (group, res)
}

/// Of `pages` (0-based; all when empty), those that use transparency. Out-of-range pages are
/// ignored.
pub fn transparent_pages(doc: &Document, pages: &[usize]) -> Vec<usize> {
    let all = pdfcraft_model::pages(doc);
    let mut scan = Scan::default();
    let wanted: Vec<usize> = if pages.is_empty() { (0..all.len()).collect() } else { pages.to_vec() };
    let mut out: Vec<usize> = wanted
        .into_iter()
        .filter(|&i| {
            all.get(i).is_some_and(|p| {
                let (g, r) = page_transparency(doc, &mut scan, p);
                g || r
            })
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The media box, normalized; the default letter size when it is missing or unusable.
fn media(doc: &Document, page: &pdfcraft_model::Page) -> [f64; 4] {
    let v: Vec<f64> = page
        .dict
        .get(b"MediaBox")
        .and_then(|m| doc.resolve(m).as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|x| doc.resolve(x).as_f64())
        .collect();
    match v.as_slice() {
        [a, b, c, d] if v.iter().all(|x| x.is_finite()) && (c - a).abs() >= 1.0 && (d - b).abs() >= 1.0 => {
            [a.min(*c), b.min(*d), a.max(*c), b.max(*d)]
        }
        _ => [0.0, 0.0, 612.0, 792.0],
    }
}

fn rect_object(r: [f64; 4]) -> Object {
    Object::Array(r.iter().map(|v| Object::Real(*v)).collect())
}

/// The document as a renderer reads it, with `pages` shown unrotated over their whole media box,
/// in user units and without annotations, so pixels map straight to user space.
fn render_bytes(doc: &Document, pages: &[(usize, ObjRef, [f64; 4])]) -> Result<Arc<Vec<u8>>, String> {
    let mut view = doc.clone();
    for (_, obj, m) in pages {
        view.update_dict(*obj, |d| {
            d.set(b"MediaBox".to_vec(), rect_object(*m));
            d.set(b"CropBox".to_vec(), rect_object(*m));
            d.set(b"Rotate".to_vec(), Object::Int(0));
            d.remove(b"Annots");
            d.remove(b"UserUnit");
        })
        .map_err(|e| e.to_string())?;
    }
    pdfcraft_cos::write_decrypted_view(&view).map(Arc::new).map_err(|e| format!("the page could not be prepared for rendering: {e}"))
}

fn renderer(bytes: Arc<Vec<u8>>) -> PageRenderer {
    PageRenderer::new(bytes, RenderConfig { native_vector: false, ..RenderConfig::default() })
}

fn rendered(r: RenderedPage, page: usize) -> Result<RenderedPage, String> {
    match &r.error {
        Some(e) => Err(format!("page {} could not be rendered: {e}", page + 1)),
        None => Ok(r),
    }
}

/// A rectangle of pixels, `[x0, y0, x1, y1)`, y down.
type PixRect = [usize; 4];

fn area(r: &PixRect) -> usize {
    r[2].saturating_sub(r[0]).saturating_mul(r[3].saturating_sub(r[1]))
}

fn near(a: &PixRect, b: &PixRect, gap: usize) -> bool {
    a[0] <= b[2].saturating_add(gap) && b[0] <= a[2].saturating_add(gap) && a[1] <= b[3].saturating_add(gap) && b[1] <= a[3].saturating_add(gap)
}

fn union(a: &PixRect, b: &PixRect) -> PixRect {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

/// Regions where two equally sized renders differ.
fn diff_regions(before: &[u8], after: &[u8], w: usize, h: usize, gap: usize) -> Vec<PixRect> {
    let (cw, ch) = (w.div_ceil(CELL), h.div_ceil(CELL));
    let mut cells: Vec<Option<PixRect>> = vec![None; cw.saturating_mul(ch)];
    for (i, (a, b)) in before.as_chunks::<4>().0.iter().zip(after.as_chunks::<4>().0.iter()).enumerate() {
        if a == b {
            continue;
        }
        let (x, y) = (i % w.max(1), i / w.max(1));
        if let Some(c) = cells.get_mut((y / CELL) * cw + x / CELL) {
            let px = [x, y, x + 1, y + 1];
            *c = Some(c.map_or(px, |r| union(&r, &px)));
        }
    }
    // 8-connected groups of cells.
    let mut seen = vec![false; cells.len()];
    let mut regions: Vec<PixRect> = Vec::new();
    for start in 0..cells.len() {
        if seen.get(start).copied().unwrap_or(true) || cells.get(start).copied().flatten().is_none() {
            continue;
        }
        let mut bbox: Option<PixRect> = None;
        let mut queue = VecDeque::from([start]);
        if let Some(s) = seen.get_mut(start) {
            *s = true;
        }
        while let Some(c) = queue.pop_front() {
            if let Some(r) = cells.get(c).copied().flatten() {
                bbox = Some(bbox.map_or(r, |b| union(&b, &r)));
            }
            let (cx, cy) = ((c % cw) as i64, (c / cw) as i64);
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    let (nx, ny) = (cx + dx, cy + dy);
                    if nx < 0 || ny < 0 || nx >= cw as i64 || ny >= ch as i64 {
                        continue;
                    }
                    let n = ny as usize * cw + nx as usize;
                    if !seen.get(n).copied().unwrap_or(true) && cells.get(n).copied().flatten().is_some() {
                        if let Some(s) = seen.get_mut(n) {
                            *s = true;
                        }
                        queue.push_back(n);
                    }
                }
            }
        }
        if let Some(b) = bbox {
            regions.push([b[0].saturating_sub(MARGIN), b[1].saturating_sub(MARGIN), (b[2] + MARGIN).min(w), (b[3] + MARGIN).min(h)]);
        }
    }
    if regions.len() > MAX_REGIONS {
        return regions.iter().copied().reduce(|a, b| union(&a, &b)).into_iter().collect();
    }
    // Merge regions that overlap or are within `gap` of each other.
    loop {
        let mut merged = false;
        'outer: for i in 0..regions.len() {
            for j in (i + 1)..regions.len() {
                if let (Some(a), Some(b)) = (regions.get(i), regions.get(j))
                    && near(a, b, gap)
                {
                    let u = union(a, b);
                    if let Some(slot) = regions.get_mut(i) {
                        *slot = u;
                    }
                    regions.swap_remove(j);
                    merged = true;
                    break 'outer;
                }
            }
        }
        if !merged {
            break;
        }
    }
    regions
}

/// How page content can be extended with images on top: the `q`s to open first so its own
/// unbalanced `Q`s can't reach the page's initial state, the `Q`s that then close everything,
/// and whether it ends inside a text object. `None` when it can't be read or nests too deep.
fn content_balance(doc: &Document, items: &[Object]) -> Option<(i64, i64, bool)> {
    let mut data = Vec::new();
    for c in items {
        let Object::Stream(s) = &*doc.resolve(c) else { continue };
        let room = MAX_CONTENT.checked_sub(data.len())?;
        data.extend_from_slice(&s.decoded_within(room).ok()?);
        data.push(b'\n');
    }
    let parsed = pdfcraft_content::parse(&data);
    let (mut depth, mut lowest, mut text) = (0i64, 0i64, false);
    for op in &parsed.ops {
        match op.op.as_slice() {
            b"q" => depth += 1,
            b"Q" => depth -= 1,
            b"BT" => text = true,
            b"ET" => text = false,
            _ => {}
        }
        lowest = lowest.min(depth);
        if depth.abs() > MAX_NESTING {
            return None;
        }
    }
    let open = 1 - lowest;
    let close = open + depth;
    (close >= 0).then_some((open, close, text))
}

/// The page's content streams, as an array of objects (references kept).
fn content_items(doc: &Document, page: &pdfcraft_model::Page) -> Vec<Object> {
    match page.dict.get(b"Contents") {
        None => Vec::new(),
        Some(c) => match &*doc.resolve(c) {
            Object::Array(a) => a.clone(),
            Object::Stream(_) => vec![c.clone()],
            _ => Vec::new(),
        },
    }
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" { "0".into() } else { s.into() }
}

/// Images rendered from `r` for `regions` (pixels at `scale`) of the page at `media`: their
/// objects and the content that draws them.
fn patches(
    doc: &mut Document,
    r: &mut PageRenderer,
    page: usize,
    m: [f64; 4],
    scale: f64,
    regions: &[PixRect],
) -> Result<Vec<(ObjRef, String)>, String> {
    let mut out = Vec::new();
    for reg in regions {
        let mut y = reg[1];
        while y < reg[3] {
            let th = (reg[3] - y).min(TILE as usize);
            let mut x = reg[0];
            while x < reg[2] {
                let tw = (reg[2] - x).min(TILE as usize);
                let tile = Tile { x: x as u32, y: y as u32, w: tw as u32, h: th as u32 };
                let img = rendered(r.render(RenderRequest { page, tile: Some(tile), scale: scale as f32, ..Default::default() }), page)?;
                if img.width as usize != tw || img.height as usize != th {
                    return Err(format!("page {} rendered at an unexpected size", page + 1));
                }
                let mut rgb = Vec::with_capacity(tw.saturating_mul(th).saturating_mul(3));
                for &[r, g, b, a] in img.rgba.as_chunks::<4>().0 {
                    // Premultiplied over white: what shows through is white.
                    let under = 255 - a;
                    rgb.extend([r.saturating_add(under), g.saturating_add(under), b.saturating_add(under)]);
                }
                let mut d = Dict::new();
                d.set(b"Type".to_vec(), Object::name("XObject"));
                d.set(b"Subtype".to_vec(), Object::name("Image"));
                d.set(b"Width".to_vec(), Object::Int(tw as i64));
                d.set(b"Height".to_vec(), Object::Int(th as i64));
                d.set(b"ColorSpace".to_vec(), Object::name("DeviceRGB"));
                d.set(b"BitsPerComponent".to_vec(), Object::Int(8));
                let obj = doc.add(Object::Stream(Stream::flate(d, &rgb)));
                let (llx, lly) = (m[0] + x as f64 / scale, m[3] - (y + th) as f64 / scale);
                let draw = format!("q {} 0 0 {} {} {} cm", fmt(tw as f64 / scale), fmt(th as f64 / scale), fmt(llx), fmt(lly));
                out.push((obj, draw));
                x += tw;
            }
            y += th;
        }
    }
    Ok(out)
}

/// Flatten transparency on `pages` (0-based; every page when empty). Pages that use none are
/// left exactly as they are. All or nothing: on an error `doc` may be partly changed, so call
/// it on a copy (the engine does).
pub fn flatten_transparency(doc: &mut Document, pages: &[usize], settings: &FlattenSettings) -> Result<FlattenReport, String> {
    settings.validate()?;
    let all = pdfcraft_model::pages(doc);
    if all.is_empty() {
        return Err("the document has no pages".into());
    }
    if let Some(p) = pages.iter().find(|p| **p >= all.len()) {
        return Err(format!("page {} is out of range: the document has {} pages", p.saturating_add(1), all.len()));
    }
    let mut wanted: Vec<usize> = if pages.is_empty() { (0..all.len()).collect() } else { pages.to_vec() };
    wanted.sort_unstable();
    wanted.dedup();

    let mut scan = Scan::default();
    let mut report = FlattenReport::default();
    // (page, its group, resources transparent, too complex to clean)
    let mut targets = Vec::new();
    for &i in &wanted {
        let Some(p) = all.get(i) else { continue };
        scan.overflow = false;
        let (group, res) = page_transparency(doc, &mut scan, p);
        if group || res {
            targets.push((i, group, res, scan.overflow));
        } else {
            report.untouched.push(i);
        }
    }
    if targets.is_empty() {
        return Ok(report);
    }
    let render: Vec<(usize, ObjRef, [f64; 4])> =
        targets.iter().filter(|t| t.2).filter_map(|t| all.get(t.0).map(|p| (t.0, p.obj, media(doc, p)))).collect();
    let before = if render.is_empty() { None } else { Some(render_bytes(doc, &render)?) };

    // Opaque resources, no page group.
    for t in &mut targets {
        let (i, group, res) = (t.0, t.1, t.2);
        let Some(p) = all.get(i) else { continue };
        scan.overflow = false;
        scan.visits = 0;
        let cleaned = match (res, p.dict.get(b"Resources")) {
            (true, Some(r)) => scan.clean(doc, Kind::Resources, r, 0),
            _ => None,
        };
        // Nested too deep to copy completely: the page's content is replaced by its image.
        t.3 |= scan.overflow;
        doc.update_dict(p.obj, |d| {
            if group {
                d.remove(b"Group");
            }
            if let Some(c) = cleaned {
                d.set(b"Resources".to_vec(), c);
            }
        })
        .map_err(|e| e.to_string())?;
        if !res {
            report.pages.push(PageFlatten {
                page: i,
                action: PageAction::GroupRemoved,
                regions: 0,
                images: 0,
                raster_fraction: 0.0,
                resolution: 0.0,
            });
        }
    }
    let Some(before) = before else { return Ok(report) };
    let after = render_bytes(doc, &render)?;
    let (mut before, mut after) = (renderer(before), renderer(after));
    let pages_now = pdfcraft_model::pages(doc);

    for &(i, _, _, overflow) in targets.iter().filter(|t| t.2) {
        let (Some(page), Some(&(_, _, m))) = (pages_now.get(i), render.iter().find(|r| r.0 == i)) else { continue };
        let (w_pt, h_pt) = (m[2] - m[0], m[3] - m[1]);
        let ds = DIFF_SCALE.min(MAX_DIFF_SIDE / w_pt.max(h_pt)).max(0.01);
        let req = RenderRequest { page: i, scale: ds as f32, ..Default::default() };
        let b = rendered(before.render(req), i)?;
        let a = rendered(after.render(req), i)?;
        if (b.width, b.height) != (a.width, a.height) || b.width == 0 || b.height == 0 {
            return Err(format!("page {} could not be compared before and after", i + 1));
        }
        let (dw, dh) = (b.width as usize, b.height as usize);
        let (dsx, dsy) = (dw as f64 / w_pt, dh as f64 / h_pt);
        let gap = ((100 - settings.balance.min(100)) as f64 / 100.0 * 18.0 * dsx).round() as usize;
        let regions = diff_regions(&b.rgba, &a.rgba, dw, dh, gap);
        let covered: usize = regions.iter().map(area).sum();
        let fraction = covered as f64 / (dw.saturating_mul(dh)).max(1) as f64;
        let items = content_items(doc, page);
        let balance = content_balance(doc, &items);
        let whole = settings.balance == 0 || overflow || balance.is_none() || fraction > settings.balance as f64 / 100.0;
        if regions.is_empty() && !whole {
            report.pages.push(PageFlatten { page: i, action: PageAction::Cleaned, regions: 0, images: 0, raster_fraction: 0.0, resolution: 0.0 });
            continue;
        }
        // Regions at the final resolution, within the page's pixel budget.
        let mut scale = settings.resolution / 72.0;
        let (pw, ph) = ((w_pt * scale).ceil(), (h_pt * scale).ceil());
        let to_final = |scale: f64| -> Vec<PixRect> {
            let (pw, ph) = ((w_pt * scale).ceil().max(1.0) as usize, (h_pt * scale).ceil().max(1.0) as usize);
            if whole {
                return vec![[0, 0, pw, ph]];
            }
            regions
                .iter()
                .map(|r| {
                    let f = |v: usize, s: f64| v as f64 / s * scale;
                    [
                        (f(r[0], dsx).floor() as usize).min(pw),
                        (f(r[1], dsy).floor() as usize).min(ph),
                        (f(r[2], dsx).ceil() as usize).min(pw),
                        (f(r[3], dsy).ceil() as usize).min(ph),
                    ]
                })
                .filter(|r| area(r) > 0)
                .collect()
        };
        let mut finals = to_final(scale);
        let pixels: f64 = finals.iter().map(|r| area(r) as f64).sum();
        if pixels > MAX_PAGE_PIXELS || !(pw.is_finite() && ph.is_finite()) {
            scale *= (MAX_PAGE_PIXELS / pixels.max(1.0)).sqrt() * 0.99;
            finals = to_final(scale);
        }
        if !(scale.is_finite() && scale >= 0.01) {
            return Err(format!("page {} is too large to flatten", i + 1));
        }
        let images = patches(doc, &mut before, i, m, scale, &finals)?;
        // The page's resources (already opaque), with the images.
        let mut res = page.dict.get(b"Resources").and_then(|r| doc.dict(r)).unwrap_or_default();
        let mut xobjects = if whole { Dict::new() } else { res.get(b"XObject").and_then(|x| doc.dict(x)).unwrap_or_default() };
        let mut draw = String::new();
        let mut n = 0usize;
        for (obj, place) in &images {
            let name = loop {
                let name = format!("PcFlat{n}");
                n += 1;
                if !xobjects.contains(name.as_bytes()) {
                    break name;
                }
            };
            xobjects.set(name.as_bytes().to_vec(), Object::Ref(*obj));
            draw.push_str(&format!("{place} /{name} Do Q\n"));
        }
        let contents = match (whole, balance) {
            (false, Some((open, close, text))) => {
                let prefix = doc.add(Object::Stream(Stream::flate(Dict::new(), "q\n".repeat(open.max(0) as usize).as_bytes())));
                let mut suffix = String::new();
                if text {
                    suffix.push_str("ET\n");
                }
                suffix.push_str(&"Q\n".repeat(close.max(0) as usize));
                suffix.push_str(&draw);
                let suffix = doc.add(Object::Stream(Stream::flate(Dict::new(), suffix.as_bytes())));
                let mut v = vec![Object::Ref(prefix)];
                v.extend(items);
                v.push(Object::Ref(suffix));
                Object::Array(v)
            }
            _ => {
                res = Dict::new();
                Object::Ref(doc.add(Object::Stream(Stream::flate(Dict::new(), draw.as_bytes()))))
            }
        };
        res.set(b"XObject".to_vec(), Object::Dict(xobjects));
        doc.update_dict(page.obj, |d| {
            d.set(b"Resources".to_vec(), Object::Dict(res));
            d.set(b"Contents".to_vec(), contents);
        })
        .map_err(|e| e.to_string())?;
        let total = (w_pt * scale).ceil().max(1.0) * (h_pt * scale).ceil().max(1.0);
        let raster: f64 = finals.iter().map(|r| area(r) as f64).sum();
        report.pages.push(PageFlatten {
            page: i,
            action: if whole { PageAction::Rasterized } else { PageAction::Patched },
            regions: finals.len(),
            images: images.len(),
            raster_fraction: if whole { 1.0 } else { (raster / total).clamp(0.0, 1.0) },
            resolution: scale * 72.0,
        });
    }
    report.pages.sort_by_key(|p| p.page);
    Ok(report)
}

/// The most pixels all the pages of one [`rasterize_pages`] call may have together (about 3 GB
/// of uncompressed RGB): 1 000 Letter pages at 300 ppi fit, 10 000 do not.
pub const MAX_RASTER_PIXELS: f64 = 1.0e9;

/// What [`rasterize_pages`] did to one page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageRaster {
    /// 0-based page.
    pub page: usize,
    /// The picture's size in pixels.
    pub width: usize,
    pub height: usize,
    /// Images (tiles of at most 2048 pixels) the picture is made of.
    pub images: usize,
    /// Pixels per inch (per 72 user units) the picture was made at: the requested resolution,
    /// or less when the page is larger than [`MAX_PAGE_PIXELS`] allows at it.
    pub resolution: f64,
}

/// Print as image: replace each of `pages` (0-based; every page when empty) by one opaque
/// DeviceRGB picture of itself at `resolution` pixels per inch, so a printer receives no fonts,
/// vectors or transparency. The picture covers the media box (unrotated, white where nothing is
/// drawn); the boxes, `/Rotate` and annotations stay, so annotations are not part of the picture
/// (impose them into the page first, as printing does). The page's transparency group is removed.
///
/// All or nothing: on an error `doc` may be partly changed, so call it on a copy.
pub fn rasterize_pages(doc: &mut Document, pages: &[usize], resolution: f64) -> Result<Vec<PageRaster>, String> {
    if !(resolution.is_finite() && (MIN_RESOLUTION..=MAX_RESOLUTION).contains(&resolution)) {
        return Err(format!("the resolution must be between {MIN_RESOLUTION} and {MAX_RESOLUTION} pixels per inch"));
    }
    let all = pdfcraft_model::pages(doc);
    if all.is_empty() {
        return Err("the document has no pages".into());
    }
    if let Some(p) = pages.iter().find(|p| **p >= all.len()) {
        return Err(format!("page {} is out of range: the document has {} pages", p.saturating_add(1), all.len()));
    }
    let mut wanted: Vec<usize> = if pages.is_empty() { (0..all.len()).collect() } else { pages.to_vec() };
    wanted.sort_unstable();
    wanted.dedup();
    let render: Vec<(usize, ObjRef, [f64; 4])> = wanted.iter().filter_map(|&i| all.get(i).map(|p| (i, p.obj, media(doc, p)))).collect();
    // Pixel sizes first, so an oversized job fails before anything is rendered.
    let mut plan = Vec::with_capacity(render.len());
    let mut total = 0.0;
    for &(i, obj, m) in &render {
        let (w_pt, h_pt) = (m[2] - m[0], m[3] - m[1]);
        let mut scale = resolution / 72.0;
        let pixels = (w_pt * scale).ceil().max(1.0) * (h_pt * scale).ceil().max(1.0);
        if !pixels.is_finite() {
            return Err(format!("page {} is too large to print as an image", i + 1));
        }
        if pixels > MAX_PAGE_PIXELS {
            scale *= (MAX_PAGE_PIXELS / pixels).sqrt() * 0.99;
        }
        if !(scale.is_finite() && scale >= 0.01) {
            return Err(format!("page {} is too large to print as an image", i + 1));
        }
        let (pw, ph) = ((w_pt * scale).ceil().max(1.0) as usize, (h_pt * scale).ceil().max(1.0) as usize);
        total += pw as f64 * ph as f64;
        if total > MAX_RASTER_PIXELS {
            return Err(format!(
                "printing these pages as images at {resolution} ppi needs more than {} million pixels: lower the resolution or print fewer pages",
                MAX_RASTER_PIXELS / 1.0e6
            ));
        }
        plan.push((i, obj, m, scale, pw, ph));
    }
    let mut r = renderer(render_bytes(doc, &render)?);
    let mut out = Vec::with_capacity(plan.len());
    for (i, obj, m, scale, pw, ph) in plan {
        let images = patches(doc, &mut r, i, m, scale, &[[0, 0, pw, ph]])?;
        let mut xobjects = Dict::new();
        let mut draw = String::new();
        for (k, (img, place)) in images.iter().enumerate() {
            let name = format!("PcImg{k}");
            xobjects.set(name.as_bytes().to_vec(), Object::Ref(*img));
            draw.push_str(&format!("{place} /{name} Do Q\n"));
        }
        let mut res = Dict::new();
        res.set(b"XObject".to_vec(), Object::Dict(xobjects));
        let contents = doc.add(Object::Stream(Stream::flate(Dict::new(), draw.as_bytes())));
        doc.update_dict(obj, |d| {
            d.set(b"Resources".to_vec(), Object::Dict(res));
            d.set(b"Contents".to_vec(), Object::Ref(contents));
            d.remove(b"Group");
        })
        .map_err(|e| e.to_string())?;
        out.push(PageRaster { page: i, width: pw, height: ph, images: images.len(), resolution: scale * 72.0 });
    }
    Ok(out)
}

#[cfg(test)]
#[path = "flatten_tests.rs"]
mod tests;

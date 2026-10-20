//! Edit a PDF ▸ edit existing images and grouped artwork (Image/Form XObjects painted by `Do`
//! in its own content streams), and changing one: move/resize/rotate/flip (the `Do` is wrapped in
//! `q … cm … Q` with the extra transform), replace (it draws another XObject in the same place),
//! crop (the `Do` draws a new Form XObject whose clip keeps part of it, so the crop moves with
//! later edits), or delete. Only the stream that draws it is rewritten, as a new object.

use pdfcraft_content::{Matrix, Op, parse, serialize_ops};
use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::EditError;

/// An image drawn on a page.
#[derive(Clone, Debug, PartialEq)]
pub struct PageImage {
    /// Its box in user space.
    pub rect: [f64; 4],
    /// The placement: image unit square or Form coordinates → user space.
    pub matrix: [f64; 6],
    /// The XObject resource name and object.
    pub name: String,
    pub object: Option<ObjRef>,
    /// Whether this is grouped Form artwork, edited as a whole (including its nested content).
    pub is_form: bool,
    /// Pixel size; zero for vector/grouped Form artwork.
    pub width: u32,
    pub height: u32,
    stream: usize,
    op: usize,
    /// CTM before `Do`, excluding a Form's own Matrix.
    draw_matrix: [f64; 6],
}

fn streams(doc: &Document, page: &Dict) -> Vec<(Object, Vec<u8>)> {
    let list: Vec<Object> = match page.get(b"Contents") {
        None => Vec::new(),
        Some(c) => match &*doc.resolve(c) {
            Object::Array(a) => a.clone(),
            _ => vec![c.clone()],
        },
    };
    list.into_iter()
        .map(|o| {
            let data = match &*doc.resolve(&o) {
                Object::Stream(s) => s.decoded().unwrap_or_default(),
                _ => Vec::new(),
            };
            (o, data)
        })
        .collect()
}

fn page_of(doc: &Document, page: usize) -> Result<pdfcraft_model::Page, EditError> {
    pdfcraft_model::pages(doc).into_iter().nth(page).ok_or(EditError::NoSuchPage(page))
}

fn xobjects(doc: &Document, page: &Dict) -> Dict {
    page.get(b"Resources")
        .map(|r| doc.resolve(r))
        .and_then(|r| r.as_dict().and_then(|d| d.get(b"XObject").map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned())))
        .unwrap_or_default()
}

/// The marker a crop wrapper carries: the bitmap it clips (Replace and Save Image As use it).
const CROP_OF: &[u8] = b"PCCropOf";

fn pixels(d: &Dict, key: &[u8]) -> u32 {
    d.int(key).unwrap_or(0).clamp(0, u32::MAX as i64) as u32
}

/// For a crop wrapper made by [`change_image`] around a bitmap: that bitmap and its pixel size.
fn cropped_image(doc: &Document, form: &Dict) -> Option<(ObjRef, u32, u32)> {
    let inner = form.get(CROP_OF)?.as_ref()?;
    let obj = doc.get(inner);
    let Object::Stream(s) = &*obj else { return None };
    (s.dict.name(b"Subtype") == Some(b"Image")).then(|| (inner, pixels(&s.dict, b"Width"), pixels(&s.dict, b"Height")))
}

/// The images and Form artwork page `page` (0-based) draws, in drawing order.
/// Forms stay intact: no traversal or rewriting of their internal objects is needed.
pub fn page_images(doc: &Document, page: usize) -> Result<Vec<PageImage>, EditError> {
    let p = page_of(doc, page)?;
    let xo = xobjects(doc, &p.dict);
    let mut out = Vec::new();
    // The graphics state carries over from one stream to the next.
    let mut ctm = Matrix::IDENTITY;
    let mut stack = Vec::new();
    for (si, (_, data)) in streams(doc, &p.dict).into_iter().enumerate() {
        for (i, op) in parse(&data).ops.iter().enumerate() {
            match op.op.as_slice() {
                b"q" => stack.push(ctm),
                b"Q" => ctm = stack.pop().unwrap_or(ctm),
                b"cm" => {
                    if let Some(m) = op.nums::<6>() {
                        ctm = Matrix(m).then(&ctm);
                    }
                }
                b"Do" => {
                    let Some(name) = op.name(0) else { continue };
                    let Some(resource) = xo.get(name) else { continue };
                    let obj = doc.resolve(resource);
                    let Object::Stream(s) = &*obj else { continue };
                    let mut is_form = s.dict.name(b"Subtype") == Some(b"Form");
                    // A crop wrapper around a bitmap is still that bitmap (Replace, Save Image As).
                    let cropped = if is_form { cropped_image(doc, &s.dict) } else { None };
                    let (matrix, bounds) = if is_form {
                        let Some(bounds) = s.dict.get(b"BBox").map(|v| doc.resolve(v)).and_then(|v| {
                            v.as_array().and_then(|a| {
                                if a.len() != 4 {
                                    return None;
                                }
                                let mut b = [0.0; 4];
                                for (n, v) in b.iter_mut().zip(a) {
                                    *n = v.as_f64()?;
                                }
                                Some(b)
                            })
                        }) else {
                            continue;
                        };
                        let matrix = match s.dict.get(b"Matrix") {
                            Some(v) => {
                                let v = doc.resolve(v);
                                let Some(m) = v.as_array().and_then(|a| Matrix::from_operands(a)) else { continue };
                                m
                            }
                            None => Matrix::IDENTITY,
                        };
                        (matrix.then(&ctm), bounds)
                    } else if s.dict.name(b"Subtype") == Some(b"Image") {
                        (ctm, [0.0, 0.0, 1.0, 1.0])
                    } else {
                        continue;
                    };
                    let rect = matrix.bbox(bounds);
                    if !bounds.iter().chain(matrix.0.iter()).chain(rect.iter()).all(|v| v.is_finite())
                        || bounds[2] <= bounds[0]
                        || bounds[3] <= bounds[1]
                        || rect[2] <= rect[0]
                        || rect[3] <= rect[1]
                    {
                        continue;
                    }
                    let (object, pixels) = match &cropped {
                        Some((inner, w, h)) => {
                            is_form = false;
                            (Some(*inner), [*w, *h])
                        }
                        None if is_form => (resource.as_ref(), [0, 0]),
                        None => (resource.as_ref(), [pixels(&s.dict, b"Width"), pixels(&s.dict, b"Height")]),
                    };
                    out.push(PageImage {
                        rect,
                        matrix: matrix.0,
                        name: String::from_utf8_lossy(name).into_owned(),
                        object,
                        is_form,
                        width: pixels[0],
                        height: pixels[1],
                        stream: si,
                        op: i,
                        draw_matrix: ctm.0,
                    });
                }
                _ => {}
            }
        }
    }
    Ok(out)
}

/// The images page `page` (0-based) shows, in drawing order, including those drawn inside form
/// XObjects (Export to Word, HTML and RTF). Read-only: indexes into this list are not
/// [`page_images`] indexes, so they can't be passed to [`change_image`].
pub fn reading_images(doc: &Document, page: usize) -> Result<Vec<PageImage>, EditError> {
    /// The CTM and `q` stack carry from one page stream to the next (§7.8.2); a form starts
    /// its own.
    /// The forms being read (cycle guard) and how many were entered (the page's budget).
    struct Forms {
        path: Vec<ObjRef>,
        visits: usize,
    }
    struct State {
        ctm: Matrix,
        stack: Vec<Matrix>,
    }
    fn walk(doc: &Document, data: &[u8], resources: &Dict, st: &mut State, stream: usize, forms: &mut Forms, out: &mut Vec<PageImage>) {
        let xo = resources.get(b"XObject").map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default();
        let (ctm, stack) = (&mut st.ctm, &mut st.stack);
        for (i, op) in parse(data).ops.iter().enumerate() {
            match op.op.as_slice() {
                b"q" => stack.push(*ctm),
                b"Q" => *ctm = stack.pop().unwrap_or(*ctm),
                b"cm" => {
                    if let Some(m) = op.nums::<6>() {
                        *ctm = Matrix(m).then(ctm);
                    }
                }
                b"Do" => {
                    let Some(name) = op.name(0) else { continue };
                    if let Some(form) = crate::text::form_call(doc, resources, name, *ctm, &forms.path, &mut forms.visits) {
                        if let Some(r) = form.obj {
                            forms.path.push(r);
                        }
                        let mut inner = State { ctm: form.ctm, stack: Vec::new() };
                        walk(doc, &form.data, &form.resources, &mut inner, stream, forms, out);
                        if form.obj.is_some() {
                            forms.path.pop();
                        }
                        continue;
                    }
                    let Some(r) = xo.get(name).and_then(Object::as_ref) else { continue };
                    let obj = doc.get(r);
                    let Object::Stream(s) = &*obj else { continue };
                    if s.dict.name(b"Subtype") != Some(b"Image") {
                        continue;
                    }
                    out.push(PageImage {
                        rect: ctm.bbox([0.0, 0.0, 1.0, 1.0]),
                        matrix: ctm.0,
                        name: String::from_utf8_lossy(name).into_owned(),
                        object: Some(r),
                        is_form: false,
                        width: s.dict.int(b"Width").unwrap_or(0).max(0) as u32,
                        height: s.dict.int(b"Height").unwrap_or(0).max(0) as u32,
                        stream,
                        op: i,
                        draw_matrix: ctm.0,
                    });
                }
                _ => {}
            }
        }
    }
    let p = page_of(doc, page)?;
    let res = p.dict.get(b"Resources").map(|r| doc.resolve(r)).and_then(|r| r.as_dict().cloned()).unwrap_or_default();
    let mut out = Vec::new();
    let mut st = State { ctm: Matrix::IDENTITY, stack: Vec::new() };
    let mut forms = Forms { path: Vec::new(), visits: 0 };
    for (si, (_, data)) in streams(doc, &p.dict).into_iter().enumerate() {
        walk(doc, &data, &res, &mut st, si, &mut forms, &mut out);
    }
    Ok(out)
}

/// What to do with an image.
#[derive(Clone, Debug, PartialEq)]
pub enum ImageChange {
    /// Apply a user-space transform after its placement (move, resize, rotate, flip).
    Transform([f64; 6]),
    /// Draw this image XObject instead, in the same place.
    Replace(ObjRef),
    /// Keep only the part inside this user-space box (PDF: Crop Image). The rest is
    /// clipped, not discarded, and the clip moves, turns and flips with later edits.
    Crop([f64; 4]),
    Delete,
}

/// The user-space transform that maps box `from` onto box `to` (move and resize).
pub fn rect_to_rect(from: [f64; 4], to: [f64; 4]) -> [f64; 6] {
    let (fw, fh) = ((from[2] - from[0]).max(1e-6), (from[3] - from[1]).max(1e-6));
    let (sx, sy) = ((to[2] - to[0]) / fw, (to[3] - to[1]) / fh);
    [sx, 0.0, 0.0, sy, to[0] - from[0] * sx, to[1] - from[1] * sy]
}

/// Turn the image a quarter turn clockwise (`quarters` = 1, 2, 3) about its centre, or flip it.
pub fn turn_about_centre(rect: [f64; 4], quarters: i32, flip_h: bool, flip_v: bool) -> [f64; 6] {
    let (cx, cy) = ((rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0);
    let (a, b, c, d) = match quarters.rem_euclid(4) {
        1 => (0.0, -1.0, 1.0, 0.0),
        2 => (-1.0, 0.0, 0.0, -1.0),
        3 => (0.0, 1.0, -1.0, 0.0),
        _ => (1.0, 0.0, 0.0, 1.0),
    };
    let (fx, fy) = (if flip_h { -1.0 } else { 1.0 }, if flip_v { -1.0 } else { 1.0 });
    let (a, b, c, d) = (a * fx, b * fy, c * fx, d * fy);
    // Translate the centre to the origin, turn, translate back.
    [a, b, c, d, cx - a * cx - c * cy, cy - b * cx - d * cy]
}

/// The XObject name the last `Do` in `ops` paints.
fn op_name(ops: &[Op]) -> Option<String> {
    ops.iter().rev().find(|o| o.op.as_slice() == b"Do").and_then(|o| o.name(0)).map(|n| String::from_utf8_lossy(n).into_owned())
}

/// The crop wrapper page resource `name` is, if it is one: its dictionary, content and resources.
fn crop_wrapper(doc: &Document, page: &Dict, name: &str) -> Option<(Dict, Vec<Op>, Dict)> {
    let resource = xobjects(doc, page).get(name.as_bytes()).cloned()?;
    let obj = doc.resolve(&resource);
    let Object::Stream(s) = &*obj else { return None };
    cropped_image(doc, &s.dict)?;
    let res = s.dict.get(b"Resources").map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default();
    Some((s.dict.clone(), parse(&s.decoded().ok()?).ops, res))
}

/// The page's resources with `target` added under `name`.
fn add_xobject_ref(doc: &Document, page: &Dict, name: &str, target: ObjRef) -> (Dict, String) {
    let mut res = page.get(b"Resources").map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default();
    let mut xo = res.get(b"XObject").map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default();
    xo.set(name.as_bytes().to_vec(), Object::Ref(target));
    res.set(b"XObject".to_vec(), Object::Dict(xo));
    (res, name.to_string())
}

/// Add `object` to the document and the page's resources as `{prefix}{number}`.
fn add_xobject(doc: &mut Document, page: &Dict, prefix: &str, object: Object) -> (Dict, String) {
    let r = doc.add(object);
    add_xobject_ref(doc, page, &format!("{prefix}{}", r.num), r)
}

/// Change image `index` (from [`page_images`]) on `page`.
pub fn change_image(doc: &mut Document, page: usize, index: usize, change: &ImageChange) -> Result<(), EditError> {
    let images = page_images(doc, page)?;
    let img = images.get(index).cloned().ok_or_else(|| EditError::Invalid(format!("page {} has no image {}", page + 1, index + 1)))?;
    let p = page_of(doc, page)?;
    let all = streams(doc, &p.dict);
    let (obj, data) = all.get(img.stream).cloned().ok_or_else(|| EditError::Invalid("the image stream is missing".into()))?;
    let mut ops = parse(&data).ops;
    let original = ops.get(img.op).cloned().ok_or_else(|| EditError::Invalid("the image operation is missing".into()))?;
    let mut resources = None;
    let replacement: Vec<Op> = match change {
        ImageChange::Delete => Vec::new(),
        ImageChange::Transform(t) => {
            if !t.iter().all(|v| v.is_finite()) {
                return Err(EditError::Invalid("the image transform must be finite".into()));
            }
            // New CTM = placement × T; as a `cm` before the Do: M = P · T · P⁻¹.
            let pm = Matrix(img.draw_matrix);
            let inv = pm.invert().ok_or_else(|| EditError::Invalid("the image has no area".into()))?;
            let m = pm.then(&Matrix(*t)).then(&inv);
            if !m.0.iter().all(|v| v.is_finite()) {
                return Err(EditError::Invalid("the image transform is too large".into()));
            }
            vec![Op::new("q", vec![]), Op::new("cm", m.0.iter().map(|v| pdfcraft_content::num(*v)).collect()), original, Op::new("Q", vec![])]
        }
        ImageChange::Crop(keep) => {
            let [x0, y0, x1, y1] = [keep[0].max(img.rect[0]), keep[1].max(img.rect[1]), keep[2].min(img.rect[2]), keep[3].min(img.rect[3])];
            if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) || x1 - x0 < 0.01 || y1 - y0 < 0.01 {
                return Err(EditError::Invalid("the crop must keep part of the image".into()));
            }
            // The kept box in the space the `Do` draws in (the wrapper's own space, as its
            // Matrix is the identity), so it follows any later transform of the wrapper.
            let inv = Matrix(img.draw_matrix).invert().ok_or_else(|| EditError::Invalid("the image has no area".into()))?;
            let corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].map(|(x, y)| inv.apply(x, y));
            if !corners.iter().all(|(x, y)| x.is_finite() && y.is_finite()) {
                return Err(EditError::Invalid("the image transform is too large".into()));
            }
            let fold = |f: fn(f64, f64) -> f64, k: fn(&(f64, f64)) -> f64| corners.iter().map(k).fold(k(&corners[0]), f);
            let bbox = [fold(f64::min, |c| c.0), fold(f64::min, |c| c.1), fold(f64::max, |c| c.0), fold(f64::max, |c| c.1)];
            let mut path = Vec::new();
            for (i, (x, y)) in corners.iter().enumerate() {
                path.push(Op::new(if i == 0 { "m" } else { "l" }, vec![pdfcraft_content::num(*x), pdfcraft_content::num(*y)]));
            }
            path.extend([Op::new("h", vec![]), Op::new("W", vec![]), Op::new("n", vec![])]);
            // Cropping a crop again adds its clip to the existing wrapper's content, so there
            // is only ever one wrapper around the bitmap.
            let (body, inner_res) = match crop_wrapper(doc, &p.dict, &img.name) {
                Some((_, ops, res)) => (ops, res),
                None => {
                    let target = xobjects(doc, &p.dict)
                        .get(img.name.as_bytes())
                        .cloned()
                        .ok_or_else(|| EditError::Invalid("the image resource is missing".into()))?;
                    let mut xo = Dict::new();
                    xo.set(img.name.clone().into_bytes(), target);
                    let mut res = Dict::new();
                    res.set(b"XObject".to_vec(), Object::Dict(xo));
                    (vec![original], res)
                }
            };
            path.extend(body);
            let mut form = Dict::new();
            form.set(b"Type".to_vec(), Object::name("XObject"));
            form.set(b"Subtype".to_vec(), Object::name("Form"));
            form.set(b"BBox".to_vec(), Object::Array(bbox.iter().map(|v| pdfcraft_content::num(*v)).collect()));
            form.set(b"Resources".to_vec(), Object::Dict(inner_res));
            if let Some(bitmap) = img.object.filter(|_| !img.is_form) {
                form.set(CROP_OF.to_vec(), Object::Ref(bitmap));
            }
            let (res, name) = add_xobject(doc, &p.dict, "PCCrop", Object::Stream(Stream::flate(form, &serialize_ops(&path))));
            resources = Some(res);
            vec![Op::new("Do", vec![Object::name(&name)])]
        }
        ImageChange::Replace(r) => {
            if img.is_form {
                return Err(EditError::Invalid(
                    "grouped Form artwork can be moved, resized, rotated, flipped or deleted; replacing it with a bitmap is not supported".into(),
                ));
            }
            // A cropped bitmap keeps its crop: a copy of the wrapper draws the new bitmap.
            let (res, name) = match crop_wrapper(doc, &p.dict, &img.name) {
                Some((mut form, ops, mut inner_res)) => {
                    let inner = op_name(&ops).ok_or_else(|| EditError::Invalid("the cropped image is missing".into()))?;
                    let mut xo = inner_res.get(b"XObject").map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default();
                    xo.set(inner.into_bytes(), Object::Ref(*r));
                    inner_res.set(b"XObject".to_vec(), Object::Dict(xo));
                    form.set(b"Resources".to_vec(), Object::Dict(inner_res));
                    form.set(CROP_OF.to_vec(), Object::Ref(*r));
                    form.remove(b"Length");
                    form.remove(b"Filter");
                    form.remove(b"DecodeParms");
                    add_xobject(doc, &p.dict, "PCCrop", Object::Stream(Stream::flate(form, &serialize_ops(&ops))))
                }
                None => add_xobject_ref(doc, &p.dict, &format!("PCImg{}", r.num), *r),
            };
            resources = Some(res);
            vec![Op::new("Do", vec![Object::name(&name)])]
        }
    };
    ops.splice(img.op..=img.op, replacement);
    let mut dict = match &*doc.resolve(&obj) {
        Object::Stream(s) => s.dict.clone(),
        _ => Dict::new(),
    };
    dict.remove(b"Length");
    let new = doc.add(Object::Stream(Stream::flate(dict, &serialize_ops(&ops))));
    let contents: Vec<Object> = all.iter().enumerate().map(|(i, (o, _))| if i == img.stream { Object::Ref(new) } else { o.clone() }).collect();
    doc.update_dict(p.obj, |d| {
        d.set(b"Contents".to_vec(), if let [one] = contents.as_slice() { one.clone() } else { Object::Array(contents) });
        if let Some(res) = resources {
            d.set(b"Resources".to_vec(), Object::Dict(res));
        }
    })?;
    Ok(())
}

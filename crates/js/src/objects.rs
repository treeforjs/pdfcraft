//! Comments (`getAnnots`, `getAnnot`, `addAnnot` and Annotation objects), layers (`getOCGs`
//! and OCG objects) and the dialogs that wait for an answer (`app.alert` with buttons,
//! `app.response`).

use super::*;

/// Most comments one script can add.
const MAX_ADDED_ANNOTS: usize = 1000;
/// Most numbers in a `quads` or `points` array.
const MAX_COORDINATES: usize = 8 * 1024;

// ── comments ────────────────────────────────────────────────────────────────────────────────

/// Which comment an Annotation object stands for: one of the document's ([`DocInfo::annots`])
/// or one the script added.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Slot {
    Existing(usize),
    Added(usize),
}

fn key(slot: Slot) -> JsString {
    JsString::from(match slot {
        Slot::Existing(i) => format!("e{i}"),
        Slot::Added(i) => format!("a{i}"),
    })
}

fn slot(key: &JsString) -> JsResult<Slot> {
    let k = key.to_std_string_escaped();
    let n = k.get(1..).and_then(|n| n.parse().ok());
    match (k.get(..1), n) {
        (Some("e"), Some(i)) => Ok(Slot::Existing(i)),
        (Some("a"), Some(i)) => Ok(Slot::Added(i)),
        _ => Err(error("not a comment")),
    }
}

/// A comment as an Annotation object reads it.
struct View {
    kind: String,
    page: usize,
    name: String,
    author: String,
    contents: String,
    rect: [f64; 4],
    locked: bool,
}

fn view(ctx: &Context, slot: Slot) -> JsResult<View> {
    let h = host(ctx)?.borrow();
    let gone = || error("the comment was deleted");
    match slot {
        Slot::Existing(i) => {
            let a = h.annots.get(i).filter(|_| !h.destroyed.contains(&i)).ok_or_else(gone)?;
            Ok(View {
                kind: a.kind.clone(),
                page: a.page,
                name: a.name.clone(),
                author: a.author.clone(),
                contents: a.contents.clone(),
                rect: a.rect,
                locked: a.locked,
            })
        }
        Slot::Added(i) => {
            let a = h.added.get(i).and_then(Option::as_ref).ok_or_else(gone)?;
            Ok(View {
                kind: a.kind.clone(),
                page: a.page,
                name: String::new(),
                author: a.author.clone(),
                contents: a.contents.clone(),
                rect: a.rect,
                locked: false,
            })
        }
    }
}

/// Change a comment's author or text (`contents`: which).
fn set_text(ctx: &Context, slot: Slot, contents: bool, value: String) -> JsResult<()> {
    let mut h = host(ctx)?.borrow_mut();
    let gone = || error("the comment was deleted");
    let (author, text) = match slot {
        Slot::Existing(i) => {
            if h.destroyed.contains(&i) {
                return Err(gone());
            }
            let a = h.annots.get_mut(i).ok_or_else(gone)?;
            if a.locked {
                return Err(error("the comment is locked"));
            }
            (&mut a.author, &mut a.contents)
        }
        Slot::Added(i) => {
            let a = h.added.get_mut(i).and_then(Option::as_mut).ok_or_else(gone)?;
            (&mut a.author, &mut a.contents)
        }
    };
    *(if contents { text } else { author }) = value;
    Ok(())
}

fn annot_object(ctx: &mut Context, which: Slot) -> JsObject {
    let k = key(which);
    macro_rules! get {
        (|$v:ident, $c:ident| $body:expr) => {{
            fn get(_: &JsValue, _: &[JsValue], k: &JsString, $c: &mut Context) -> JsResult<JsValue> {
                let $v = view($c, slot(k)?)?;
                Ok($body)
            }
            function(ctx, NativeFunction::from_copy_closure_with_captures(get, k.clone()))
        }};
    }
    macro_rules! set {
        ($contents:expr) => {{
            fn set(_: &JsValue, args: &[JsValue], k: &JsString, c: &mut Context) -> JsResult<JsValue> {
                let v = text(&arg(args, 0), c)?;
                set_text(c, slot(k)?, $contents, v)?;
                Ok(JsValue::undefined())
            }
            function(ctx, NativeFunction::from_copy_closure_with_captures(set, k.clone()))
        }};
    }
    let kind = get!(|v, _c| s(&v.kind));
    let page = get!(|v, _c| JsValue::from(v.page as f64));
    let name = get!(|v, _c| s(&v.name));
    let author_get = get!(|v, _c| s(&v.author));
    let author_set = set!(false);
    let contents_get = get!(|v, _c| s(&v.contents));
    let contents_set = set!(true);
    let rect = get!(|v, c| JsArray::from_iter(v.rect.map(JsValue::from), c).into());
    let read_only = get!(|v, _c| JsValue::from(v.locked));
    fn destroy(_: &JsValue, _: &[JsValue], k: &JsString, c: &mut Context) -> JsResult<JsValue> {
        let slot = slot(k)?;
        if view(c, slot)?.locked {
            return Err(error("the comment is locked"));
        }
        let mut h = host(c)?.borrow_mut();
        match slot {
            Slot::Existing(i) => {
                h.destroyed.insert(i);
            }
            Slot::Added(i) => {
                if let Some(a) = h.added.get_mut(i) {
                    *a = None;
                }
            }
        }
        Ok(JsValue::undefined())
    }
    let destroy = NativeFunction::from_copy_closure_with_captures(destroy, k.clone());
    let rw = Attribute::CONFIGURABLE | Attribute::ENUMERABLE;
    ObjectInitializer::new(ctx)
        .accessor(js_string!("type"), Some(kind), None, rw)
        .accessor(js_string!("page"), Some(page), None, rw)
        .accessor(js_string!("name"), Some(name), None, rw)
        .accessor(js_string!("author"), Some(author_get), Some(author_set), rw)
        .accessor(js_string!("contents"), Some(contents_get), Some(contents_set), rw)
        .accessor(js_string!("rect"), Some(rect), None, rw)
        .accessor(js_string!("readOnly"), Some(read_only), None, rw)
        .function(destroy, js_string!("destroy"), 0)
        .build()
}

/// A page number argument (0-based), checked against the document.
fn page_arg(v: &JsValue, ctx: &mut Context) -> JsResult<usize> {
    let n = v.to_number(ctx)?;
    let pages = host(ctx)?.borrow().doc.num_pages;
    if !n.is_finite() || n < 0.0 || n >= pages as f64 {
        return Err(error(format!("there is no page {n} (pages are numbered from 0)")));
    }
    Ok(n as usize)
}

/// `this.getAnnots({ nPage })` or `this.getAnnots(nPage)`: the comments (on one page), or
/// null when there are none.
pub(super) fn get_annots(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let a = arg(args, 0);
    let page = match a.as_object() {
        Some(o) if !o.is_array() => {
            let p = o.get(js_string!("nPage"), ctx)?;
            if p.is_undefined() { None } else { Some(page_arg(&p, ctx)?) }
        }
        _ if a.is_undefined() || a.is_null() => None,
        _ => Some(page_arg(&a, ctx)?),
    };
    let slots: Vec<Slot> = {
        let h = host(ctx)?.borrow();
        let on = |p: usize| page.is_none_or(|q| q == p);
        let existing = h.annots.iter().enumerate().filter(|(i, a)| !h.destroyed.contains(i) && on(a.page)).map(|(i, _)| Slot::Existing(i));
        let added = h.added.iter().enumerate().filter(|(_, a)| a.as_ref().is_some_and(|a| on(a.page))).map(|(i, _)| Slot::Added(i));
        existing.chain(added).collect()
    };
    if slots.is_empty() {
        return Ok(JsValue::null());
    }
    let objects: Vec<JsValue> = slots.into_iter().map(|sl| annot_object(ctx, sl).into()).collect();
    Ok(JsArray::from_iter(objects, ctx).into())
}

/// `this.getAnnot(nPage, cName)`: the comment named `cName` (`/NM`) on the page, or null.
pub(super) fn get_annot(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let page = page_arg(&arg(args, 0), ctx)?;
    let name = text(&arg(args, 1), ctx)?;
    let found = {
        let h = host(ctx)?.borrow();
        h.annots.iter().enumerate().position(|(i, a)| !h.destroyed.contains(&i) && a.page == page && !name.is_empty() && a.name == name)
    };
    Ok(found.map_or(JsValue::null(), |i| annot_object(ctx, Slot::Existing(i)).into()))
}

/// The numbers in array `v` (at most [`MAX_COORDINATES`]), each finite. A line's points come
/// as pairs (`[[x, y], [x, y]]`): one level of nested arrays is flattened, deeper ones refused.
fn numbers(v: &JsValue, what: &str, ctx: &mut Context) -> JsResult<Vec<f64>> {
    let bad = || error(format!("{what} must be an array of numbers"));
    let mut out = Vec::new();
    let push = |x: JsValue, nested: bool, out: &mut Vec<f64>, ctx: &mut Context| -> JsResult<Option<JsArray>> {
        if let Some(inner) = x.as_object().filter(|o| o.is_array()) {
            return if nested { Err(bad()) } else { Ok(Some(JsArray::from_object(inner.clone())?)) };
        }
        let x = x.to_number(ctx)?;
        if !x.is_finite() || out.len() >= MAX_COORDINATES {
            return Err(if x.is_finite() { error(format!("{what} has too many numbers")) } else { bad() });
        }
        out.push(x);
        Ok(None)
    };
    let a = JsArray::from_object(v.as_object().filter(|o| o.is_array()).ok_or_else(bad)?.clone())?;
    let n = a.length(ctx)?;
    if n as usize > MAX_COORDINATES {
        return Err(error(format!("{what} has too many numbers")));
    }
    for i in 0..n {
        let x = a.get(i, ctx)?;
        if let Some(pair) = push(x, false, &mut out, ctx)? {
            let m = pair.length(ctx)?;
            if m as usize > MAX_COORDINATES {
                return Err(error(format!("{what} has too many numbers")));
            }
            for j in 0..m {
                let y = pair.get(j, ctx)?;
                push(y, true, &mut out, ctx)?;
            }
        }
    }
    Ok(out)
}

/// An Acrobat colour array as RGB (`None` for transparent).
fn rgb(v: &JsValue, ctx: &mut Context) -> JsResult<Option<[f64; 3]>> {
    let parts = strings(v, ctx)?;
    let n = |i: usize| parts.get(i).and_then(|p| p.parse::<f64>().ok()).filter(|x| x.is_finite()).unwrap_or(0.0).clamp(0.0, 1.0);
    Ok(match parts.first().map(String::as_str) {
        Some("RGB") => Some([n(1), n(2), n(3)]),
        Some("G") => Some([n(1); 3]),
        Some("CMYK") => {
            let k = 1.0 - n(4);
            Some([(1.0 - n(1)) * k, (1.0 - n(2)) * k, (1.0 - n(3)) * k])
        }
        Some("T") | None => None,
        Some(other) => return Err(error(format!("unknown colour space {other}"))),
    })
}

/// `this.addAnnot({ type, page, rect, point, points, quads, contents, author, strokeColor,
/// textSize })`: add a sticky note, text box, rectangle, oval, line or text markup.
pub(super) fn add_annot(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let props = arg(args, 0);
    let o = props.as_object().filter(|o| !o.is_array()).ok_or_else(|| error("addAnnot takes an object of properties"))?.clone();
    let kind = {
        let t = o.get(js_string!("type"), ctx)?;
        text(&t, ctx)?
    };
    if !ADDABLE_ANNOTS.contains(&kind.as_str()) {
        return Err(error(format!("addAnnot can't add a {kind:?} comment (it can add {})", ADDABLE_ANNOTS.join(", "))));
    }
    let page = {
        let p = o.get(js_string!("page"), ctx)?;
        if p.is_undefined() { page_arg(&JsValue::from(0), ctx)? } else { page_arg(&p, ctx)? }
    };
    let get_numbers = |name: &str, ctx: &mut Context| -> JsResult<Option<Vec<f64>>> {
        let v = o.get(JsString::from(name), ctx)?;
        if v.is_undefined() || v.is_null() { Ok(None) } else { numbers(&v, name, ctx).map(Some) }
    };
    let rect = match get_numbers("rect", ctx)? {
        Some(r) => {
            let [x0, y0, x1, y1] = r[..] else { return Err(error("rect must be [x0, y0, x1, y1]")) };
            Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)])
        }
        None => None,
    };
    let point = match get_numbers("point", ctx)? {
        Some(p) => {
            let [x, y] = p[..] else { return Err(error("point must be [x, y]")) };
            Some([x, y])
        }
        None => None,
    };
    let points = get_numbers("points", ctx)?.unwrap_or_default();
    let quads = get_numbers("quads", ctx)?.unwrap_or_default();
    let mut a = NewAnnot { kind: kind.clone(), page, ..Default::default() };
    match kind.as_str() {
        "Text" => {
            // The note's icon sits with its top-left corner at the point.
            let [x, y] = point.or(rect.map(|r| [r[0], r[3]])).ok_or_else(|| error("a Text comment needs a point or a rect"))?;
            a.rect = [x, y - 20.0, x + 20.0, y];
        }
        "Line" => {
            let [x0, y0, x1, y1] = points[..] else { return Err(error("a Line needs points: [[x1, y1], [x2, y2]]")) };
            a.points = vec![[x0, y0], [x1, y1]];
            a.rect = [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
        }
        "Highlight" | "Underline" | "StrikeOut" | "Squiggly" => {
            if quads.is_empty() || !quads.len().is_multiple_of(8) {
                return Err(error(format!("a {kind} needs quads: 8 numbers for each marked area")));
            }
            a.quads = quads.as_chunks::<8>().0.to_vec();
            let (xs, ys): (Vec<f64>, Vec<f64>) = quads.as_chunks::<2>().0.iter().map(|[x, y]| (*x, *y)).unzip();
            let lo = |v: &[f64]| v.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = |v: &[f64]| v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            a.rect = [lo(&xs), lo(&ys), hi(&xs), hi(&ys)];
        }
        _ => {
            let r = rect.ok_or_else(|| error(format!("a {kind} needs a rect")))?;
            if r[2] - r[0] < 1.0 || r[3] - r[1] < 1.0 {
                return Err(error(format!("a {kind}'s rect must be at least 1 point wide and high")));
            }
            a.rect = r;
        }
    }
    let get_text = |name: &str, ctx: &mut Context| -> JsResult<String> {
        let v = o.get(JsString::from(name), ctx)?;
        text(&v, ctx)
    };
    a.contents = get_text("contents", ctx)?;
    a.author = get_text("author", ctx)?;
    let colour = o.get(js_string!("strokeColor"), ctx)?;
    if !colour.is_undefined() {
        a.color = rgb(&colour, ctx)?;
    }
    let size = o.get(js_string!("textSize"), ctx)?;
    if !size.is_undefined() {
        let n = size.to_number(ctx)?;
        if !(1.0..=1000.0).contains(&n) {
            return Err(error("textSize must be between 1 and 1000"));
        }
        a.text_size = Some(n);
    }
    let i = {
        let mut h = host(ctx)?.borrow_mut();
        if h.added.len() >= MAX_ADDED_ANNOTS {
            return Err(error(format!("a script can add at most {MAX_ADDED_ANNOTS} comments")));
        }
        h.added.push(Some(a));
        h.added.len() - 1
    };
    Ok(annot_object(ctx, Slot::Added(i)).into())
}

/// What the script did to the comments: changed text and authors, then deletions (on each page
/// from the last index down, so each still names the right comment), then additions.
pub(super) fn annot_changes(
    before: &[AnnotState],
    after: &[AnnotState],
    destroyed: &BTreeSet<usize>,
    added: &[Option<NewAnnot>],
) -> Vec<AnnotChange> {
    let mut out: Vec<AnnotChange> = before
        .iter()
        .zip(after)
        .enumerate()
        .filter(|(i, _)| !destroyed.contains(i))
        .filter_map(|(_, (b, a))| {
            let contents = (a.contents != b.contents).then(|| a.contents.clone());
            let author = (a.author != b.author).then(|| a.author.clone());
            (contents.is_some() || author.is_some()).then_some(AnnotChange::Set { page: b.page, index: b.index, contents, author })
        })
        .collect();
    let mut gone: Vec<(usize, usize)> = destroyed.iter().filter_map(|i| before.get(*i)).map(|a| (a.page, a.index)).collect();
    gone.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    gone.dedup();
    out.extend(gone.into_iter().map(|(page, index)| AnnotChange::Delete { page, index }));
    out.extend(added.iter().flatten().cloned().map(AnnotChange::Add));
    out
}

// ── layers ──────────────────────────────────────────────────────────────────────────────────

fn layer(ctx: &Context, k: &JsString) -> JsResult<(usize, LayerState)> {
    let i: usize = k.to_std_string_escaped().parse().map_err(|_| error("not a layer"))?;
    let l = host(ctx)?.borrow().doc.layers.get(i).cloned().ok_or_else(|| error("not a layer"))?;
    Ok((i, l))
}

/// `this.getOCGs()`: the document's layers, or null when it has none. (Every layer, whatever
/// page is asked for.)
pub(super) fn get_ocgs(_: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let n = host(ctx)?.borrow().doc.layers.len();
    if n == 0 {
        return Ok(JsValue::null());
    }
    fn name(_: &JsValue, _: &[JsValue], k: &JsString, c: &mut Context) -> JsResult<JsValue> {
        Ok(s(&layer(c, k)?.1.name))
    }
    fn state_get(_: &JsValue, _: &[JsValue], k: &JsString, c: &mut Context) -> JsResult<JsValue> {
        Ok(JsValue::from(layer(c, k)?.1.on))
    }
    fn state_set(_: &JsValue, args: &[JsValue], k: &JsString, c: &mut Context) -> JsResult<JsValue> {
        let on = arg(args, 0).to_boolean();
        let (index, l) = layer(c, k)?;
        if l.on != on {
            let mut h = host(c)?.borrow_mut();
            if let Some(l) = h.doc.layers.get_mut(index) {
                l.on = on;
            }
            h.requests.push(Request::SetLayer { index, on });
        }
        Ok(JsValue::undefined())
    }
    let rw = Attribute::CONFIGURABLE | Attribute::ENUMERABLE;
    let mut objects = Vec::new();
    for i in 0..n {
        let k = JsString::from(i.to_string());
        let name = function(ctx, NativeFunction::from_copy_closure_with_captures(name, k.clone()));
        let get = function(ctx, NativeFunction::from_copy_closure_with_captures(state_get, k.clone()));
        let set = function(ctx, NativeFunction::from_copy_closure_with_captures(state_set, k.clone()));
        let o = ObjectInitializer::new(ctx)
            .accessor(js_string!("name"), Some(name), None, rw)
            .accessor(js_string!("state"), Some(get), Some(set), rw)
            .build();
        objects.push(JsValue::from(o));
    }
    Ok(JsArray::from_iter(objects, ctx).into())
}

// ── dialogs ─────────────────────────────────────────────────────────────────────────────────

/// The answer to `q`: the next one given, `None` when nobody is asked (not interactive), or an
/// error that stops the script until the user answers.
fn ask(ctx: &Context, q: Question) -> JsResult<Option<Answer>> {
    let mut h = host(ctx)?.borrow_mut();
    let waiting = || error("the script is waiting for an answer");
    if h.pending.is_some() {
        return Err(waiting());
    }
    if !h.dialogs.interactive {
        return Ok(None);
    }
    if let Some(a) = h.dialogs.answers.get(h.answered).cloned() {
        h.answered += 1;
        return Ok(Some(a));
    }
    h.pending = Some(q);
    Err(waiting())
}

/// The named properties of an options object (`app.alert({ cMsg, … })`), or the positional
/// arguments.
fn options(args: &[JsValue], names: &[&str], ctx: &mut Context) -> JsResult<Vec<JsValue>> {
    match arg(args, 0).as_object() {
        Some(o) if !o.is_array() && !o.is_callable() => names.iter().map(|n| o.get(JsString::from(*n), ctx)).collect(),
        _ => Ok((0..names.len()).map(|i| arg(args, i)).collect()),
    }
}

fn int(v: &JsValue, ctx: &mut Context) -> JsResult<i32> {
    if v.is_undefined() || v.is_null() {
        return Ok(0);
    }
    let n = v.to_number(ctx)?;
    Ok(if n.is_finite() { n.clamp(-1.0, 100.0) as i32 } else { 0 })
}

/// `app.alert(cMsg, nIcon, nType, cTitle)`: returns the button pressed (1 OK, 2 Cancel, 3 No,
/// 4 Yes). Without buttons to choose from it is listed in [`Outcome::alerts`]; with them it is
/// a [`Question`].
pub(super) fn alert(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let o = options(args, &["cMsg", "nIcon", "nType", "cTitle"], ctx)?;
    let get = |i: usize| o.get(i).cloned().unwrap_or_default();
    let message = text(&get(0), ctx)?;
    let icon = int(&get(1), ctx)?.clamp(0, 3);
    let buttons = int(&get(2), ctx)?;
    if !(1..=3).contains(&buttons) {
        host(ctx)?.borrow_mut().alerts.push(message);
        return Ok(JsValue::from(1));
    }
    let title = text(&get(3), ctx)?;
    let q = Question::Alert { message: message.clone(), title, icon, buttons };
    let offered = q.buttons();
    // Cancel, or No when there is no Cancel.
    let fallback = if buttons == 2 { 3 } else { 2 };
    Ok(JsValue::from(match ask(ctx, q)? {
        Some(Answer::Button(b)) if offered.contains(&b) => b,
        Some(_) => fallback,
        None => {
            host(ctx)?.borrow_mut().alerts.push(message);
            fallback
        }
    }))
}

/// `app.response(cQuestion, cTitle, cDefault, bPassword, cLabel)`: the text entered, or null
/// for Cancel.
pub(super) fn response(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let o = options(args, &["cQuestion", "cTitle", "cDefault", "bPassword", "cLabel"], ctx)?;
    let get = |i: usize| o.get(i).cloned().unwrap_or_default();
    let q = Question::Response {
        question: text(&get(0), ctx)?,
        title: text(&get(1), ctx)?,
        default: text(&get(2), ctx)?,
        password: get(3).to_boolean(),
        label: text(&get(4), ctx)?,
    };
    Ok(match ask(ctx, q)? {
        Some(Answer::Text(Some(t))) => s(&t),
        _ => JsValue::null(),
    })
}

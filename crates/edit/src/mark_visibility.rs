//! Watermarks shown only when printing or only on screen (Acrobat's Appearance Options: Show
//! when printing / Show when displaying on screen).
//!
//! Such a watermark's content is optional content (§8.11) of an optional content group made for
//! it: `/OC /PCWmPrint BDC … EMC` inside the mark. The group's `/Usage` says when it shows
//! (`/View /ViewState`, `/Print /PrintState`) and the default configuration applies those usages
//! automatically (`/AS` for the View and Print events), so other readers honour it; the default
//! state is what is seen on screen (a print-only group starts off). The mark's stream dictionary
//! also records `/PCShow /Print` or `/PCShow /Screen`, which is how PdfCraft's own printing drops
//! screen-only watermarks ([`without_screen_only_marks`]) without decompressing page content.

use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString};

use crate::{EditError, contents, page_list};

/// When a watermark shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MarkVisibility {
    /// On screen and in print.
    #[default]
    Always,
    /// Only when printed.
    PrintOnly,
    /// Only on screen.
    ScreenOnly,
}

impl MarkVisibility {
    /// From whether it shows when printing and on screen; neither is refused.
    pub fn from_flags(print: bool, screen: bool) -> Result<MarkVisibility, EditError> {
        match (print, screen) {
            (true, true) => Ok(MarkVisibility::Always),
            (true, false) => Ok(MarkVisibility::PrintOnly),
            (false, true) => Ok(MarkVisibility::ScreenOnly),
            (false, false) => Err(EditError::Invalid("a watermark must show when printing, on screen, or both".into())),
        }
    }

    pub fn prints(self) -> bool {
        self != MarkVisibility::ScreenOnly
    }

    pub fn on_screen(self) -> bool {
        self != MarkVisibility::PrintOnly
    }

    /// The stream dictionary's `/PCShow` value and the page resource name of the group.
    pub(crate) fn names(self) -> Option<(&'static str, &'static str)> {
        match self {
            MarkVisibility::Always => None,
            MarkVisibility::PrintOnly => Some(("Print", "PCWmPrint")),
            MarkVisibility::ScreenOnly => Some(("Screen", "PCWmScreen")),
        }
    }
}

/// Most optional content groups looked through for one made before.
const MAX_GROUPS: usize = 100_000;

fn list(d: &Dict, k: &[u8], doc: &Document) -> Vec<Object> {
    d.get(k).map(|v| doc.resolve(v)).and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

fn state(k: &str, v: &str) -> Object {
    let mut d = Dict::new();
    d.set(k.as_bytes().to_vec(), Object::name(v));
    Object::Dict(d)
}

/// The group for `visibility` (made before, or new and registered in `/OCProperties`), and put it
/// in page `page`'s resources under its name.
pub(crate) fn group_on_page(doc: &mut Document, page: &pdfcraft_model::Page, visibility: MarkVisibility) -> Result<(), EditError> {
    let Some((show, name)) = visibility.names() else { return Ok(()) };
    let group = group(doc, show)?;
    let p = doc.get(page.obj).as_dict().cloned().unwrap_or_default();
    let mut res = p.get(b"Resources").map(|r| doc.resolve(r)).and_then(|r| r.as_dict().cloned()).unwrap_or_default();
    let mut props = res.get(b"Properties").map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default();
    props.set(name.as_bytes().to_vec(), Object::Ref(group));
    res.set(b"Properties".to_vec(), Object::Dict(props));
    doc.update_dict(page.obj, |d| d.set(b"Resources".to_vec(), Object::Dict(res)))?;
    Ok(())
}

fn group(doc: &mut Document, show: &str) -> Result<ObjRef, EditError> {
    let root = doc.root().ok_or_else(|| EditError::Invalid("the document has no catalog".into()))?;
    let catalog = doc.get(root).as_dict().cloned().ok_or_else(|| EditError::Invalid("the document has no catalog".into()))?;
    let (props_ref, mut props) = match catalog.get(b"OCProperties") {
        Some(Object::Ref(r)) => (Some(*r), doc.get(*r).as_dict().cloned().unwrap_or_default()),
        Some(Object::Dict(d)) => (None, d.clone()),
        _ => (None, Dict::new()),
    };
    let mut ocgs = list(&props, b"OCGs", doc);
    let ours = ocgs.iter().take(MAX_GROUPS).filter_map(Object::as_ref).find(|r| {
        let o = doc.get(*r);
        o.as_dict().is_some_and(|d| d.name(b"Type") == Some(b"OCG".as_slice()) && d.name(b"PCShow") == Some(show.as_bytes()))
    });
    if let Some(r) = ours {
        return Ok(r);
    }
    let print = show == "Print";
    let mut ocg = Dict::new();
    ocg.set(b"Type".to_vec(), Object::name("OCG"));
    ocg.set(b"Name".to_vec(), Object::String(PdfString::text(if print { "Watermark (print only)" } else { "Watermark (screen only)" })));
    ocg.set(b"PCShow".to_vec(), Object::name(show));
    let mut usage = Dict::new();
    usage.set(b"View".to_vec(), state("ViewState", if print { "OFF" } else { "ON" }));
    usage.set(b"Print".to_vec(), state("PrintState", if print { "ON" } else { "OFF" }));
    ocg.set(b"Usage".to_vec(), Object::Dict(usage));
    let r = doc.add(Object::Dict(ocg));
    ocgs.push(Object::Ref(r));
    let mut config = props.get(b"D").map(|d| doc.resolve(d)).and_then(|d| d.as_dict().cloned()).unwrap_or_default();
    let mut order = list(&config, b"Order", doc);
    order.push(Object::Ref(r));
    config.set(b"Order".to_vec(), Object::Array(order));
    // What is seen on screen: a print-only group starts off; a screen-only one on.
    if print {
        let mut off = list(&config, b"OFF", doc);
        off.push(Object::Ref(r));
        config.set(b"OFF".to_vec(), Object::Array(off));
    } else if config.name(b"BaseState") == Some(b"OFF".as_slice()) {
        let mut on = list(&config, b"ON", doc);
        on.push(Object::Ref(r));
        config.set(b"ON".to_vec(), Object::Array(on));
    }
    // Apply the usage when viewing and printing: add the group to the configuration's View and
    // Print usage applications (new ones when there are none).
    let mut apps = list(&config, b"AS", doc);
    for (event, category) in [("View", "View"), ("Print", "Print")] {
        let found = apps.iter().position(|a| {
            doc.resolve(a).as_dict().is_some_and(|d| {
                d.name(b"Event") == Some(event.as_bytes())
                    && list(d, b"Category", doc).iter().any(|c| matches!(c, Object::Name(n) if n.as_slice() == category.as_bytes()))
            })
        });
        match found {
            Some(k) => {
                let mut d = apps.get(k).map(|a| doc.resolve(a)).and_then(|a| a.as_dict().cloned()).unwrap_or_default();
                let mut groups = list(&d, b"OCGs", doc);
                groups.push(Object::Ref(r));
                d.set(b"OCGs".to_vec(), Object::Array(groups));
                if let Some(slot) = apps.get_mut(k) {
                    *slot = Object::Dict(d);
                }
            }
            None => {
                let mut d = Dict::new();
                d.set(b"Event".to_vec(), Object::name(event));
                d.set(b"OCGs".to_vec(), Object::Array(vec![Object::Ref(r)]));
                d.set(b"Category".to_vec(), Object::Array(vec![Object::name(category)]));
                apps.push(Object::Dict(d));
            }
        }
    }
    config.set(b"AS".to_vec(), Object::Array(apps));
    props.set(b"OCGs".to_vec(), Object::Array(ocgs));
    props.set(b"D".to_vec(), Object::Dict(config));
    match props_ref {
        Some(p) => doc.set(p, Object::Dict(props)),
        None => doc.update_dict(root, |c| c.set(b"OCProperties".to_vec(), Object::Dict(props)))?,
    }
    Ok(r)
}

/// When page content stream `o` is a mark shown only on screen.
fn screen_only(doc: &Document, o: &Object) -> bool {
    doc.resolve(o).as_dict().is_some_and(|d| d.name(b"PCShow") == Some(b"Screen".as_slice()))
}

/// Remove the watermarks shown only on screen from every page (the copy PdfCraft prints).
/// Returns how many were removed. Print-only watermarks need nothing: they are off only in the
/// on-screen configuration, which a print copy doesn't use.
pub fn without_screen_only_marks(doc: &mut Document) -> Result<usize, EditError> {
    let mut removed = 0usize;
    for page in page_list(doc) {
        let list = contents(doc, &page)?;
        if !list.iter().any(|o| screen_only(doc, o)) {
            continue;
        }
        let before = list.len();
        let kept: Vec<Object> = list.into_iter().filter(|o| !screen_only(doc, o)).collect();
        removed = removed.saturating_add(before.saturating_sub(kept.len()));
        doc.update_dict(page.obj, |d| d.set(b"Contents".to_vec(), Object::Array(kept)))?;
    }
    Ok(removed)
}

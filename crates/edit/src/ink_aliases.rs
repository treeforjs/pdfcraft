//! Ink Manager ▸ ink aliases, made permanent: every Separation and DeviceN colour space that
//! names an aliased colorant names the ink it is aliased to instead, so it prints on that plate.
//!
//! The colour spaces keep their own alternate space and tint transform, so the document looks
//! the same in a composite viewer and only the plate changes. DeviceN `Colorants` entries
//! (attributes) are renamed with them; an entry for an ink the dictionary already has is dropped.
//! A DeviceN colour space in which aliasing would name one ink twice (PDF forbids repeated
//! colorants) is left as it was and reported. `All`, `None` and the process inks are never
//! renamed, and nothing is renamed *into* `All` or `None`.
//!
//! Every current object of the document is looked at (colour spaces can sit in page and form
//! resources, images, shadings, patterns, annotation appearances, Type 3 fonts); streams keep
//! their data, only their dictionaries change. Walks are depth- and size-limited.

use std::collections::{BTreeMap, HashSet};

use pdfcraft_cos::{Dict, Document, ObjRef, Object};

use crate::EditError;

/// Aliases accepted at once.
pub const MAX_ALIASES: usize = 256;
/// The longest colorant name accepted (PDF names are limited to 127 bytes).
pub const MAX_NAME: usize = 127;
const MAX_DEPTH: usize = 32;
/// Array elements and dictionary entries visited per object at most.
const MAX_NODES: usize = 1_000_000;
const PROCESS: [&[u8]; 4] = [b"Cyan", b"Magenta", b"Yellow", b"Black"];

/// What [`alias_inks`] changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AliasReport {
    /// Colour spaces (and `Colorants` entries) renamed, per original colorant.
    pub renamed: BTreeMap<String, usize>,
    /// Objects rewritten.
    pub objects: usize,
    /// What was left as it was, and why.
    pub skipped: Vec<String>,
}

impl AliasReport {
    pub fn changed(&self) -> bool {
        self.objects > 0
    }
}

/// Check `aliases` (colorant → the ink it prints on): no chains, no self-aliases, nothing to or
/// from `All`/`None`, no process ink aliased, names 1–127 bytes.
pub fn check_aliases(aliases: &BTreeMap<Vec<u8>, Vec<u8>>) -> Result<(), EditError> {
    if aliases.len() > MAX_ALIASES {
        return Err(EditError::Invalid(format!("at most {MAX_ALIASES} ink aliases")));
    }
    let show = |n: &[u8]| String::from_utf8_lossy(n).into_owned();
    for (from, to) in aliases {
        for n in [from, to] {
            if n.is_empty() || n.len() > MAX_NAME {
                return Err(EditError::Invalid(format!("ink names are 1 to {MAX_NAME} bytes ({:?})", show(n))));
            }
            if n.as_slice() == b"All" || n.as_slice() == b"None" {
                return Err(EditError::Invalid("All (registration) and None are not inks and can't be aliased".into()));
            }
        }
        if PROCESS.contains(&from.as_slice()) {
            return Err(EditError::Invalid(format!("{} is a process ink; only spot inks can be aliased", show(from))));
        }
        if from == to {
            return Err(EditError::Invalid(format!("{} can't be aliased to itself", show(from))));
        }
        if aliases.contains_key(to) {
            return Err(EditError::Invalid(format!("{} is itself aliased: alias {} to the ink it finally prints on", show(to), show(from))));
        }
    }
    Ok(())
}

struct Walk<'a> {
    aliases: &'a BTreeMap<Vec<u8>, Vec<u8>>,
    report: AliasReport,
    skipped: std::collections::BTreeSet<&'static str>,
    nodes: usize,
}

impl Walk<'_> {
    fn count(&mut self, from: &[u8]) {
        let n = self.report.renamed.entry(String::from_utf8_lossy(from).into_owned()).or_insert(0);
        *n = n.saturating_add(1);
    }

    fn object(&mut self, o: &mut Object, depth: usize) -> bool {
        if depth > MAX_DEPTH {
            self.skipped.insert("objects nested too deeply to look at");
            return false;
        }
        self.nodes = self.nodes.saturating_add(1);
        if self.nodes > MAX_NODES {
            self.skipped.insert("objects too large to look at");
            return false;
        }
        match o {
            Object::Array(a) => self.array(a, depth),
            Object::Dict(d) => self.dict(d, depth),
            Object::Stream(s) => self.dict(&mut s.dict, depth),
            _ => false,
        }
    }

    fn array(&mut self, a: &mut [Object], depth: usize) -> bool {
        let family = a.first().and_then(Object::as_name).map(<[u8]>::to_vec);
        let mut changed = false;
        match family.as_deref() {
            Some(b"Separation") => {
                if let Some(slot) = a.get_mut(1)
                    && let Some(name) = slot.as_name().map(<[u8]>::to_vec)
                    && let Some(to) = self.aliases.get(&name)
                {
                    *slot = Object::Name(to.clone());
                    self.count(&name);
                    changed = true;
                }
            }
            Some(b"DeviceN") => {
                if let Some(Object::Array(names)) = a.get_mut(1) {
                    let old: Vec<Vec<u8>> = names.iter().map(|n| n.as_name().map(<[u8]>::to_vec).unwrap_or_default()).collect();
                    if old.iter().any(|n| self.aliases.contains_key(n)) {
                        let new: Vec<Vec<u8>> = old.iter().map(|n| self.aliases.get(n).cloned().unwrap_or_else(|| n.clone())).collect();
                        let mut seen = HashSet::new();
                        if new.iter().filter(|n| n.as_slice() != b"None").all(|n| seen.insert(n.clone())) {
                            for (slot, (from, to)) in names.iter_mut().zip(old.iter().zip(&new)) {
                                if from != to {
                                    *slot = Object::Name(to.clone());
                                    self.count(from);
                                }
                            }
                            changed = true;
                        } else {
                            self.skipped.insert("DeviceN colour spaces in which an alias would name one ink twice (left as they were)");
                        }
                    }
                }
            }
            _ => {}
        }
        for item in a.iter_mut() {
            changed |= self.object(item, depth + 1);
        }
        changed
    }

    fn dict(&mut self, d: &mut Dict, depth: usize) -> bool {
        let mut changed = false;
        // DeviceN attributes: rename the Colorants entries of aliased inks.
        if let Some(Object::Dict(colorants)) = d.get_mut(b"Colorants") {
            let keys: Vec<Vec<u8>> = colorants.iter().map(|(k, _)| k.to_vec()).filter(|k| self.aliases.contains_key(k)).collect();
            for key in keys {
                let Some(to) = self.aliases.get(&key).cloned() else { continue };
                let Some(value) = colorants.remove(&key) else { continue };
                if !colorants.contains(&to) {
                    colorants.set(to, value);
                }
                self.count(&key);
                changed = true;
            }
        }
        let keys: Vec<Vec<u8>> = d.iter().map(|(k, _)| k.to_vec()).collect();
        for key in keys {
            if let Some(v) = d.get_mut(&key) {
                changed |= self.object(v, depth + 1);
            }
        }
        changed
    }
}

/// Rename aliased colorants (`aliases`: colorant → ink it prints on, see [`check_aliases`]) in
/// every colour space of `doc`. On error nothing has been changed.
pub fn alias_inks(doc: &mut Document, aliases: &BTreeMap<Vec<u8>, Vec<u8>>) -> Result<AliasReport, EditError> {
    check_aliases(aliases)?;
    let mut walk = Walk { aliases, report: AliasReport::default(), skipped: Default::default(), nodes: 0 };
    if aliases.is_empty() {
        return Ok(walk.report);
    }
    let mut changes: Vec<(ObjRef, Object)> = Vec::new();
    for (r, object) in doc.scan_objects() {
        if !matches!(object.as_ref(), Object::Array(_) | Object::Dict(_) | Object::Stream(_)) {
            continue;
        }
        let mut copy = object.as_ref().clone();
        walk.nodes = 0;
        if walk.object(&mut copy, 0) {
            changes.push((r, copy));
        }
    }
    walk.report.objects = changes.len();
    for (r, o) in changes {
        doc.set(r, o);
    }
    walk.report.skipped = walk.skipped.into_iter().map(String::from).collect();
    Ok(walk.report)
}

#[cfg(test)]
mod tests;

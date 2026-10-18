//! PDF/A levels a and u: every glyph the content shows maps to Unicode (ISO 19005-1 §6.3.8,
//! ISO 19005-2/-3 §6.2.11.7), and — for level a — words are separated by spaces rather than
//! only by positioning (a heuristic, reported as a warning).
//!
//! A character code has a meaning when the font's ToUnicode CMap maps it, or when its encoding
//! does: a predefined encoding (Standard, WinAnsi, MacRoman) or a `/Differences` glyph name
//! with a known Unicode value, or, for composite fonts, the Adobe-GB1/CNS1/Japan1/Korea1
//! character collections. U+0000, U+FEFF and U+FFFE are not allowed; U+FFFD (the replacement
//! character) carries no meaning; Private Use Area values are reported as warnings (level a
//! needs ActualText for them, which isn't checked per character).
//!
//! [`check`] also says which fonts [`crate::convert`] can repair by writing a ToUnicode CMap
//! derived from the encoding (simple fonts held as indirect objects).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;

use pdfcraft_cos::{Dict, Document, ObjRef, Object};
use pdfcraft_fonts::pdf::{Metrics, encoding_unicode_map, to_unicode_map};

use crate::{Issue, Level};

const MAX_FORM_DEPTH: usize = 12;
/// Text-showing operations read in all; past it the check stops and says so.
const MAX_TEXT_OPS: usize = 2_000_000;
/// Distinct character codes kept per font.
const MAX_CODES_PER_FONT: usize = 65_536;
/// Distinct fonts tracked.
const MAX_FONTS: usize = 10_000;
/// Content stream parts per page.
const MAX_PARTS: usize = 10_000;
/// Codes quoted in a message.
const EXAMPLES: usize = 5;
/// A `TJ` adjustment that moves the next glyph this far right (thousandths of an em) reads as
/// a word break.
const WORD_GAP: f64 = 250.0;

/// Where a font comes from: an indirect object, or a direct dictionary in some resources.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Key {
    Indirect(ObjRef),
    Direct(String),
}

struct Used {
    dict: Dict,
    name: String,
    page: Option<usize>,
    codes: BTreeSet<u32>,
}

#[derive(Default)]
struct Usage {
    fonts: BTreeMap<Key, Used>,
    /// Page → `TJ` gaps that look like word breaks without a space character.
    word_gaps: BTreeMap<usize, usize>,
    text_ops: usize,
    truncated: bool,
}

struct Walk<'a> {
    doc: &'a Document,
    usage: Usage,
    metrics: HashMap<Key, Metrics>,
    forms: HashSet<ObjRef>,
}

fn sub(doc: &Document, d: &Dict, key: &[u8]) -> Dict {
    d.get(key).map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default()
}

fn page_content(doc: &Document, page: &Dict) -> Vec<u8> {
    let Some(c) = page.get(b"Contents") else { return Vec::new() };
    match &*doc.resolve(c) {
        Object::Stream(s) => s.decoded().unwrap_or_default(),
        Object::Array(a) => {
            let mut out = Vec::new();
            for x in a.iter().take(MAX_PARTS) {
                if let Object::Stream(s) = &*doc.resolve(x) {
                    out.extend(s.decoded().unwrap_or_default());
                    out.push(b'\n');
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

impl Walk<'_> {
    /// Record the codes of one shown string; returns its first and last code's text.
    fn show(&mut self, key: &Key, bytes: &[u8]) -> (Option<String>, Option<String>) {
        let Some(m) = self.metrics.get(key) else { return (None, None) };
        let codes = m.codes(bytes);
        let first = codes.first().and_then(|(c, _)| m.text_of(*c)).map(str::to_owned);
        let last = codes.last().and_then(|(c, _)| m.text_of(*c)).map(str::to_owned);
        if let Some(u) = self.usage.fonts.get_mut(key) {
            for (c, _) in codes {
                if u.codes.len() >= MAX_CODES_PER_FONT {
                    self.usage.truncated = true;
                    break;
                }
                u.codes.insert(c);
            }
        }
        (first, last)
    }

    fn font(&mut self, fonts: &Dict, name: &[u8], owner: &str, page: Option<usize>) -> Option<Key> {
        let entry = fonts.get(name)?;
        let (key, dict) = match entry {
            Object::Ref(r) => (Key::Indirect(*r), self.doc.get(*r).as_dict().cloned()?),
            other => (Key::Direct(format!("{owner}/{}", String::from_utf8_lossy(name))), self.doc.resolve(other).as_dict().cloned()?),
        };
        if !self.usage.fonts.contains_key(&key) {
            if self.usage.fonts.len() >= MAX_FONTS {
                self.usage.truncated = true;
                return None;
            }
            let label =
                dict.name(b"BaseFont").map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_else(|| String::from_utf8_lossy(name).into_owned());
            self.metrics.insert(key.clone(), Metrics::from_dict(self.doc, &dict));
            self.usage.fonts.insert(key.clone(), Used { dict, name: label, page, codes: BTreeSet::new() });
        }
        Some(key)
    }

    fn content(&mut self, data: &[u8], res: &Dict, owner: &str, page: Option<usize>, depth: usize) {
        let fonts = sub(self.doc, res, b"Font");
        let xobjects = sub(self.doc, res, b"XObject");
        let mut font: Option<Key> = None;
        let mut saved: Vec<Option<Key>> = Vec::new();
        for op in pdfcraft_content::parse(data).ops {
            match op.op.as_slice() {
                b"q" => {
                    if saved.len() < 1024 {
                        saved.push(font.clone());
                    }
                }
                b"Q" => {
                    if let Some(f) = saved.pop() {
                        font = f;
                    }
                }
                b"Tf" => font = op.name(0).and_then(|n| self.font(&fonts, n, owner, page)),
                b"Tj" | b"'" | b"\"" | b"TJ" => {
                    self.usage.text_ops = self.usage.text_ops.saturating_add(1);
                    if self.usage.text_ops > MAX_TEXT_OPS {
                        self.usage.truncated = true;
                        return;
                    }
                    let Some(key) = font.clone() else { continue };
                    match op.op.as_slice() {
                        b"TJ" => {
                            let items = op.operands.first().and_then(Object::as_array).cloned().unwrap_or_default();
                            // The last text shown, and the adjustment since.
                            let mut prev: Option<String> = None;
                            let mut gap = 0.0;
                            for item in &items {
                                match item {
                                    Object::String(s) => {
                                        let (first, last) = self.show(&key, &s.bytes);
                                        let spaced = |t: &Option<String>| t.as_deref().is_some_and(|t| t.chars().all(char::is_whitespace));
                                        if -gap >= WORD_GAP
                                            && prev.is_some()
                                            && !spaced(&prev)
                                            && !spaced(&first)
                                            && first.is_some()
                                            && let Some(p) = page
                                        {
                                            let n = self.usage.word_gaps.entry(p).or_insert(0);
                                            *n = n.saturating_add(1);
                                        }
                                        if last.is_some() {
                                            prev = last;
                                        }
                                        gap = 0.0;
                                    }
                                    other => gap += other.as_f64().filter(|v| v.is_finite()).unwrap_or(0.0),
                                }
                            }
                        }
                        b"\"" => {
                            if let Some(Object::String(s)) = op.operands.get(2) {
                                self.show(&key, &s.bytes);
                            }
                        }
                        _ => {
                            if let Some(Object::String(s)) = op.operands.first() {
                                self.show(&key, &s.bytes);
                            }
                        }
                    }
                }
                b"Do" if depth < MAX_FORM_DEPTH => {
                    let Some(r) = op.name(0).and_then(|n| xobjects.get(n)).and_then(Object::as_ref) else { continue };
                    if !self.forms.insert(r) {
                        continue;
                    }
                    let obj = self.doc.get(r);
                    if let Object::Stream(x) = &*obj
                        && x.dict.name(b"Subtype") == Some(b"Form")
                    {
                        let inner =
                            x.dict.get(b"Resources").map(|o| self.doc.resolve(o)).and_then(|o| o.as_dict().cloned()).unwrap_or_else(|| res.clone());
                        let data = x.decoded().unwrap_or_default();
                        self.content(&data, &inner, &format!("{}", r.num), page, depth + 1);
                    }
                    self.forms.remove(&r);
                }
                _ => {}
            }
        }
    }
}

/// Every font the pages and their annotation appearances show text with, and the codes used.
fn usage(doc: &Document) -> Usage {
    let mut w = Walk { doc, usage: Usage::default(), metrics: HashMap::new(), forms: HashSet::new() };
    for (i, p) in pdfcraft_model::pages(doc).iter().enumerate() {
        let res = sub(doc, &p.dict, b"Resources");
        let data = page_content(doc, &p.dict);
        w.content(&data, &res, &format!("page{i}"), Some(i), 0);
        let annots = p.dict.get(b"Annots").map(|a| doc.resolve(a)).and_then(|a| a.as_array().cloned()).unwrap_or_default();
        for a in annots.iter().take(100_000) {
            let Some(ad) = doc.resolve(a).as_dict().cloned() else { continue };
            let Some(n) = sub(doc, &ad, b"AP").get(b"N").cloned() else { continue };
            // A stream, or a dictionary of appearance states.
            let streams: Vec<Object> = match &*doc.resolve(&n) {
                Object::Stream(_) => vec![n.clone()],
                Object::Dict(states) => states.iter().take(64).map(|(_, v)| v.clone()).collect(),
                _ => Vec::new(),
            };
            for s in streams {
                let r = s.as_ref();
                if let Object::Stream(x) = &*doc.resolve(&s) {
                    if r.is_some_and(|r| !w.forms.insert(r)) {
                        continue;
                    }
                    let inner = x.dict.get(b"Resources").map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned()).unwrap_or_default();
                    let data = x.decoded().unwrap_or_default();
                    let owner = r.map_or_else(|| format!("page{i}-annot"), |r| format!("{}", r.num));
                    w.content(&data, &inner, &owner, Some(i), 1);
                    if let Some(r) = r {
                        w.forms.remove(&r);
                    }
                }
            }
        }
        if w.usage.truncated && w.usage.text_ops > MAX_TEXT_OPS {
            break;
        }
    }
    w.usage
}

/// What a mapped value says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Value {
    Ok,
    /// U+0000, U+FEFF or U+FFFE.
    Forbidden,
    /// U+FFFD.
    Replacement,
    /// Only Private Use Area characters.
    Private,
}

fn is_private(c: char) -> bool {
    matches!(u32::from(c), 0xE000..=0xF8FF | 0xF_0000..=0xF_FFFD | 0x10_0000..=0x10_FFFD)
}

fn classify(s: &str) -> Value {
    if s.chars().any(|c| matches!(c, '\u{0}' | '\u{FEFF}' | '\u{FFFE}')) {
        Value::Forbidden
    } else if s.contains('\u{FFFD}') {
        Value::Replacement
    } else if s.chars().all(is_private) {
        Value::Private
    } else {
        Value::Ok
    }
}

/// Whether a composite font's glyphs are named by an Adobe character collection that has a
/// Unicode mapping (ISO 19005-2 §6.2.11.7.2).
fn known_collection(doc: &Document, font: &Dict) -> bool {
    if font.name(b"Subtype") != Some(b"Type0") {
        return false;
    }
    let desc = font
        .get(b"DescendantFonts")
        .map(|d| doc.resolve(d))
        .and_then(|d| d.as_array().and_then(|a| a.first().cloned()))
        .and_then(|d| doc.resolve(&d).as_dict().cloned())
        .unwrap_or_default();
    let info = sub(doc, &desc, b"CIDSystemInfo");
    let text = |k: &[u8]| info.get(k).map(|o| doc.resolve(o)).and_then(|o| o.as_string().map(|s| s.bytes.clone()));
    text(b"Registry").as_deref() == Some(b"Adobe") && matches!(text(b"Ordering").as_deref(), Some(b"GB1" | b"CNS1" | b"Japan1" | b"Korea1" | b"KR"))
}

fn hex(code: u32, wide: bool) -> String {
    if wide || code > 0xFF { format!("<{code:04X}>") } else { format!("<{code:02X}>") }
}

fn examples(codes: &[u32], wide: bool) -> String {
    let mut s: Vec<String> = codes.iter().take(EXAMPLES).map(|c| hex(*c, wide)).collect();
    if codes.len() > EXAMPLES {
        s.push("…".into());
    }
    s.join(", ")
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The result of [`check`].
pub(crate) struct Check {
    pub issues: Vec<Issue>,
    /// Fonts [`crate::convert`] can repair: the font object and the ToUnicode map to write.
    pub fixes: Vec<(ObjRef, BTreeMap<u32, String>)>,
}

/// Check every glyph the document shows against `level`'s Unicode rules (levels a and u; for
/// level a also the word-break heuristic).
pub(crate) fn check(doc: &Document, level: Level) -> Check {
    let mut out = Check { issues: Vec::new(), fixes: Vec::new() };
    if !level.needs_unicode() {
        return out;
    }
    let p1 = level.part() == 1;
    let clause = if p1 { "6.3.8" } else { "6.2.11.7.2" };
    let pua_clause = if p1 { "6.3.8" } else { "6.2.11.7.3" };
    let usage = usage(doc);
    for (key, used) in &usage.fonts {
        if used.codes.is_empty() {
            continue;
        }
        let tu = to_unicode_map(doc, &used.dict);
        let enc = encoding_unicode_map(doc, &used.dict);
        let collection = known_collection(doc, &used.dict);
        let from_encoding = |c: u32| enc.as_ref().and_then(|e| e.get(&c)).filter(|t| !t.is_empty() && classify(t) == Value::Ok);
        let (mut missing, mut forbidden, mut replacement, mut private) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        // Every problem code has a good meaning in the encoding.
        let mut repairable = true;
        for &c in &used.codes {
            let value = tu
                .as_ref()
                .and_then(|m| m.get(&c))
                .filter(|t| !t.is_empty())
                .or_else(|| enc.as_ref().and_then(|e| e.get(&c)).filter(|t| !t.is_empty()));
            let problem = match value.map(|v| classify(v)) {
                Some(Value::Ok) => continue,
                None if collection => continue,
                None => &mut missing,
                Some(Value::Forbidden) => &mut forbidden,
                Some(Value::Replacement) => &mut replacement,
                Some(Value::Private) => {
                    private.push(c);
                    continue;
                }
            };
            problem.push(c);
            repairable &= from_encoding(c).is_some();
        }
        let simple = used.dict.name(b"Subtype") != Some(b"Type0");
        let fixable = repairable && simple && matches!(key, Key::Indirect(_));
        let name = &used.name;
        let wide = !simple;
        let examples = |codes: &[u32]| examples(codes, wide);
        let mut add = |codes: &[u32], what: String| {
            if !codes.is_empty() {
                out.issues.push(Issue::error(clause, what, used.page, fixable));
            }
        };
        let n = missing.len();
        add(&missing, format!("The font {name} shows {n} character code{} with no Unicode mapping ({})", plural(n), examples(&missing)));
        let n = forbidden.len();
        add(&forbidden, format!("The font {name} maps {n} character code{} to U+0000, U+FEFF or U+FFFE ({})", plural(n), examples(&forbidden)));
        let n = replacement.len();
        add(
            &replacement,
            format!(
                "The font {name} maps {n} character code{} to U+FFFD, the replacement character, which says nothing about the text ({})",
                plural(n),
                examples(&replacement)
            ),
        );
        if !private.is_empty() {
            let n = private.len();
            let why = if level.needs_structure() {
                "level a needs ActualText for such text, which isn't checked per character"
            } else {
                "the text has no standard meaning"
            };
            out.issues.push(Issue::warning(
                pua_clause,
                format!("The font {name} maps {n} character code{} only to the Private Use Area ({}): {why}", plural(n), examples(&private)),
                used.page,
            ));
        }
        if fixable
            && missing.len() + forbidden.len() + replacement.len() > 0
            && let Key::Indirect(r) = key
        {
            // Keep the CMap's good entries; take every other code from the encoding.
            let mut map: BTreeMap<u32, String> = BTreeMap::new();
            if let Some(e) = &enc {
                for (c, t) in e {
                    if *c <= 0xFF && !t.is_empty() && classify(t) == Value::Ok {
                        map.insert(*c, t.clone());
                    }
                }
            }
            if let Some(m) = &tu {
                for (c, t) in m {
                    if *c <= 0xFF && !t.is_empty() && classify(t) != Value::Forbidden && classify(t) != Value::Replacement {
                        map.insert(*c, t.clone());
                    }
                }
            }
            out.fixes.push((*r, map));
        }
    }
    if level.needs_structure() {
        for (p, n) in &usage.word_gaps {
            let (p, n) = (*p, *n);
            out.issues.push(Issue::warning(
                if p1 { "6.8" } else { "6.7" },
                format!(
                    "Page {}: {n} word break{} made by positioning rather than a space character; text extraction and assistive technology may run the words together (ISO 32000-1 §14.8.2.5)",
                    p + 1,
                    plural(n)
                ),
                Some(p),
            ));
        }
    }
    if usage.truncated {
        out.issues.push(Issue::warning(clause, "The Unicode check stopped at its limits; some text wasn't checked".into(), None));
    }
    out
}

/// UTF-16BE hex of `s`, as a ToUnicode destination.
fn utf16_hex(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 4);
    for u in s.encode_utf16() {
        let _ = write!(out, "{u:04X}");
    }
    out
}

/// A ToUnicode CMap (ISO 32000-2 §9.10.3) for a one-byte code space.
pub(crate) fn to_unicode_cmap(map: &BTreeMap<u32, String>) -> Vec<u8> {
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<00> <FF>\nendcodespacerange\n",
    );
    let entries: Vec<(&u32, &String)> = map.iter().filter(|(c, _)| **c <= 0xFF).collect();
    for chunk in entries.chunks(100) {
        let _ = writeln!(s, "{} beginbfchar", chunk.len());
        for (c, t) in chunk {
            let _ = writeln!(s, "<{:02X}> <{}>", c, utf16_hex(t));
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s.into_bytes()
}

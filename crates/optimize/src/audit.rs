//! PDF Optimizer ▸ Audit space usage: how many bytes each kind of content takes.
//!
//! Objects are put in the first category that reaches them (each object counts once, however
//! many pages share it). Each object's size is **measured in the file**: from its `n g obj` to
//! the end of its `endobj`, found through the cross-reference offsets (a stream's data is skipped
//! by its length, so data that happens to contain `endobj` is not cut short). Objects stored in a
//! compressed object stream share that stream's bytes in proportion to their decoded sizes.
//! Objects with unsaved edits are not in the file yet; they are sized as they would be written
//! and reported as estimated. Everything else (the catalog, page tree, document info, metadata,
//! cross-reference sections and trailers, the header, superseded revisions and unreachable
//! objects) is document overhead, so the rows always add up to the file size.

use std::collections::{HashMap, HashSet};

use pdfcraft_cos::{Dict, Document, ObjRef, Object, XrefEntry};

/// The categories, in Acrobat's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SpaceCategory {
    Images,
    ContentStreams,
    Fonts,
    XObjectForms,
    StructureInfo,
    Bookmarks,
    Thumbnails,
    Comments,
    Forms,
    NamedDestinations,
    ColorSpaces,
    Shadings,
    Patterns,
    ExtendedGraphicsStates,
    EmbeddedFiles,
    DocumentOverhead,
}

impl SpaceCategory {
    pub const ALL: [SpaceCategory; 16] = [
        SpaceCategory::Images,
        SpaceCategory::ContentStreams,
        SpaceCategory::Fonts,
        SpaceCategory::XObjectForms,
        SpaceCategory::StructureInfo,
        SpaceCategory::Bookmarks,
        SpaceCategory::Thumbnails,
        SpaceCategory::Comments,
        SpaceCategory::Forms,
        SpaceCategory::NamedDestinations,
        SpaceCategory::ColorSpaces,
        SpaceCategory::Shadings,
        SpaceCategory::Patterns,
        SpaceCategory::ExtendedGraphicsStates,
        SpaceCategory::EmbeddedFiles,
        SpaceCategory::DocumentOverhead,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SpaceCategory::Images => "Images",
            SpaceCategory::ContentStreams => "Content Streams",
            SpaceCategory::Fonts => "Fonts",
            SpaceCategory::XObjectForms => "X Object Forms",
            SpaceCategory::StructureInfo => "Structure Info",
            SpaceCategory::Bookmarks => "Bookmarks",
            SpaceCategory::Thumbnails => "Thumbnails",
            SpaceCategory::Comments => "Comments",
            SpaceCategory::Forms => "Forms",
            SpaceCategory::NamedDestinations => "Named Destinations",
            SpaceCategory::ColorSpaces => "Color Spaces",
            SpaceCategory::Shadings => "Shadings",
            SpaceCategory::Patterns => "Patterns",
            SpaceCategory::ExtendedGraphicsStates => "Extended Graphics States",
            SpaceCategory::EmbeddedFiles => "Embedded Files",
            SpaceCategory::DocumentOverhead => "Document Overhead",
        }
    }
}

/// One row of the audit.
#[derive(Clone, Debug, PartialEq)]
pub struct SpaceUse {
    pub category: SpaceCategory,
    pub bytes: u64,
    /// Share of the file (0–100).
    pub percent: f64,
    /// Objects counted in this category (0 for document overhead).
    pub objects: usize,
}

/// The whole audit: one row per category (adding up to [`SpaceAudit::total_bytes`]) and how the
/// document overhead breaks down.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpaceAudit {
    pub rows: Vec<SpaceUse>,
    /// The size of the file the document was read from.
    pub file_bytes: u64,
    /// What the rows add up to: the file size, or more when unsaved edits are estimated larger.
    pub total_bytes: u64,
    /// Part of the overhead: cross-reference tables or streams and trailers, every revision.
    pub cross_reference_bytes: u64,
    /// Part of the overhead: objects no category reaches (catalog, page tree, info, metadata,
    /// output intents, unreachable objects).
    pub unclaimed_object_bytes: u64,
    /// Part of the overhead: everything else (header, object-stream index tables, superseded
    /// revisions' objects, whitespace and anything the cross-reference does not point at).
    pub other_overhead_bytes: u64,
    /// Bytes of compressed object streams shared out among the objects they hold, by decoded
    /// size (an apportioned figure; directly stored objects are measured exactly).
    pub apportioned_bytes: u64,
    /// Objects sized by serializing them instead of measuring the file (unsaved edits, or
    /// offsets the file does not confirm). 0 means every counted byte was measured.
    pub estimated_objects: usize,
}

impl SpaceAudit {
    /// The row for `category`.
    pub fn get(&self, category: SpaceCategory) -> Option<&SpaceUse> {
        self.rows.iter().find(|r| r.category == category)
    }
}

/// Decoded object streams larger than this are shared out by serialized object sizes instead.
const MAX_OBJSTM_DECODE: usize = 64 << 20;

struct Audit<'a> {
    doc: &'a Document,
    owner: HashMap<u32, SpaceCategory>,
    /// Page and page-tree objects reached while claiming (they belong to the overhead).
    skipped: HashSet<u32>,
}

/// Push the references `o` holds (directly or in nested arrays and dictionaries) onto `out`,
/// leaving out back-pointers (`/Parent`, `/P`) that lead up the tree instead of to content.
/// Iterative: nesting depth costs heap, not stack.
fn collect_refs(o: &Object, out: &mut Vec<ObjRef>) {
    let mut stack: Vec<&Object> = vec![o];
    let up = |k: &[u8]| k == b"Parent" || k == b"P";
    while let Some(o) = stack.pop() {
        match o {
            Object::Ref(r) => out.push(*r),
            Object::Array(a) => stack.extend(a.iter()),
            Object::Dict(d) => stack.extend(d.iter().filter(|(k, _)| !up(k)).map(|(_, v)| v)),
            Object::Stream(s) => stack.extend(s.dict.iter().filter(|(k, _)| !up(k)).map(|(_, v)| v)),
            _ => {}
        }
    }
}

impl Audit<'_> {
    /// Claim `o` and everything it references for `cat` (objects already claimed keep theirs).
    /// Bounded: an object is expanded only when it is first claimed.
    fn claim(&mut self, o: &Object, cat: SpaceCategory) {
        let mut todo = Vec::new();
        collect_refs(o, &mut todo);
        while let Some(r) = todo.pop() {
            if self.owner.contains_key(&r.num) || self.skipped.contains(&r.num) {
                continue;
            }
            let obj = self.doc.get(r);
            // Pages belong to the page tree, not to whatever links to them.
            if obj.as_dict().is_some_and(|d| matches!(d.name(b"Type"), Some(b"Page" | b"Pages"))) {
                self.skipped.insert(r.num);
                continue;
            }
            self.owner.insert(r.num, cat);
            collect_refs(&obj, &mut todo);
        }
    }

    fn resolve_dict(&self, o: Option<&Object>) -> Dict {
        o.map(|x| self.doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default()
    }

    /// Resources, by kind, including those of the forms they draw (iteratively: a hostile chain
    /// of nested forms cannot exhaust the stack, and each form is visited once).
    fn resources(&mut self, res: &Dict) {
        let mut queue = vec![res.clone()];
        let mut forms: Vec<ObjRef> = Vec::new();
        while let Some(res) = queue.pop() {
            let xobjects = self.resolve_dict(res.get(b"XObject"));
            for (_, x) in xobjects.iter() {
                let Some(r) = x.as_ref() else { continue };
                if self.owner.contains_key(&r.num) {
                    continue;
                }
                let obj = self.doc.get(r);
                let Object::Stream(s) = &*obj else { continue };
                if s.dict.name(b"Subtype") == Some(b"Image") {
                    self.claim(x, SpaceCategory::Images);
                } else {
                    // A form: its own resources by kind, then whatever else it references.
                    self.owner.insert(r.num, SpaceCategory::XObjectForms);
                    queue.push(self.resolve_dict(s.dict.get(b"Resources")));
                    forms.push(r);
                }
            }
            for (key, cat) in [
                (&b"Font"[..], SpaceCategory::Fonts),
                (b"ColorSpace", SpaceCategory::ColorSpaces),
                (b"Shading", SpaceCategory::Shadings),
                (b"Pattern", SpaceCategory::Patterns),
                (b"ExtGState", SpaceCategory::ExtendedGraphicsStates),
            ] {
                if let Some(v) = res.get(key) {
                    self.claim(v, cat);
                }
            }
        }
        for f in forms {
            let obj = self.doc.get(f);
            if let Object::Stream(s) = &*obj {
                let mut rest = s.dict.clone();
                rest.remove(b"Resources");
                self.claim(&Object::Dict(rest), SpaceCategory::XObjectForms);
            }
        }
    }
}

fn is_pdf_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' | b'\0')
}

fn find(data: &[u8], needle: &[u8], from: usize, to: usize) -> Option<usize> {
    let hay = data.get(from..to.min(data.len()))?;
    hay.windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// Does the file hold `num g obj` at `offset`?
fn starts_object(data: &[u8], offset: usize, num: u32) -> bool {
    let digits = num.to_string();
    let Some(rest) = data.get(offset..) else { return false };
    rest.starts_with(digits.as_bytes()) && rest.get(digits.len()).copied().is_some_and(is_pdf_space)
}

/// Skip up to two end-of-line bytes from `at`, never beyond `limit`.
fn past_eol(data: &[u8], at: usize, limit: usize) -> usize {
    let mut end = at;
    while end < limit && end - at < 2 && data.get(end).copied().is_some_and(|b| b == b'\r' || b == b'\n') {
        end += 1;
    }
    end
}

/// The end of the object at `offset` (just past `endobj` and its end of line), never beyond
/// `limit`. `raw_len` is the stored length of a stream object's data.
fn object_end(data: &[u8], offset: usize, limit: usize, raw_len: Option<usize>) -> usize {
    let limit = limit.min(data.len());
    let mut from = offset;
    if let Some(len) = raw_len
        && let Some(kw) = find(data, b"stream", offset, limit)
    {
        // Skip the data by its length (it may contain anything, `endobj` included).
        from = kw.saturating_add(6).saturating_add(len).min(limit);
    }
    match find(data, b"endobj", from, limit) {
        Some(e) => past_eol(data, e + 6, limit),
        None => limit,
    }
}

/// Serialized size: what writing the object would take ("n g obj … endobj" and an xref entry).
fn estimated_size(obj: &Object) -> u64 {
    let mut out = Vec::new();
    pdfcraft_cos::serialize(obj, &mut out);
    out.len() as u64 + 20 + 20
}

/// Decoded size of each object an object stream holds, and the size of its index table.
fn objstm_members(stream: &Object) -> Option<(HashMap<u32, u64>, u64)> {
    let Object::Stream(s) = stream else { return None };
    let first = usize::try_from(s.dict.int(b"First")?).ok()?;
    let n = usize::try_from(s.dict.int(b"N")?).ok()?.min(1_000_000);
    let data = s.decoded_within(MAX_OBJSTM_DECODE).ok()?;
    let header = data.get(..first)?;
    let ints: Vec<u64> = header
        .split(|b| is_pdf_space(*b))
        .filter(|t| !t.is_empty())
        .take(n.saturating_mul(2))
        .map(|t| std::str::from_utf8(t).ok()?.parse().ok())
        .collect::<Option<_>>()?;
    let mut entries: Vec<(u32, u64)> = ints.as_chunks::<2>().0.iter().filter_map(|p| Some((u32::try_from(*p.first()?).ok()?, *p.get(1)?))).collect();
    entries.sort_by_key(|e| e.1);
    let body = data.len().saturating_sub(first) as u64;
    let mut sizes = HashMap::new();
    for (i, (num, off)) in entries.iter().enumerate() {
        let next = entries.get(i + 1).map_or(body, |e| e.1);
        *sizes.entry(*num).or_insert(0) += next.min(body).saturating_sub((*off).min(body));
    }
    Some((sizes, first as u64))
}

/// Audit the space a document uses; `file_len` is the size of the file on disk.
pub fn audit_space(doc: &Document, file_len: u64) -> Vec<SpaceUse> {
    audit_space_detailed(doc, file_len).rows
}

/// [`audit_space`] with the overhead broken down and how each figure was obtained.
pub fn audit_space_detailed(doc: &Document, file_len: u64) -> SpaceAudit {
    let mut a = Audit { doc, owner: HashMap::new(), skipped: HashSet::new() };
    let cat = doc.root().and_then(|r| doc.get(r).as_dict().cloned()).unwrap_or_default();
    let pages = pdfcraft_model::pages(doc);
    // Order matters: the most specific owners first. A page's resources come before its
    // content (a content stream's dictionary may point at what it draws, as PdfCraft's /PCAdded
    // does, and that stays an image or font) and before the structure tree, which points at
    // content streams, forms and annotations.
    for p in &pages {
        let res = a.resolve_dict(p.dict.get(b"Resources"));
        a.resources(&res);
        if let Some(c) = p.dict.get(b"Contents") {
            a.claim(c, SpaceCategory::ContentStreams);
        }
        if let Some(t) = p.dict.get(b"Thumb") {
            a.claim(t, SpaceCategory::Thumbnails);
        }
        if let Some(annots) = p.dict.get(b"Annots") {
            for x in doc.resolve(annots).as_array().cloned().unwrap_or_default() {
                let widget = doc.resolve(&x).as_dict().is_some_and(|d| d.name(b"Subtype") == Some(b"Widget"));
                a.claim(&x, if widget { SpaceCategory::Forms } else { SpaceCategory::Comments });
            }
        }
    }
    if let Some(s) = cat.get(b"StructTreeRoot") {
        a.claim(s, SpaceCategory::StructureInfo);
    }
    if let Some(f) = cat.get(b"AcroForm") {
        a.claim(f, SpaceCategory::Forms);
    }
    if let Some(o) = cat.get(b"Outlines") {
        a.claim(o, SpaceCategory::Bookmarks);
    }
    let names = a.resolve_dict(cat.get(b"Names"));
    if let Some(d) = names.get(b"Dests").or_else(|| cat.get(b"Dests")) {
        a.claim(d, SpaceCategory::NamedDestinations);
    }
    if let Some(e) = names.get(b"EmbeddedFiles") {
        a.claim(e, SpaceCategory::EmbeddedFiles);
    }
    let owner = a.owner;

    let data: &[u8] = doc.bytes();
    let numbers = doc.object_numbers();
    // Every position the cross-reference names: no object runs past the next one.
    let mut starts: Vec<usize> = numbers
        .iter()
        .filter_map(|n| match doc.xref_entry(*n) {
            Some(XrefEntry::InFile { offset, .. }) => usize::try_from(offset).ok(),
            _ => None,
        })
        .chain(doc.revisions().iter().filter_map(|r| usize::try_from(r.xref_offset).ok()))
        .filter(|o| *o < data.len())
        .collect();
    starts.push(data.len());
    starts.sort_unstable();
    starts.dedup();
    let next_start = |at: usize| starts.get(starts.partition_point(|s| *s <= at)).copied().unwrap_or(data.len());

    let mut bytes: HashMap<SpaceCategory, u64> = HashMap::new();
    let mut objects: HashMap<SpaceCategory, usize> = HashMap::new();
    let mut audit = SpaceAudit { file_bytes: file_len, ..SpaceAudit::default() };
    let mut add = |audit: &mut SpaceAudit, num: u32, size: u64| match owner.get(&num) {
        Some(c) => {
            *bytes.entry(*c).or_default() += size;
            *objects.entry(*c).or_default() += 1;
        }
        None => audit.unclaimed_object_bytes += size,
    };
    let get = |num: u32| doc.get(ObjRef::new(num, doc.generation(num)));
    let mut measured_offsets: HashSet<usize> = HashSet::new();
    // Members of each object stream, and each object stream's measured size.
    let mut in_streams: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut objstm_span: HashMap<u32, u64> = HashMap::new();
    for &num in &numbers {
        let entry = if doc.is_edited(num) { None } else { doc.xref_entry(num) };
        match entry {
            Some(XrefEntry::InFile { offset, .. }) => {
                let obj = get(num);
                let at = usize::try_from(offset).unwrap_or(usize::MAX);
                if !starts_object(data, at, num) {
                    audit.estimated_objects += 1;
                    add(&mut audit, num, estimated_size(&obj));
                    continue;
                }
                let (dict, raw_len) = match &*obj {
                    Object::Stream(s) => (Some(&s.dict), Some(s.raw.len())),
                    Object::Dict(d) => (Some(d), None),
                    _ => (None, None),
                };
                let size = object_end(data, at, next_start(at), raw_len).saturating_sub(at) as u64;
                measured_offsets.insert(at);
                match dict.and_then(|d| d.name(b"Type")) {
                    Some(b"ObjStm") if raw_len.is_some() => {
                        objstm_span.insert(num, size);
                    }
                    Some(b"XRef") if raw_len.is_some() => audit.cross_reference_bytes += size,
                    _ => add(&mut audit, num, size),
                }
            }
            Some(XrefEntry::InStream { stream, .. }) => in_streams.entry(stream).or_default().push(num),
            _ => {
                // An unsaved edit: not in the file yet.
                audit.estimated_objects += 1;
                add(&mut audit, num, estimated_size(&get(num)));
            }
        }
    }
    for (stream, members) in &in_streams {
        let Some(span) = objstm_span.remove(stream) else {
            // The object stream itself was not measured: estimate its members.
            for &m in members {
                audit.estimated_objects += 1;
                add(&mut audit, m, estimated_size(&get(m)));
            }
            continue;
        };
        // Decoded member sizes, or serialized sizes when the stream can't be decoded here.
        let (sizes, header) = objstm_members(&get(*stream)).unwrap_or_else(|| (members.iter().map(|m| (*m, estimated_size(&get(*m)))).collect(), 0));
        let whole = sizes.values().sum::<u64>().saturating_add(header).max(1);
        let mut given = 0u64;
        for &m in members {
            let part = sizes.get(&m).copied().unwrap_or(0);
            // share ≤ span because part ≤ whole.
            let share = u64::try_from(u128::from(span) * u128::from(part) / u128::from(whole)).unwrap_or(span);
            given = given.saturating_add(share);
            add(&mut audit, m, share);
        }
        audit.apportioned_bytes = audit.apportioned_bytes.saturating_add(given);
    }
    // Object streams nothing in the cross-reference lives in any more (superseded).
    audit.unclaimed_object_bytes += objstm_span.values().sum::<u64>();
    // Classic cross-reference tables and their trailers (streams were measured as objects).
    for rev in doc.revisions() {
        let Ok(at) = usize::try_from(rev.xref_offset) else { continue };
        if rev.is_stream || measured_offsets.contains(&at) || !data.get(at..).is_some_and(|d| d.starts_with(b"xref")) {
            continue;
        }
        let limit = next_start(at);
        // Up to and including this section's %%EOF line.
        let end = find(data, b"%%EOF", at, limit).map_or(limit, |e| past_eol(data, e + 5, limit));
        audit.cross_reference_bytes += end.saturating_sub(at) as u64;
    }

    let counted: u64 = bytes.values().sum();
    let overhead_known = audit.cross_reference_bytes.saturating_add(audit.unclaimed_object_bytes);
    let total = file_len.max(counted.saturating_add(overhead_known)).max(1);
    let overhead = total - counted;
    audit.other_overhead_bytes = overhead.saturating_sub(overhead_known);
    audit.total_bytes = total;
    bytes.insert(SpaceCategory::DocumentOverhead, overhead);
    audit.rows = SpaceCategory::ALL
        .into_iter()
        .map(|c| {
            let b = bytes.get(&c).copied().unwrap_or(0);
            SpaceUse { category: c, bytes: b, percent: b as f64 * 100.0 / total as f64, objects: objects.get(&c).copied().unwrap_or(0) }
        })
        .collect();
    audit
}

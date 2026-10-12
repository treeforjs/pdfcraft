//! Bounded structural preflight. An empty report is not a standards certificate.
//!
//! These checks inspect PDF objects, including direct dictionaries. They do not validate
//! every content operator, ICC profile, font program, or requirement of PDF/A, PDF/X or PDF/UA.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

const MAX_OBJECTS: usize = 100_000;
const MAX_NODES: usize = 250_000;
const MAX_FINDINGS: usize = 10_000;
const MAX_DEPTH: usize = 48;
const MAX_PAGE_LINKS: usize = 250_000;
const MAX_FIXES: usize = 1_000;
const MAX_RETAINED_PATH_BYTES: usize = 4 << 20;
pub const LIMITATION: &str = "Structural checks only. No detected issues does not establish PDF/A, PDF/X or PDF/UA conformance. Content operators, font programs, color management and all standard requirements are not fully validated.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Profile {
    #[default]
    General,
    Print,
    PdfA2b,
    PdfA3b,
}

impl Profile {
    pub const ALL: [Self; 4] = [Self::General, Self::Print, Self::PdfA2b, Self::PdfA3b];
    pub fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Print => "print",
            Self::PdfA2b => "pdfa-2b",
            Self::PdfA3b => "pdfa-3b",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::General => "General document checks",
            Self::Print => "Print preparation checks",
            Self::PdfA2b => "PDF/A-2b structural checks",
            Self::PdfA3b => "PDF/A-3b structural checks",
        }
    }
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }
    pub fn rules(self) -> Vec<Rule> {
        let mut rules = vec![
            Rule::FontEmbedding,
            Rule::AdditionalActions,
            Rule::ImageInterpolation,
            Rule::AnnotationPrint,
            Rule::ExternalStream,
            Rule::LzwCompression,
            Rule::Transparency,
            Rule::EmbeddedFiles,
        ];
        if matches!(self, Self::PdfA2b | Self::PdfA3b) {
            rules.push(Rule::PdfaStructure);
        }
        if self == Self::Print {
            rules.retain(|r| !matches!(r, Rule::AdditionalActions | Rule::EmbeddedFiles));
        }
        rules
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
    FontEmbedding,
    AdditionalActions,
    ImageInterpolation,
    AnnotationPrint,
    ExternalStream,
    LzwCompression,
    Transparency,
    EmbeddedFiles,
    PdfaStructure,
}

impl Rule {
    pub const ALL: [Self; 9] = [
        Self::FontEmbedding,
        Self::AdditionalActions,
        Self::ImageInterpolation,
        Self::AnnotationPrint,
        Self::ExternalStream,
        Self::LzwCompression,
        Self::Transparency,
        Self::EmbeddedFiles,
        Self::PdfaStructure,
    ];
    pub fn id(self) -> &'static str {
        match self {
            Self::FontEmbedding => "font-embedding",
            Self::AdditionalActions => "additional-actions",
            Self::ImageInterpolation => "image-interpolation",
            Self::AnnotationPrint => "annotation-print",
            Self::ExternalStream => "external-stream",
            Self::LzwCompression => "lzw-compression",
            Self::Transparency => "transparency",
            Self::EmbeddedFiles => "embedded-files",
            Self::PdfaStructure => "pdfa-structure",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::FontEmbedding => "Fonts without embedded programs",
            Self::AdditionalActions => "Additional actions",
            Self::ImageInterpolation => "Image interpolation",
            Self::AnnotationPrint => "Annotations without print flag",
            Self::ExternalStream => "External stream data",
            Self::LzwCompression => "LZW compression",
            Self::Transparency => "Transparency groups",
            Self::EmbeddedFiles => "Embedded file specifications",
            Self::PdfaStructure => "Additional PDF/A structural checks",
        }
    }
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.id() == id)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub profile: Profile,
    /// None selects the profile's built-in checks.
    pub checks: Option<Vec<Rule>>,
}

impl Settings {
    pub fn rules(&self) -> Result<Vec<Rule>, String> {
        let mut rules = self.checks.clone().unwrap_or_else(|| self.profile.rules());
        if rules.is_empty() || rules.len() > Rule::ALL.len() {
            return Err("Select between one and nine preflight checks".into());
        }
        if rules.contains(&Rule::PdfaStructure) && !matches!(self.profile, Profile::PdfA2b | Profile::PdfA3b) {
            return Err("The PDF/A structural check needs a PDF/A-2b or PDF/A-3b profile".into());
        }
        rules.sort();
        rules.dedup();
        Ok(rules)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Information,
}
impl Severity {
    pub fn id(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Information => "information",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathPart {
    Key(Vec<u8>),
    Index(usize),
}

// Queued siblings share their ancestry. A deep, wide direct-object tree must not clone
// every ancestor key for every queued child (which can amplify a tiny PDF by gigabytes).
struct PathNode {
    parent: Option<Arc<PathNode>>,
    part: PathPart,
    depth: usize,
    bytes: usize,
}

fn extend_path(parent: &Option<Arc<PathNode>>, part: PathPart) -> Arc<PathNode> {
    let own_bytes = match &part {
        PathPart::Key(k) => k.len(),
        PathPart::Index(_) => 0,
    };
    Arc::new(PathNode {
        parent: parent.clone(),
        part,
        depth: parent.as_ref().map_or(1, |p| p.depth + 1),
        bytes: parent.as_ref().map_or(own_bytes, |p| p.bytes + own_bytes),
    })
}

fn materialize_path(path: &Option<Arc<PathNode>>) -> Vec<PathPart> {
    let mut result = Vec::with_capacity(path.as_ref().map_or(0, |p| p.depth));
    let mut at = path.as_ref();
    while let Some(node) = at {
        result.push(node.part.clone());
        at = node.parent.as_ref();
    }
    result.reverse();
    result
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub object: ObjRef,
    pub path: Vec<PathPart>,
    /// Zero-based pages. Empty means no page association was established.
    pub pages: Vec<usize>,
}

impl Location {
    pub fn display(&self) -> String {
        let mut s = self.object.to_string();
        for part in &self.path {
            match part {
                PathPart::Key(key) => {
                    s.push('/');
                    for b in key {
                        if b.is_ascii_alphanumeric() || *b == b'_' {
                            s.push(char::from(*b));
                        } else {
                            s.push_str(&format!("#{b:02X}"));
                        }
                    }
                }
                PathPart::Index(i) => s.push_str(&format!("[{i}]")),
            }
        }
        s
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fixup {
    DisableInterpolation,
    SetPrintFlag,
    RemoveAdditionalActions,
    RecompressLzw,
}
impl Fixup {
    pub fn id(self) -> &'static str {
        match self {
            Self::DisableInterpolation => "disable-interpolation",
            Self::SetPrintFlag => "set-print-flag",
            Self::RemoveAdditionalActions => "remove-additional-actions",
            Self::RecompressLzw => "lzw-to-flate",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::DisableInterpolation => "Disable image interpolation (may change smoothing)",
            Self::SetPrintFlag => "Set annotation print flag (preserve all other flags)",
            Self::RemoveAdditionalActions => "Remove these additional actions (changes interactive behavior)",
            Self::RecompressLzw => "Recompress decoded stream with Flate",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub id: usize,
    pub rule: Rule,
    pub severity: Severity,
    pub message: String,
    pub location: Option<Location>,
    pub fixup: Option<Fixup>,
    pub clause: Option<&'static str>,
    // A fix is bound to the complete original owner object, including its encoded stream.
    expected: Option<Arc<Object>>,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub settings: Settings,
    pub checks: Vec<Rule>,
    pub findings: Vec<Finding>,
    pub objects_checked: usize,
    pub truncated: bool,
    pub limitations: Vec<String>,
}

impl Report {
    pub fn text(&self) -> String {
        let mut out = format!(
            "PdfCraft preflight: {}\n{}\nObjects checked: {}\nFindings: {}\n",
            self.settings.profile.label(),
            LIMITATION,
            self.objects_checked,
            self.findings.len()
        );
        for note in &self.limitations {
            out.push_str(&format!("Coverage: {note}\n"));
        }
        for f in &self.findings {
            let at = f.location.as_ref().map(Location::display).unwrap_or_else(|| "document".into());
            let pages = f.location.as_ref().map(|l| l.pages.iter().map(|p| (p + 1).to_string()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
            out.push_str(&format!(
                "{} [{}] {} — {}{}\n",
                f.id,
                f.severity.id(),
                at,
                f.message,
                if pages.is_empty() { String::new() } else { format!(" (pages {pages})") }
            ));
            if let Some(fix) = f.fixup {
                out.push_str(&format!("  Explicit fix: {}\n", fix.label()));
            }
        }
        out
    }

    pub fn fixes(&self, ids: &[usize]) -> Result<Vec<FixRequest>, String> {
        if ids.is_empty() || ids.len() > MAX_FIXES {
            return Err(format!("Select between one and {MAX_FIXES} findings to fix"));
        }
        let mut seen = BTreeSet::new();
        ids.iter()
            .map(|id| {
                if !seen.insert(*id) {
                    return Err(format!("Finding {id} was selected twice"));
                }
                let f = self.findings.iter().find(|f| f.id == *id).ok_or_else(|| format!("Unknown finding {id}; analyze again"))?;
                let fixup = f.fixup.ok_or_else(|| format!("Finding {id} has no supported fix"))?;
                Ok(FixRequest {
                    location: f.location.clone().ok_or("The finding has no object location")?,
                    expected: f.expected.clone().ok_or("The finding has no binding")?,
                    fixup,
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FixRequest {
    location: Location,
    expected: Arc<Object>,
    fixup: Fixup,
}

fn embedded_font(doc: &Document, d: &Dict) -> bool {
    if d.name(b"Subtype") == Some(b"Type3") {
        return true;
    }
    let descriptor = if d.name(b"Subtype") == Some(b"Type0") {
        d.get(b"DescendantFonts")
            .map(|o| doc.resolve(o))
            .and_then(|a| a.as_array().and_then(|a| a.first().cloned()))
            .map(|o| doc.resolve(&o))
            .and_then(|o| o.as_dict().and_then(|d| d.get(b"FontDescriptor").cloned()))
    } else {
        d.get(b"FontDescriptor").cloned()
    };
    descriptor.map(|o| doc.resolve(&o)).and_then(|o| o.as_dict().cloned()).is_some_and(|d| {
        [b"FontFile".as_slice(), b"FontFile2", b"FontFile3"].iter().any(|k| d.get(k).is_some_and(|o| matches!(&*doc.resolve(o), Object::Stream(_))))
    })
}

fn lzw(doc: &Document, d: &Dict) -> bool {
    d.get(b"Filter").map(|o| doc.resolve(o)).is_some_and(|o| match &*o {
        Object::Name(n) => n == b"LZWDecode" || n == b"LZW",
        Object::Array(a) => a.iter().take(64).any(|o| doc.resolve(o).as_name().is_some_and(|n| n == b"LZWDecode" || n == b"LZW")),
        _ => false,
    })
}

// Indirect filter/parameter changes would escape an owner-object binding. Report those
// streams, but do not offer a fix until dependency binding is implemented.
fn direct_filter_chain(d: &Dict) -> bool {
    let filter = match d.get(b"Filter") {
        Some(Object::Name(_)) => true,
        Some(Object::Array(a)) => a.len() <= 32 && a.iter().all(|o| matches!(o, Object::Name(_))),
        _ => false,
    };
    let direct_dict = |d: &Dict| d.iter().all(|(_, v)| !matches!(v, Object::Ref(_) | Object::Array(_) | Object::Dict(_) | Object::Stream(_)));
    filter
        && match d.get(b"DecodeParms") {
            None | Some(Object::Null) => true,
            Some(Object::Dict(d)) => direct_dict(d),
            Some(Object::Array(a)) => {
                a.len() <= 32
                    && a.iter().all(|o| match o {
                        Object::Null => true,
                        Object::Dict(d) => direct_dict(d),
                        _ => false,
                    })
            }
            _ => false,
        }
}

/// Page references and inherited resource references. A single global budget applies even
/// when a malicious page tree reuses large arrays or shares resources across many pages.
fn page_roots(doc: &Document, truncated: &mut bool) -> Vec<(ObjRef, Arc<Vec<ObjRef>>)> {
    let Some(root) = doc.root() else {
        return Vec::new();
    };
    let Some(top) = doc.get(root).as_dict().and_then(|d| d.reference(b"Pages")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![(top, Arc::new(Vec::new()))];
    let mut seen = BTreeSet::new();
    let mut budget = MAX_PAGE_LINKS;
    while let Some((reference, inherited)) = stack.pop() {
        if budget == 0 {
            *truncated = true;
            break;
        }
        budget -= 1;
        if !seen.insert(reference) {
            continue;
        }
        let object = doc.get(reference);
        let Some(d) = object.as_dict() else {
            continue;
        };
        let resources = if let Some(resource) = d.get(b"Resources") {
            let mut refs = Vec::new();
            let mut todo = vec![(resource, 0usize)];
            while let Some((o, depth)) = todo.pop() {
                if budget == 0 {
                    *truncated = true;
                    break;
                }
                budget -= 1;
                if depth >= MAX_DEPTH {
                    *truncated = true;
                    continue;
                }
                match o {
                    Object::Ref(r) => refs.push(*r),
                    Object::Dict(d) => {
                        let available = budget.saturating_sub(todo.len());
                        if d.len() > available {
                            *truncated = true;
                        }
                        todo.extend(d.iter().take(available).map(|(_, v)| (v, depth + 1)));
                    }
                    Object::Array(a) => {
                        let available = budget.saturating_sub(todo.len());
                        if a.len() > available {
                            *truncated = true;
                        }
                        todo.extend(a.iter().take(available).map(|v| (v, depth + 1)));
                    }
                    _ => {}
                }
            }
            Arc::new(refs)
        } else {
            inherited
        };
        if d.name(b"Type") == Some(b"Pages") {
            if let Some(kids) = d.get(b"Kids").map(|o| doc.resolve(o))
                && let Some(kids) = kids.as_array()
            {
                let available = budget.saturating_sub(stack.len());
                if kids.len() > available {
                    *truncated = true;
                }
                for child in kids.iter().take(available).rev().filter_map(Object::as_ref) {
                    stack.push((child, resources.clone()));
                }
            }
        } else if d.name(b"Type") == Some(b"Page") {
            out.push((reference, resources));
        }
    }
    out
}

/// Analyze without modifying the document. Both retained output and traversal are bounded.
pub fn analyze(doc: &Document, settings: &Settings) -> Result<Report, String> {
    let checks = settings.rules()?;
    let mut report = Report {
        settings: settings.clone(),
        checks,
        findings: Vec::new(),
        objects_checked: 0,
        truncated: false,
        limitations: vec![LIMITATION.into()],
    };
    let numbers = doc.object_numbers();
    report.truncated = numbers.len() > MAX_OBJECTS;
    let mut edges: BTreeMap<ObjRef, BTreeSet<ObjRef>> = BTreeMap::new();
    let mut nodes = 0usize;
    // IDs are independent of the selected checks, so changing a profile cannot reinterpret
    // a previously selected ID as a different fix on the same document revision.
    let mut next_id = 0usize;
    let mut retained_path_bytes = 0usize;
    for number in numbers.into_iter().take(MAX_OBJECTS) {
        if nodes >= MAX_NODES {
            report.truncated = true;
            break;
        }
        let reference = ObjRef::new(number, doc.generation(number));
        let owner = doc.get(reference);
        report.objects_checked += 1;
        let mut stack = vec![(&*owner, None::<Arc<PathNode>>, false)];
        while let Some((object, path, back_reference)) = stack.pop() {
            if nodes >= MAX_NODES {
                report.truncated = true;
                break;
            }
            nodes += 1;
            if path.as_ref().is_some_and(|p| p.depth >= MAX_DEPTH) {
                report.truncated = true;
                continue;
            }
            if let Object::Ref(to) = object {
                if !back_reference {
                    edges.entry(reference).or_default().insert(*to);
                }
                continue;
            }
            if let Some(d) = object.as_dict() {
                let mut add = |rule, severity, message: &str, fixup| {
                    next_id += 1;
                    if !report.checks.contains(&rule) {
                        return;
                    }
                    if report.findings.len() == MAX_FINDINGS {
                        report.truncated = true;
                        return;
                    }
                    let path_bytes = path.as_ref().map_or(0, |p| p.bytes);
                    if path_bytes > MAX_RETAINED_PATH_BYTES.saturating_sub(retained_path_bytes) {
                        report.truncated = true;
                        return;
                    }
                    retained_path_bytes += path_bytes;
                    report.findings.push(Finding {
                        id: next_id,
                        rule,
                        severity,
                        message: message.into(),
                        location: Some(Location { object: reference, path: materialize_path(&path), pages: Vec::new() }),
                        fixup,
                        clause: None,
                        expected: fixup.map(|_| owner.clone()),
                    });
                };
                let ty = d.name(b"Type");
                let sub = d.name(b"Subtype");
                if ty == Some(b"Font") && !embedded_font(doc, d) {
                    add(Rule::FontEmbedding, Severity::Error, "Font has no embedded font program", None);
                }
                let annot = ty == Some(b"Annot") || (d.get(b"Rect").is_some() && d.get(b"P").is_some() && sub.is_some());
                if d.get(b"AA").is_some() && (matches!(ty, Some(b"Catalog") | Some(b"Page")) || annot || d.get(b"FT").is_some()) {
                    add(Rule::AdditionalActions, Severity::Warning, "Additional actions are present", Some(Fixup::RemoveAdditionalActions));
                }
                if annot && sub != Some(b"Popup") && d.int(b"F").unwrap_or(0) & 4 == 0 {
                    let fixable = match d.get(b"F") {
                        None => true,
                        Some(Object::Int(flags)) => (0..=u32::MAX as i64).contains(flags),
                        _ => false,
                    };
                    add(
                        Rule::AnnotationPrint,
                        Severity::Warning,
                        "Annotation does not have a readable print flag",
                        fixable.then_some(Fixup::SetPrintFlag),
                    );
                }
                if matches!(object, Object::Stream(_)) {
                    if sub == Some(b"Image") && matches!(d.get(b"Interpolate"), Some(Object::Bool(true))) {
                        add(Rule::ImageInterpolation, Severity::Warning, "Image interpolation is enabled", Some(Fixup::DisableInterpolation));
                    }
                    if d.get(b"F").is_some() || d.get(b"FFilter").is_some() {
                        add(Rule::ExternalStream, Severity::Error, "Stream depends on an external file", None);
                    }
                    if lzw(doc, d) {
                        add(
                            Rule::LzwCompression,
                            Severity::Warning,
                            "Stream uses LZW compression",
                            if d.get(b"F").is_none() && d.get(b"FFilter").is_none() && direct_filter_chain(d) {
                                Some(Fixup::RecompressLzw)
                            } else {
                                None
                            },
                        );
                    }
                }
                if ty == Some(b"Group") && d.name(b"S") == Some(b"Transparency") {
                    add(Rule::Transparency, Severity::Information, "Transparency group requires compatible output", None);
                }
                if ty == Some(b"Filespec") && d.get(b"EF").is_some() {
                    add(Rule::EmbeddedFiles, Severity::Information, "An embedded file specification is present", None);
                }
                // Push only a bounded number of children, even for hostile huge containers.
                let available = MAX_NODES.saturating_sub(nodes + stack.len());
                if d.len() > available {
                    report.truncated = true;
                }
                for (key, value) in d.iter().take(available) {
                    if key.len() > 256 {
                        report.truncated = true;
                        continue;
                    }
                    let next = extend_path(&path, PathPart::Key(key.clone()));
                    stack.push((value, Some(next), matches!(key.as_slice(), b"Parent" | b"P")));
                }
            } else if let Object::Array(a) = object {
                let available = MAX_NODES.saturating_sub(nodes + stack.len());
                if a.len() > available {
                    report.truncated = true;
                }
                for (i, value) in a.iter().enumerate().take(available) {
                    let next = extend_path(&path, PathPart::Index(i));
                    stack.push((value, Some(next), back_reference));
                }
            }
        }
    }
    // One graph was built above. Propagate page associations with a global budget, not a
    // separate unlimited recursive object traversal for every finding or page.
    let pages = page_roots(doc, &mut report.truncated);
    let mut queue = VecDeque::new();
    for (page, (object, resources)) in pages.iter().enumerate().take(MAX_PAGE_LINKS) {
        queue.push_back((page, *object));
        let available = MAX_PAGE_LINKS.saturating_sub(queue.len());
        if resources.len() > available {
            report.truncated = true;
        }
        queue.extend(resources.iter().take(available).map(|r| (page, *r)));
        if queue.len() >= MAX_PAGE_LINKS {
            report.truncated = true;
            break;
        }
    }
    let mut reached = BTreeSet::new();
    let mut object_pages: BTreeMap<ObjRef, Vec<usize>> = BTreeMap::new();
    while let Some((page, object)) = queue.pop_front() {
        if reached.len() >= MAX_PAGE_LINKS {
            report.truncated = true;
            break;
        }
        if !reached.insert((page, object)) {
            continue;
        }
        object_pages.entry(object).or_default().push(page);
        if let Some(children) = edges.get(&object) {
            let available = MAX_PAGE_LINKS.saturating_sub(queue.len() + reached.len());
            if children.len() > available {
                report.truncated = true;
            }
            queue.extend(children.iter().take(available).map(|child| (page, *child)));
        }
    }
    let mut retained_pages = 0usize;
    for finding in &mut report.findings {
        if let Some(location) = &mut finding.location
            && let Some(pages) = object_pages.get(&location.object)
        {
            let available = MAX_PAGE_LINKS.saturating_sub(retained_pages);
            location.pages = pages.iter().copied().take(available).collect();
            location.pages.sort_unstable();
            location.pages.dedup();
            retained_pages += location.pages.len();
            if pages.len() > available {
                report.truncated = true;
            }
        }
    }
    if report.checks.contains(&Rule::PdfaStructure) {
        if report.truncated {
            report.limitations.push("Additional PDF/A checks skipped because the traversal budget was reached".into());
        } else {
            let level = if settings.profile == Profile::PdfA3b { crate::Level::A3b } else { crate::Level::A2b };
            // Keep the legacy verifier's potentially expensive all-object/page comparisons
            // outside this bounded workflow. These catalog checks have explicit coverage.
            let mut issues = Vec::new();
            let metadata = doc.root().and_then(|r| doc.get(r).as_dict().and_then(|d| d.get(b"Metadata").cloned())).map(|o| doc.resolve(&o));
            let xmp = metadata.and_then(|o| match &*o {
                Object::Stream(s) => s.decoded_within(1 << 20).ok().and_then(|b| String::from_utf8(b).ok()),
                _ => None,
            });
            let identified = xmp.as_ref().is_some_and(|x| {
                crate::xmp::value(x, "pdfaid:part").as_deref() == Some(&level.part().to_string())
                    && crate::xmp::value(x, "pdfaid:conformance").is_some_and(|c| c.eq_ignore_ascii_case("B"))
            });
            if !identified {
                issues.push(crate::Issue {
                    clause: "6.6.4",
                    message: format!("Readable XMP (up to 1 MiB) does not identify the document as {}", level.label()),
                    page: None,
                    fixable: false,
                });
            }
            if doc.trailer().get(b"Encrypt").is_some() {
                issues.push(crate::Issue { clause: "6.1.3", message: "Document is encrypted".into(), page: None, fixable: false });
            }
            for issue in issues {
                if report.findings.len() >= MAX_FINDINGS {
                    report.truncated = true;
                    break;
                }
                next_id += 1;
                report.findings.push(Finding {
                    id: next_id,
                    rule: Rule::PdfaStructure,
                    severity: Severity::Error,
                    message: issue.message.chars().take(1024).collect(),
                    location: None,
                    fixup: None,
                    clause: Some(issue.clause),
                    expected: None,
                });
            }
            report.limitations.push("PDF/A-specific checks cover declared identification and encryption only. General checks cover selected fonts, actions, annotations and streams. Output intent validity, embedded-file conformance and remaining ISO requirements are not validated; no automatic PDF/A conversion is performed.".into());
        }
    }
    if report.truncated {
        report.limitations.push("Traversal or report limit reached; some objects, findings or page associations were omitted".into());
    }
    Ok(report)
}

fn target_mut<'a>(object: &'a mut Object, path: &[PathPart]) -> Result<&'a mut Object, String> {
    if path.len() > MAX_DEPTH {
        return Err("Fix target path is too deep".into());
    }
    let mut target = object;
    for part in path {
        target = match part {
            PathPart::Key(k) => target.as_dict_mut().and_then(|d| d.get_mut(k)),
            PathPart::Index(i) => match target {
                Object::Array(a) => a.get_mut(*i),
                _ => None,
            },
        }
        .ok_or("Fix target changed; analyze again")?;
    }
    Ok(target)
}

/// ISO 32000 LZW code framing. The general reader tolerates a missing end-of-data code;
/// a lossless fix must refuse that repair. Decoding still validates dictionary contents.
fn lzw_has_eod(data: &[u8], early_change: bool) -> bool {
    let (mut bit, mut width, mut next, mut previous) = (0usize, 9usize, 258usize, false);
    loop {
        let mut code = 0u16;
        for _ in 0..width {
            let Some(byte) = data.get(bit / 8) else {
                return false;
            };
            code = (code << 1) | u16::from((byte >> (7 - bit % 8)) & 1);
            bit += 1;
        }
        if code == 257 {
            return true;
        }
        if code == 256 {
            width = 9;
            next = 258;
            previous = false;
            continue;
        }
        if previous && next < 4096 {
            next += 1;
        }
        previous = true;
        if width < 12 && next + usize::from(early_change) >= (1 << width) {
            width += 1;
        }
    }
}

fn decode_for_fix(stream: &Stream) -> Result<Vec<u8>, String> {
    const LIMIT: usize = 32 << 20;
    if stream.raw.len() > LIMIT {
        return Err("Encoded stream exceeds the 32 MiB fix limit".into());
    }
    let mut data = stream.raw.to_vec();
    for (filter, params) in stream.filters() {
        if filter == pdfcraft_filters::Filter::Lzw && !lzw_has_eod(&data, params.early_change != 0) {
            return Err("LZW stream is missing its end-of-data code; not recompressed".into());
        }
        data = pdfcraft_filters::decode(&[(filter, params)], &data, LIMIT).map_err(|e| format!("Cannot recompress stream: {e}"))?;
    }
    Ok(data)
}

/// Apply selected, value-bound fixes as one transaction. Cancellation never publishes a
/// partial result. The caller must enforce document permissions (the engine does).
pub fn apply_fixes(doc: &mut Document, fixes: &[FixRequest], mut cancelled: impl FnMut() -> bool) -> Result<(), String> {
    if fixes.is_empty() || fixes.len() > MAX_FIXES {
        return Err(format!("Select between one and {MAX_FIXES} fixes"));
    }
    let mut seen = BTreeSet::new();
    for f in fixes {
        if cancelled() {
            return Err("Preflight fixes cancelled; document unchanged".into());
        }
        let key = format!("{}:{}", f.location.display(), f.fixup.id());
        if !seen.insert(key) {
            return Err("Duplicate fix target".into());
        }
        if doc.generation(f.location.object.num) != f.location.object.generation || *doc.get(f.location.object) != *f.expected {
            return Err("Fix target changed; analyze again".into());
        }
    }
    let mut next = doc.clone();
    let mut decoded_total = 0usize;
    for f in fixes {
        if cancelled() {
            return Err("Preflight fixes cancelled; document unchanged".into());
        }
        let mut object = (*next.get(f.location.object)).clone();
        let target = target_mut(&mut object, &f.location.path)?;
        match f.fixup {
            Fixup::RecompressLzw => {
                let Object::Stream(stream) = target else {
                    return Err("Fix target is not a stream".into());
                };
                // Strict decoding refuses truncated/unsupported data rather than 'repairing' it.
                let data = decode_for_fix(stream)?;
                decoded_total = decoded_total.saturating_add(data.len());
                if decoded_total > 128 << 20 {
                    return Err("Selected streams exceed the 128 MiB total fix limit; select fewer streams".into());
                }
                let mut dict = stream.dict.clone();
                dict.remove(b"DecodeParms");
                *stream = Stream::flate(dict, &data);
            }
            fix => {
                let dict = target.as_dict_mut().ok_or("Fix target is not a dictionary")?;
                match fix {
                    Fixup::DisableInterpolation => dict.set(b"Interpolate".to_vec(), Object::Bool(false)),
                    Fixup::SetPrintFlag => dict.set(b"F".to_vec(), Object::Int(dict.int(b"F").unwrap_or(0) | 4)),
                    Fixup::RemoveAdditionalActions => {
                        dict.remove(b"AA");
                    }
                    Fixup::RecompressLzw => {}
                }
            }
        }
        next.set(f.location.object, object);
    }
    if cancelled() {
        return Err("Preflight fixes cancelled; document unchanged".into());
    }
    *doc = next;
    Ok(())
}

#[cfg(test)]
mod tests;

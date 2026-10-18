//! Structural validation against the Arlington PDF Model (`core.arlington-validation`).
//!
//! [`validate_arlington`] walks the object graph breadth-first from the trailer. Each value
//! reached through a key is given an Arlington object type from that key's links; when a key
//! links to several types (a page tree's kids are pages or page tree nodes, an action is one of
//! twenty kinds), the one whose keys and fixed values (`/Type`, `/Subtype`, `/S`, …) fit the
//! object best is chosen. Every object is then checked against its definition:
//!
//! - missing required keys (inheritable keys are also looked up through `/Parent`) and array
//!   elements;
//! - values of a type the key doesn't allow;
//! - values outside the key's possible values;
//! - keys (and types, links or values) introduced after the PDF version being checked, and
//!   deprecated ones;
//! - direct/indirect reference rules;
//! - keys the model doesn't define, as information.
//!
//! Predicates are evaluated by `pdfcraft_arlington::expr` (see [`EVALUATED_PREDICATES`]);
//! a rule that depends on any other predicate is counted in [`ArlReport::not_checked`] and
//! never reported as a problem. The walk is bounded: every indirect object is checked once per
//! type (a seen-set over the graph, so cycles terminate), at most [`MAX_NODES`] objects in all,
//! [`MAX_DEPTH`] links deep and [`MAX_ARRAY_ITEMS`] elements per array; reaching a limit is
//! reported. Stream data is never decoded.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::Arc;

use pdfcraft_arlington::{
    self as arl, BaseType, Context, Indirect, Lookup, ObjectDef, Possible, Required, Row, Since, Tri, Val, ValueCheck, Version,
};
use pdfcraft_cos::{Dict, Document, Object};

pub use pdfcraft_arlington::expr::EVALUATED as EVALUATED_PREDICATES;
pub use pdfcraft_arlington::{MODEL_COMMIT, MODEL_SOURCE, Version as PdfVersion};

/// Findings kept by default.
pub const DEFAULT_ARL_FINDINGS: usize = 1000;
/// The most findings a caller may ask to keep.
pub const MAX_ARL_FINDINGS: usize = 100_000;
/// Objects (direct and indirect) checked at most.
pub const MAX_NODES: usize = 250_000;
/// Links followed at most from the trailer.
pub const MAX_DEPTH: usize = 512;
/// Elements checked at most per array.
pub const MAX_ARRAY_ITEMS: usize = 50_000;
/// `/Parent` levels searched for an inherited key.
const MAX_INHERIT: usize = 64;
/// Array items handed to predicates.
const MAX_PREDICATE_ITEMS: usize = 64;

/// What the Arlington rows say that isn't checked here, for reports.
pub const ARL_NOT_CHECKED: &[&str] = &[
    "SpecialCase column (cross-key conditions such as fn:BitsClear, fn:IsMeaningful, fn:Ignore)",
    "Rules that depend on predicates other than the evaluated ones (e.g. fn:NotStandard14Font, fn:FontHasLatinChars, fn:InNameTree, fn:PageContainsStructContentItems, parent:: paths)",
    "Possible values of array-typed keys (rectangles, matrices, fixed arrays)",
    "Value formats: dates, text-string encodings, name-tree key order and /Limits",
    "Content streams and stream data (never decoded)",
    "Objects not reachable from the trailer",
    "Extensions are assumed to be in use: extension keys are never reported as unknown",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArlSeverity {
    Error,
    Warning,
    Info,
}

impl ArlSeverity {
    pub fn id(self) -> &'static str {
        match self {
            ArlSeverity::Error => "error",
            ArlSeverity::Warning => "warning",
            ArlSeverity::Info => "info",
        }
    }
}

/// One problem.
#[derive(Clone, Debug, PartialEq)]
pub struct ArlFinding {
    pub severity: ArlSeverity,
    /// `missing-required`, `wrong-type`, `value-not-allowed`, `introduced-later`, `deprecated`,
    /// `must-be-indirect`, `must-be-direct`, `unknown-key`, `extra-element`, `limit`.
    pub code: &'static str,
    /// Number of the indirect object holding the value (`None` for the trailer).
    pub object: Option<u32>,
    /// Where it is: `Trailer/Root/Pages/Kids[0]/MediaBox`.
    pub path: String,
    /// The key or `[index]` concerned, if any.
    pub key: Option<String>,
    /// The Arlington object type the holder was checked as.
    pub object_type: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArlOptions {
    /// Check against this PDF version instead of the document's own.
    pub version: Option<Version>,
    /// Findings kept (1 to [`MAX_ARL_FINDINGS`]); the rest are counted. 0 means the default.
    pub max_findings: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArlReport {
    /// The PDF version checked against.
    pub version: Version,
    /// `option`, `header`, `catalog` (/Version is later than the header) or `default` (2.0).
    pub version_source: &'static str,
    pub findings: Vec<ArlFinding>,
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
    /// More findings were counted than kept.
    pub truncated: bool,
    /// Objects checked (direct and indirect).
    pub objects_checked: usize,
    /// Distinct indirect objects reached.
    pub indirect_objects: usize,
    /// Arlington object type → how many objects were checked as it.
    pub types: BTreeMap<String, usize>,
    /// Rules that couldn't be decided, by the predicate that stopped them.
    pub not_checked: BTreeMap<String, usize>,
    /// A walk limit was reached (the rest of the graph was not checked).
    pub limit_reached: bool,
}

impl ArlReport {
    pub fn valid(&self) -> bool {
        self.errors == 0
    }
}

/// The version a document is checked against: the header, or the catalog's /Version when later.
pub fn document_version(doc: &Document) -> (Version, &'static str) {
    let header = Version::parse(doc.version());
    let catalog = doc
        .root()
        .map(|r| doc.get(r))
        .and_then(|o| o.as_dict().and_then(|d| d.name(b"Version")).map(|n| String::from_utf8_lossy(n).into_owned()))
        .and_then(|n| Version::parse(&n));
    match (header, catalog) {
        (Some(h), Some(c)) if c > h => (c, "catalog"),
        (Some(h), _) => (h, "header"),
        (None, Some(c)) => (c, "catalog"),
        (None, None) => (Version::V2_0, "default"),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tree {
    Name,
    Number,
}

struct Task {
    /// The value as stored (a reference stays a reference, for the indirect rules).
    value: Object,
    /// The nearest indirect object holding it.
    owner: Option<u32>,
    candidates: Vec<&'static str>,
    path: String,
    depth: usize,
    /// A name- or number-tree node whose leaf values link to `candidates`.
    tree: Option<Tree>,
}

struct Walk<'a> {
    doc: &'a Document,
    model: &'static arl::Model,
    version: Version,
    file_size: u64,
    cap: usize,
    report: ArlReport,
    queue: VecDeque<Task>,
    /// (object number, Arlington type) already checked; tree nodes use the type `#tree`.
    seen: HashSet<(u32, &'static str)>,
    reached: HashSet<u32>,
    depth_noted: bool,
    /// Kept findings by severity (errors, warnings, information), each at most `cap`.
    kept: [Vec<ArlFinding>; 3],
}

/// Predicate context: the object a rule sits on.
struct Ctx<'a> {
    doc: &'a Document,
    cur: &'a Object,
    version: Version,
    file_size: u64,
    /// The row key being checked when it is a wildcard (`*`, `1*`) and the actual key or index it
    /// stands for, so `@*` and `@1*` read the current value.
    this: Option<(&'a str, &'a str)>,
}

fn resolve(doc: &Document, o: &Object) -> Arc<Object> {
    doc.resolve(o)
}

fn to_val(doc: &Document, o: &Object, depth: usize) -> Val {
    let o = resolve(doc, o);
    match &*o {
        Object::Null => Val::Null,
        Object::Bool(b) => Val::Bool(*b),
        Object::Int(i) => Val::Num(*i as f64),
        Object::Real(r) => Val::Num(*r),
        Object::String(s) => Val::Str(s.bytes.clone()),
        Object::Name(n) => Val::Name(String::from_utf8_lossy(n).into_owned()),
        Object::Array(a) => Val::Array {
            len: a.len(),
            items: if depth > 1 { Vec::new() } else { a.iter().take(MAX_PREDICATE_ITEMS).map(|x| to_val(doc, x, depth + 1)).collect() },
        },
        Object::Dict(_) => Val::Dict,
        Object::Stream(_) => Val::Stream,
        Object::Ref(_) => Val::Null,
    }
}

fn child<'o>(o: &'o Object, seg: &str) -> Option<&'o Object> {
    match o {
        Object::Array(a) => seg.parse::<usize>().ok().and_then(|i| a.get(i)),
        other => other.as_dict().and_then(|d| d.get(seg.as_bytes())),
    }
}

impl Context for Ctx<'_> {
    fn version(&self) -> Version {
        self.version
    }

    fn file_size(&self) -> Option<u64> {
        Some(self.file_size)
    }

    fn lookup(&self, path: &[&str]) -> Lookup {
        let trailer;
        let (mut cur, rest): (Arc<Object>, &[&str]) = match path.split_first() {
            Some((&"trailer", rest)) => {
                trailer = Object::Dict(self.doc.trailer().clone());
                // `trailer::Catalog::…` names the document catalog by its type.
                match rest.split_first() {
                    Some((&"Catalog", r)) => match self.doc.trailer().get(b"Root") {
                        Some(root) => (resolve(self.doc, root), r),
                        None => return Lookup::Absent,
                    },
                    _ => (Arc::new(trailer), rest),
                }
            }
            _ => (Arc::new(self.cur.clone()), path),
        };
        let mapped;
        let rest = match (self.this, rest) {
            (Some((row, actual)), [only]) if *only == row => {
                mapped = [actual];
                &mapped[..]
            }
            _ => rest,
        };
        let Some((last, inner)) = rest.split_last() else {
            return Lookup::Found(to_val(self.doc, &cur, 0));
        };
        for seg in inner {
            let next = match child(&cur, seg) {
                Some(n) => resolve(self.doc, n),
                None => return Lookup::Absent,
            };
            cur = next;
        }
        if *last == "*" {
            // Any entry of a dictionary (a stream's dictionary doesn't count: `AP::N::*` asks
            // whether /N is a dictionary of appearance states rather than one stream).
            return match &*cur {
                Object::Dict(d) if !d.is_empty() => Lookup::Found(Val::Dict),
                Object::Array(a) if !a.is_empty() => Lookup::Found(Val::Dict),
                _ => Lookup::Absent,
            };
        }
        match child(&cur, last) {
            Some(v) => match to_val(self.doc, v, 0) {
                Val::Null => Lookup::Absent,
                v => Lookup::Found(v),
            },
            None => Lookup::Absent,
        }
    }
}

fn kind_name(o: &Object) -> &'static str {
    match o {
        Object::Null => "null",
        Object::Bool(_) => "boolean",
        Object::Int(_) => "integer",
        Object::Real(_) => "number",
        Object::String(_) => "string",
        Object::Name(_) => "name",
        Object::Array(_) => "array",
        Object::Dict(_) => "dictionary",
        Object::Stream(_) => "stream",
        Object::Ref(_) => "reference",
    }
}

fn numbers(a: &[Object], n: usize) -> bool {
    a.len() == n && a.iter().all(|x| matches!(x, Object::Int(_) | Object::Real(_)))
}

/// Whether a resolved value is of an Arlington type. Rectangles and matrices must be direct
/// numbers (references inside them are rare and are not followed here).
fn type_matches(t: BaseType, o: &Object) -> bool {
    match t {
        BaseType::Array => matches!(o, Object::Array(_)),
        BaseType::Rectangle => o.as_array().is_some_and(|a| numbers(a, 4)),
        BaseType::Matrix => o.as_array().is_some_and(|a| numbers(a, 6)),
        BaseType::Bitmask | BaseType::Integer => matches!(o, Object::Int(_)),
        BaseType::Number => matches!(o, Object::Int(_) | Object::Real(_)),
        BaseType::Boolean => matches!(o, Object::Bool(_)),
        BaseType::Name => matches!(o, Object::Name(_)),
        BaseType::Null => matches!(o, Object::Null),
        BaseType::Dictionary | BaseType::NameTree | BaseType::NumberTree => matches!(o, Object::Dict(_)),
        BaseType::Stream => matches!(o, Object::Stream(_)),
        BaseType::Date | BaseType::String | BaseType::StringAscii | BaseType::StringByte | BaseType::StringText => {
            matches!(o, Object::String(_))
        }
    }
}

fn is_literal_list(p: &[Possible]) -> bool {
    fn lit(p: &Possible, depth: usize) -> bool {
        match p {
            Possible::Lit(_) | Possible::Wildcard => true,
            Possible::Gated(_, inner) if depth < 8 => lit(inner, depth + 1),
            _ => false,
        }
    }
    !p.is_empty() && p.iter().all(|e| lit(e, 0)) && !p.iter().any(|e| matches!(e, Possible::Wildcard))
}

fn literal_values(p: &[Possible]) -> Vec<String> {
    fn collect(p: &Possible, out: &mut Vec<String>, depth: usize) {
        match p {
            Possible::Lit(arl::Lit::Word(w)) => out.push(w.clone()),
            Possible::Lit(arl::Lit::Str(s)) => out.push(format!("'{s}'")),
            Possible::Gated(_, inner) if depth < 8 => collect(inner, out, depth + 1),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for e in p {
        collect(e, &mut out, 0);
    }
    out
}

fn lit_value_matches(p: &[Possible], v: &Val) -> bool {
    literal_values(p).iter().any(|w| match v {
        Val::Name(n) => n == w,
        Val::Num(x) => w.parse::<f64>().is_ok_and(|y| (x - y).abs() < 1e-9),
        Val::Bool(b) => w == if *b { "true" } else { "false" },
        Val::Str(s) => w.strip_prefix('\'').and_then(|w| w.strip_suffix('\'')).is_some_and(|w| w.as_bytes() == s.as_slice()),
        _ => false,
    })
}

fn row_type_fits(row: &Row, o: &Object) -> Option<usize> {
    row.types.iter().position(|(_, t)| t.is_some_and(|t| type_matches(t, o)))
}

impl<'a> Walk<'a> {
    #[allow(clippy::too_many_arguments)]
    fn finding(
        &mut self,
        severity: ArlSeverity,
        code: &'static str,
        owner: Option<u32>,
        path: String,
        key: Option<String>,
        def: &str,
        message: String,
    ) {
        match severity {
            ArlSeverity::Error => self.report.errors += 1,
            ArlSeverity::Warning => self.report.warnings += 1,
            ArlSeverity::Info => self.report.infos += 1,
        }
        let bucket = match severity {
            ArlSeverity::Error => 0,
            ArlSeverity::Warning => 1,
            ArlSeverity::Info => 2,
        };
        if let Some(list) = self.kept.get_mut(bucket)
            && list.len() < self.cap
        {
            list.push(ArlFinding { severity, code, object: owner, path, key, object_type: def.to_owned(), message });
        }
    }

    fn not_checked(&mut self, names: Vec<String>) {
        if names.is_empty() {
            *self.report.not_checked.entry("value not available (parent:: path or absent key)".into()).or_insert(0) += 1;
        }
        for n in names {
            *self.report.not_checked.entry(n).or_insert(0) += 1;
        }
    }

    fn ctx<'b>(&self, cur: &'b Object) -> Ctx<'b>
    where
        'a: 'b,
    {
        Ctx { doc: self.doc, cur, version: self.version, file_size: self.file_size, this: None }
    }

    /// Evaluate a condition on `cur`: `Some(true/false)`, or `None` (counted as not checked).
    fn condition(&mut self, cond: &str, cur: &Object) -> Option<bool> {
        let e = match arl::expr::parse(cond) {
            Ok(e) => e,
            Err(_) => {
                self.not_checked(vec!["unparsed predicate".into()]);
                return None;
            }
        };
        match arl::expr::eval(&e, &self.ctx(cur)) {
            Tri::True => Some(true),
            Tri::False => Some(false),
            Tri::Unknown => {
                self.not_checked(e.unsupported());
                None
            }
        }
    }

    /// A key inherited from an ancestor through `/Parent`; form fields also inherit variable text
    /// keys (`/DA`, `/Q`) from the interactive form dictionary (ISO 32000-2 §12.7.4.3).
    fn inherited(&self, def: &ObjectDef, dict: &Dict, key: &[u8]) -> bool {
        let has = |d: &Dict| d.get(key).is_some_and(|v| !matches!(*resolve(self.doc, v), Object::Null));
        let mut seen = HashSet::new();
        let mut parent = dict.get(b"Parent").cloned();
        for _ in 0..MAX_INHERIT {
            let Some(Object::Ref(r)) = parent else { break };
            if !seen.insert(r.num) {
                break;
            }
            let o = self.doc.get(r);
            let Some(d) = o.as_dict() else { break };
            if has(d) {
                return true;
            }
            parent = d.get(b"Parent").cloned();
        }
        if def.name.starts_with("Field") && matches!(key, b"DA" | b"Q") {
            let form = self.doc.root().map(|r| self.doc.get(r)).and_then(|c| c.as_dict().and_then(|c| c.get(b"AcroForm").cloned()));
            if let Some(form) = form.map(|f| resolve(self.doc, &f))
                && let Some(f) = form.as_dict()
            {
                return has(f);
            }
        }
        false
    }

    fn score(&self, def: &ObjectDef, o: &Object) -> i64 {
        let mut score = 0i64;
        let value_score = |row: &Row, v: &Object| -> i64 {
            let v = resolve(self.doc, v);
            let Some(i) = row_type_fits(row, &v) else { return -3 };
            match row.possible.get(i).and_then(|p| p.as_ref()) {
                Some(p) if is_literal_list(p) => {
                    let val = to_val(self.doc, &v, 0);
                    if lit_value_matches(p, &val) { 6 } else { -6 }
                }
                _ => 1,
            }
        };
        match o {
            Object::Array(items) => {
                for (i, it) in items.iter().take(16).enumerate() {
                    match def.element_row(i) {
                        Some(row) => score += value_score(row, it),
                        None => score -= 3,
                    }
                }
                for row in &def.rows {
                    if let Ok(idx) = row.key.parse::<usize>()
                        && idx >= items.len()
                        && row.required == Required::Yes
                    {
                        score -= 3;
                    }
                }
            }
            _ => {
                let Some(d) = o.as_dict() else { return i64::MIN };
                for row in def.rows.iter().filter(|r| !r.is_wildcard()) {
                    if let Some(v) = d.get(row.key.as_bytes()) {
                        score += value_score(row, v);
                    }
                    if !d.contains(row.key.as_bytes()) && row.required == Required::Yes {
                        score -= 2;
                    }
                }
                if def.rows.iter().all(|r| !r.is_wildcard()) {
                    let unknown = d.iter().filter(|(k, _)| !def.has_key(&String::from_utf8_lossy(k))).count();
                    score -= i64::try_from(unknown).unwrap_or(i64::MAX).min(1000);
                }
            }
        }
        score
    }

    fn process(&mut self, task: Task) {
        let (resolved, owner) = match &task.value {
            Object::Ref(r) => {
                self.reached.insert(r.num);
                (self.doc.get(*r), Some(r.num))
            }
            other => (Arc::new(other.clone()), task.owner),
        };
        let indirect = matches!(task.value, Object::Ref(_));
        if let Some(tree) = task.tree {
            self.tree_node(&task, &resolved, owner, indirect, tree);
            return;
        }
        let is_array = matches!(*resolved, Object::Array(_));
        let compatible: Vec<&'static ObjectDef> =
            task.candidates.iter().filter_map(|c| self.model.get(c)).filter(|d| d.is_array == is_array).collect();
        if compatible.is_empty() {
            if !task.candidates.is_empty() && !matches!(*resolved, Object::Null) {
                let names = task.candidates.join(", ");
                self.finding(
                    ArlSeverity::Error,
                    "wrong-type",
                    owner,
                    task.path.clone(),
                    None,
                    "",
                    format!("a {} where {names} is expected", kind_name(&resolved)),
                );
            }
            return;
        }
        if let (true, Some(num)) = (indirect, owner)
            && compatible.iter().any(|d| self.seen.contains(&(num, d.name.as_str())))
        {
            return;
        }
        let mut best: Option<(&'static ObjectDef, i64)> = None;
        for d in &compatible {
            let s = if compatible.len() == 1 { 0 } else { self.score(d, &resolved) };
            if best.is_none_or(|(_, b)| s > b) {
                best = Some((d, s));
            }
        }
        let Some((def, _)) = best else { return };
        if let (true, Some(num)) = (indirect, owner) {
            self.seen.insert((num, def.name.as_str()));
        }
        if self.report.objects_checked >= MAX_NODES {
            if !self.report.limit_reached {
                self.report.limit_reached = true;
                self.finding(
                    ArlSeverity::Info,
                    "limit",
                    owner,
                    task.path.clone(),
                    None,
                    &def.name,
                    format!("stopped after checking {MAX_NODES} objects; the rest of the document was not checked"),
                );
            }
            return;
        }
        self.report.objects_checked += 1;
        *self.report.types.entry(def.name.clone()).or_insert(0) += 1;
        match &*resolved {
            Object::Array(items) => self.check_array(def, &resolved, items, owner, &task),
            Object::Dict(d) => {
                // A field merged with its only widget (ISO 32000-2 §12.7.4.1) is checked as both.
                let partner = self.merged_partner(def, d);
                self.check_dict(def, &resolved, d, owner, &task, partner);
                if let Some(p) = partner {
                    let fresh = match (indirect, owner) {
                        (true, Some(num)) => self.seen.insert((num, p.name.as_str())),
                        _ => true,
                    };
                    if fresh {
                        *self.report.types.entry(p.name.clone()).or_insert(0) += 1;
                        self.check_dict(p, &resolved, d, owner, &task, Some(def));
                    }
                }
            }
            Object::Stream(s) => self.check_dict(def, &resolved, &s.dict, owner, &task, None),
            _ => {}
        }
    }

    /// The other half of a merged field and widget annotation.
    fn merged_partner(&self, def: &ObjectDef, d: &Dict) -> Option<&'static ObjectDef> {
        let widget = d.name(b"Subtype") == Some(b"Widget");
        if def.name.starts_with("Field") && widget {
            return self.model.get("AnnotWidget");
        }
        if def.name == "AnnotWidget" && d.get(b"FT").is_some() {
            let fields = ["FieldTx", "FieldBtnPush", "FieldBtnCheckbox", "FieldBtnRadio", "FieldChoice", "FieldSig"];
            let obj = Object::Dict(d.clone());
            let mut best: Option<(&'static ObjectDef, i64)> = None;
            for f in fields.iter().filter_map(|f| self.model.get(f)) {
                let s = self.score(f, &obj);
                if best.is_none_or(|(_, b)| s > b) {
                    best = Some((f, s));
                }
            }
            return best.map(|(f, _)| f);
        }
        None
    }

    fn tree_node(&mut self, task: &Task, node: &Object, owner: Option<u32>, indirect: bool, tree: Tree) {
        let Some(d) = node.as_dict() else { return };
        if let (true, Some(num)) = (indirect, owner)
            && !self.seen.insert((num, "#tree"))
        {
            return;
        }
        if self.report.objects_checked >= MAX_NODES {
            self.report.limit_reached = true;
            return;
        }
        self.report.objects_checked += 1;
        if let Some(kids) = d.get(b"Kids").map(|k| resolve(self.doc, k)).and_then(|k| k.as_array().cloned()) {
            for (i, kid) in kids.into_iter().take(MAX_ARRAY_ITEMS).enumerate() {
                self.push(Task {
                    value: kid,
                    owner,
                    candidates: task.candidates.clone(),
                    path: format!("{}/Kids[{i}]", task.path),
                    depth: task.depth + 1,
                    tree: Some(tree),
                });
            }
        }
        let leaves = if tree == Tree::Name { "Names" } else { "Nums" };
        if task.candidates.is_empty() {
            return;
        }
        if let Some(pairs) = d.get(leaves.as_bytes()).map(|k| resolve(self.doc, k)).and_then(|k| k.as_array().cloned()) {
            for (i, v) in pairs.into_iter().enumerate().take(MAX_ARRAY_ITEMS.saturating_mul(2)).skip(1).step_by(2) {
                if matches!(v, Object::Null) {
                    continue;
                }
                self.push(Task {
                    value: v,
                    owner,
                    candidates: task.candidates.clone(),
                    path: format!("{}/{leaves}[{i}]", task.path),
                    depth: task.depth + 1,
                    tree: None,
                });
            }
        }
    }

    fn push(&mut self, task: Task) {
        if task.depth > MAX_DEPTH {
            if !self.depth_noted {
                self.depth_noted = true;
                self.report.limit_reached = true;
                self.finding(
                    ArlSeverity::Info,
                    "limit",
                    task.owner,
                    task.path,
                    None,
                    "",
                    format!("objects more than {MAX_DEPTH} links from the trailer were not checked"),
                );
            }
            return;
        }
        self.queue.push_back(task);
    }

    /// Check a dictionary; keys `sibling` defines (the other half of a merged object) aren't unknown.
    fn check_dict(&mut self, def: &'static ObjectDef, cur: &Object, d: &Dict, owner: Option<u32>, task: &Task, sibling: Option<&ObjectDef>) {
        for row in def.rows.iter().filter(|r| !r.is_wildcard()) {
            let key = row.key.as_bytes();
            let value =
                d.get(key).filter(|v| !matches!(*resolve(self.doc, v), Object::Null) || row.types.iter().any(|t| t.1 == Some(BaseType::Null)));
            match value {
                Some(v) => self.check_value(def, row, &row.key, v, cur, owner, task, format!("{}/{}", task.path, row.key)),
                None => {
                    if row.inheritable && self.inherited(def, d, key) {
                        continue;
                    }
                    let required = match &row.required {
                        Required::Yes => true,
                        Required::No => false,
                        Required::If(cond) => self.condition(cond, cur).unwrap_or(false),
                    };
                    if required {
                        self.finding(
                            ArlSeverity::Error,
                            "missing-required",
                            owner,
                            format!("{}/{}", task.path, row.key),
                            Some(row.key.clone()),
                            &def.name,
                            format!("required key /{} is missing from this {}", row.key, def.name),
                        );
                    }
                }
            }
        }
        let wildcard = def.rows.iter().find(|r| r.is_wildcard());
        for (k, v) in d.iter() {
            let key = String::from_utf8_lossy(k).into_owned();
            if def.has_key(&key) || sibling.is_some_and(|s| s.has_key(&key)) {
                continue;
            }
            let path = format!("{}/{key}", task.path);
            match wildcard {
                Some(w) => {
                    if !matches!(*resolve(self.doc, v), Object::Null) {
                        self.check_value(def, w, &key, v, cur, owner, task, path);
                    }
                }
                None => self.finding(
                    ArlSeverity::Info,
                    "unknown-key",
                    owner,
                    path,
                    Some(key.clone()),
                    &def.name,
                    format!("/{key} is not defined for {} in the Arlington model", def.name),
                ),
            }
        }
    }

    fn check_array(&mut self, def: &'static ObjectDef, cur: &Object, items: &[Object], owner: Option<u32>, task: &Task) {
        for (i, it) in items.iter().take(MAX_ARRAY_ITEMS).enumerate() {
            let label = format!("[{i}]");
            let path = format!("{}{label}", task.path);
            match def.element_row(i) {
                Some(row) => self.check_value(def, row, &label, it, cur, owner, task, path),
                None => self.finding(
                    ArlSeverity::Info,
                    "extra-element",
                    owner,
                    path,
                    Some(label),
                    &def.name,
                    format!("{} has no element {i} in the Arlington model ({} elements here)", def.name, items.len()),
                ),
            }
        }
        if items.len() > MAX_ARRAY_ITEMS && !self.report.limit_reached {
            self.report.limit_reached = true;
            self.finding(
                ArlSeverity::Info,
                "limit",
                owner,
                task.path.clone(),
                None,
                &def.name,
                format!("only the first {MAX_ARRAY_ITEMS} of {} elements were checked", items.len()),
            );
        }
        for row in &def.rows {
            let Ok(idx) = row.key.parse::<usize>() else { continue };
            if idx < items.len() {
                continue;
            }
            let required = match &row.required {
                Required::Yes => true,
                Required::No => false,
                Required::If(cond) => self.condition(cond, cur).unwrap_or(false),
            };
            if required {
                self.finding(
                    ArlSeverity::Error,
                    "missing-required",
                    owner,
                    format!("{}[{idx}]", task.path),
                    Some(format!("[{idx}]")),
                    &def.name,
                    format!("{} needs element {idx}; this array has {}", def.name, items.len()),
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn check_value(
        &mut self,
        def: &'static ObjectDef,
        row: &'static Row,
        key: &str,
        raw: &Object,
        cur: &Object,
        owner: Option<u32>,
        task: &Task,
        path: String,
    ) {
        let v = resolve(self.doc, raw);
        let name = &def.name;
        let shown = if key.starts_with('[') { format!("element {key}") } else { format!("/{key}") };
        if !row.is_wildcard() {
            match &row.since {
                Since::Version(s) | Since::Extension { version: Some(s), .. } if *s > self.version => self.finding(
                    ArlSeverity::Warning,
                    "introduced-later",
                    owner,
                    path.clone(),
                    Some(key.to_owned()),
                    name,
                    format!("{shown} in {name} was introduced in PDF {s}; the file is checked as PDF {}", self.version),
                ),
                _ => {}
            }
            if let Some(d) = row.deprecated.filter(|d| *d <= self.version) {
                self.finding(
                    ArlSeverity::Warning,
                    "deprecated",
                    owner,
                    path.clone(),
                    Some(key.to_owned()),
                    name,
                    format!("{shown} in {name} is deprecated since PDF {d}"),
                );
            }
        }
        let known = row.types.iter().all(|t| t.1.is_some());
        let fits: Vec<usize> = row.types.iter().enumerate().filter(|(_, (_, t))| t.is_some_and(|t| type_matches(t, &v))).map(|(i, _)| i).collect();
        let Some(&first) = fits.first() else {
            if known {
                let expected: Vec<&str> = row.types.iter().filter_map(|t| t.1.map(BaseType::id)).collect();
                self.finding(
                    ArlSeverity::Error,
                    "wrong-type",
                    owner,
                    path,
                    Some(key.to_owned()),
                    name,
                    format!("{shown} in {name} is a {} but must be {}", describe(&v), expected.join(" or ")),
                );
            } else {
                self.not_checked(vec!["unknown type".into()]);
            }
            return;
        };
        let i = fits.iter().copied().find(|i| row.types.get(*i).is_some_and(|(g, _)| g.allows(self.version))).unwrap_or(first);
        let (gate, ty) = match row.types.get(i) {
            Some((g, Some(t))) => (g.clone(), *t),
            _ => return,
        };
        if !gate.allows(self.version) {
            self.finding(
                ArlSeverity::Warning,
                "introduced-later",
                owner,
                path.clone(),
                Some(key.to_owned()),
                name,
                format!("{shown} in {name} may be a {} only {}; the file is checked as PDF {}", ty.id(), gate_text(&gate), self.version),
            );
        }
        // Direct or indirect.
        let must_indirect = match row.indirect_for(i) {
            Indirect::Either => None,
            Indirect::MustBeIndirect => Some(true),
            Indirect::MustBeDirect => Some(false),
            Indirect::If { indirect, cond } => self.condition(cond, cur).and_then(|c| c.then_some(*indirect)),
        };
        let is_ref = matches!(raw, Object::Ref(_));
        match must_indirect {
            Some(true) if !is_ref && ty != BaseType::Stream => self.finding(
                ArlSeverity::Warning,
                "must-be-indirect",
                owner,
                path.clone(),
                Some(key.to_owned()),
                name,
                format!("{shown} in {name} must be an indirect reference"),
            ),
            Some(false) if is_ref => self.finding(
                ArlSeverity::Warning,
                "must-be-direct",
                owner,
                path.clone(),
                Some(key.to_owned()),
                name,
                format!("{shown} in {name} must be a direct object, not a reference"),
            ),
            _ => {}
        }
        // Possible values.
        if let Some(Some(possible)) = row.possible.get(i) {
            let val = to_val(self.doc, &v, 0);
            let actual = key.trim_start_matches('[').trim_end_matches(']');
            let mut ctx = self.ctx(cur);
            if row.key.contains('*') {
                ctx.this = Some((row.key.as_str(), actual));
            }
            let verdict = arl::check_possible(possible, &val, &ctx);
            match verdict {
                ValueCheck::Allowed => {}
                ValueCheck::NotAllowed => {
                    let lits = literal_values(possible);
                    let allowed = if lits.is_empty() {
                        "the model's conditions".to_owned()
                    } else {
                        let mut s = lits.iter().take(12).map(|l| l.as_str()).collect::<Vec<_>>().join(", ");
                        if lits.len() > 12 {
                            s.push_str(", …");
                        }
                        s
                    };
                    self.finding(
                        ArlSeverity::Error,
                        "value-not-allowed",
                        owner,
                        path.clone(),
                        Some(key.to_owned()),
                        name,
                        format!("{shown} in {name} is {} which is not allowed (allowed: {allowed})", show_val(&val)),
                    );
                }
                ValueCheck::Later(g) => self.finding(
                    ArlSeverity::Warning,
                    "introduced-later",
                    owner,
                    path.clone(),
                    Some(key.to_owned()),
                    name,
                    format!("{shown} in {name} may be {} only {}; the file is checked as PDF {}", show_val(&val), gate_text(&g), self.version),
                ),
                ValueCheck::Deprecated(d) => self.finding(
                    ArlSeverity::Warning,
                    "deprecated",
                    owner,
                    path.clone(),
                    Some(key.to_owned()),
                    name,
                    format!("{shown} in {name} is {} which is deprecated since PDF {d}", show_val(&val)),
                ),
                ValueCheck::NotChecked(names) => self.not_checked(names),
            }
        }
        // Follow the links.
        let Some(links) = row.links.get(i).filter(|l| !l.is_empty()) else {
            return;
        };
        let allowed: Vec<&'static str> = links.iter().filter(|(g, _)| g.allows(self.version)).map(|(_, l)| l.as_str()).collect();
        let candidates = if allowed.is_empty() { links.iter().map(|(_, l)| l.as_str()).collect() } else { allowed };
        let tree = match ty {
            BaseType::NameTree => Some(Tree::Name),
            BaseType::NumberTree => Some(Tree::Number),
            _ => None,
        };
        self.push(Task { value: raw.clone(), owner, candidates, path, depth: task.depth + 1, tree });
    }
}

fn gate_text(g: &arl::Gate) -> String {
    let mut parts = Vec::new();
    if let Some(s) = g.since {
        parts.push(format!("from PDF {s}"));
    }
    if let Some(b) = g.before {
        parts.push(format!("before PDF {b}"));
    }
    if let Some(o) = g.only {
        parts.push(format!("in PDF {o}"));
    }
    if parts.is_empty() { "in another PDF version".into() } else { parts.join(" and ") }
}

fn describe(o: &Object) -> String {
    match o {
        Object::Array(a) => format!("array of {} elements", a.len()),
        other => kind_name(other).to_owned(),
    }
}

fn show_val(v: &Val) -> String {
    match v {
        Val::Name(n) => format!("/{}", n.chars().take(64).collect::<String>()),
        Val::Num(n) => format!("{n}"),
        Val::Bool(b) => b.to_string(),
        Val::Str(s) => format!("({})", String::from_utf8_lossy(s).chars().take(64).collect::<String>()),
        Val::Null => "null".into(),
        Val::Array { len, .. } => format!("an array of {len}"),
        Val::Dict => "a dictionary".into(),
        Val::Stream => "a stream".into(),
    }
}

/// Validate a document against the Arlington model. Never panics on malformed input; the walk is
/// bounded (see the module docs).
pub fn validate_arlington(doc: &Document, options: &ArlOptions) -> ArlReport {
    let (version, version_source) = match options.version {
        Some(v) => (v, "option"),
        None => document_version(doc),
    };
    let cap = if options.max_findings == 0 { DEFAULT_ARL_FINDINGS } else { options.max_findings.min(MAX_ARL_FINDINGS) };
    let mut walk = Walk {
        doc,
        model: arl::model(),
        version,
        file_size: u64::try_from(doc.bytes().len()).unwrap_or(u64::MAX),
        cap,
        report: ArlReport {
            version,
            version_source,
            findings: Vec::new(),
            errors: 0,
            warnings: 0,
            infos: 0,
            truncated: false,
            objects_checked: 0,
            indirect_objects: 0,
            types: BTreeMap::new(),
            not_checked: BTreeMap::new(),
            limit_reached: false,
        },
        queue: VecDeque::new(),
        seen: HashSet::new(),
        reached: HashSet::new(),
        depth_noted: false,
        kept: [Vec::new(), Vec::new(), Vec::new()],
    };
    walk.queue.push_back(Task {
        value: Object::Dict(doc.trailer().clone()),
        owner: None,
        candidates: vec!["FileTrailer"],
        path: "Trailer".into(),
        depth: 0,
        tree: None,
    });
    while let Some(task) = walk.queue.pop_front() {
        walk.process(task);
    }
    walk.report.indirect_objects = walk.reached.len();
    // Errors first, then warnings, then information, each in walk order (nearest the trailer first).
    let mut findings: Vec<ArlFinding> = walk.kept.into_iter().flatten().collect();
    findings.truncate(cap);
    let total = walk.report.errors.saturating_add(walk.report.warnings).saturating_add(walk.report.infos);
    walk.report.truncated = total > findings.len();
    walk.report.findings = findings;
    walk.report
}

#[cfg(test)]
#[path = "arlington_tests.rs"]
mod tests;

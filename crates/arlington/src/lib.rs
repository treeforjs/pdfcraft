//! pdfcraft-arlington — the Arlington PDF Model (L0, standalone).
//!
//! The [Arlington PDF Model](https://github.com/pdf-association/arlington-pdf-model) (PDF
//! Association, Apache-2.0) defines every object type of ISO 32000-2 as a table: its keys or array
//! elements, their allowed types, whether they are required, the PDF version that introduced and
//! deprecated them, their possible values and the object types they link to. This crate carries the
//! model's TSV data (`data/arlington-model.tsv`, generated from `tsv/latest`, see the README) and
//! parses it into [`Model`] on first use. [`expr`] evaluates the subset of the model's predicate
//! language that can be decided from an object and the PDF version.
//!
//! The crate knows nothing about how PDF objects are stored: the object walk that applies the
//! model to a document lives in `pdfcraft-preflight` (`arlington` module), on top of `cos`.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod expr;

use std::collections::BTreeMap;
use std::sync::OnceLock;

pub use expr::{Context, Lookup, Tri, Val};

/// The vendored model: one `#Object` line per Arlington TSV file followed by its rows (header row
/// and the free-text Note column removed).
pub const MODEL_TSV: &str = include_str!("../data/arlington-model.tsv");
/// The upstream commit the data was generated from.
pub const MODEL_COMMIT: &str = "d37f4c8b11f54ae0d81a278bdeb91d00d6ccde26";
/// Where the data comes from.
pub const MODEL_SOURCE: &str = "https://github.com/pdf-association/arlington-pdf-model";

/// A PDF version, `major.minor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u8,
    pub minor: u8,
}

impl Version {
    pub const V2_0: Version = Version { major: 2, minor: 0 };

    /// Parse `1.7`, `2.0` (a leading `/` or `%PDF-` is accepted).
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches('/').trim_start_matches("%PDF-");
        let (a, b) = s.split_once('.')?;
        let major = a.parse::<u8>().ok()?;
        let minor = b.parse::<u8>().ok().filter(|m| *m < 10)?;
        (1..=9).contains(&major).then_some(Version { major, minor })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The Arlington basic types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BaseType {
    Array,
    Bitmask,
    Boolean,
    Date,
    Dictionary,
    Integer,
    Matrix,
    Name,
    NameTree,
    Null,
    Number,
    NumberTree,
    Rectangle,
    Stream,
    String,
    StringAscii,
    StringByte,
    StringText,
}

impl BaseType {
    pub fn parse(s: &str) -> Option<BaseType> {
        Some(match s.trim() {
            "array" => BaseType::Array,
            "bitmask" => BaseType::Bitmask,
            "boolean" => BaseType::Boolean,
            "date" => BaseType::Date,
            "dictionary" => BaseType::Dictionary,
            "integer" => BaseType::Integer,
            "matrix" => BaseType::Matrix,
            "name" => BaseType::Name,
            "name-tree" => BaseType::NameTree,
            "null" => BaseType::Null,
            "number" => BaseType::Number,
            "number-tree" => BaseType::NumberTree,
            "rectangle" => BaseType::Rectangle,
            "stream" => BaseType::Stream,
            "string" => BaseType::String,
            "string-ascii" => BaseType::StringAscii,
            "string-byte" => BaseType::StringByte,
            "string-text" => BaseType::StringText,
            _ => return None,
        })
    }

    pub fn id(self) -> &'static str {
        match self {
            BaseType::Array => "array",
            BaseType::Bitmask => "bitmask",
            BaseType::Boolean => "boolean",
            BaseType::Date => "date",
            BaseType::Dictionary => "dictionary",
            BaseType::Integer => "integer",
            BaseType::Matrix => "matrix",
            BaseType::Name => "name",
            BaseType::NameTree => "name-tree",
            BaseType::Null => "null",
            BaseType::Number => "number",
            BaseType::NumberTree => "number-tree",
            BaseType::Rectangle => "rectangle",
            BaseType::Stream => "stream",
            BaseType::String => "string",
            BaseType::StringAscii => "string-ascii",
            BaseType::StringByte => "string-byte",
            BaseType::StringText => "string-text",
        }
    }
}

/// The version predicates wrapped around a type, a link or a possible value
/// (`fn:SinceVersion(1.5,stream)`, `fn:IsPDFVersion(1.2,ActionNOP)`, `fn:Extension(X,…)`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gate {
    pub since: Option<Version>,
    pub before: Option<Version>,
    pub only: Option<Version>,
    pub deprecated: Option<Version>,
    /// Only defined by a named extension (ISO technical specifications, vendor extensions).
    pub extension: Option<String>,
}

impl Gate {
    /// Whether the version predicates allow `v` (extensions are assumed to be in use).
    pub fn allows(&self, v: Version) -> bool {
        self.since.is_none_or(|s| v >= s) && self.before.is_none_or(|b| v < b) && self.only.is_none_or(|o| v == o)
    }
    pub fn deprecated_in(&self, v: Version) -> Option<Version> {
        self.deprecated.filter(|d| v >= *d)
    }
}

/// Split `s` at `sep` outside brackets, parentheses and quotes.
fn split_top(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote = false;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '\'' => quote = !quote,
            '(' | '[' if !quote => depth += 1,
            ')' | ']' if !quote => depth -= 1,
            c if c == sep && depth == 0 && !quote => {
                out.push(s.get(start..i).unwrap_or(""));
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(s.get(start..).unwrap_or(""));
    out
}

/// `[x]` → `x`.
fn unbracket(s: &str) -> &str {
    let t = s.trim();
    match t.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
        Some(inner) => inner,
        None => t,
    }
}

/// `fn:Name(args)` → (`fn:Name`, top-level args).
fn call_parts(s: &str) -> Option<(&str, Vec<&str>)> {
    let s = s.trim();
    if !s.starts_with("fn:") || !s.ends_with(')') {
        return None;
    }
    let open = s.find('(')?;
    let name = s.get(..open)?;
    let inner = s.get(open + 1..s.len().saturating_sub(1))?;
    // The closing parenthesis must be the one that matches `open`.
    let mut depth = 0i32;
    for c in inner.chars() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            _ => {}
        }
    }
    let args = if inner.trim().is_empty() { Vec::new() } else { split_top(inner, ',').into_iter().map(str::trim).collect() };
    Some((name, args))
}

/// Peel version predicates off `s`: (gate, the wrapped text).
pub fn parse_gated(s: &str) -> (Gate, String) {
    let mut gate = Gate::default();
    let mut cur = s.trim().to_owned();
    for _ in 0..8 {
        let Some((name, args)) = call_parts(&cur) else { break };
        let (Some(first), second) = (args.first().copied(), args.get(1).copied()) else { break };
        let next = match name {
            "fn:SinceVersion" | "fn:BeforeVersion" | "fn:IsPDFVersion" | "fn:Deprecated" => {
                let (Some(v), Some(inner)) = (Version::parse(first), second) else { break };
                match name {
                    "fn:SinceVersion" => gate.since = Some(gate.since.map_or(v, |s| s.max(v))),
                    "fn:BeforeVersion" => gate.before = Some(v),
                    "fn:IsPDFVersion" => gate.only = Some(v),
                    _ => gate.deprecated = Some(v),
                }
                inner.to_owned()
            }
            "fn:Extension" => {
                gate.extension = Some(first.to_owned());
                match second {
                    Some(inner) => inner.to_owned(),
                    None => break,
                }
            }
            _ => break,
        };
        cur = next;
    }
    (gate, cur)
}

/// When a key was introduced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Since {
    Version(Version),
    /// Defined by an extension; `version` is the earliest PDF version it may appear in, if given.
    Extension {
        name: String,
        version: Option<Version>,
    },
    Unknown(String),
}

impl Since {
    fn parse(s: &str) -> Since {
        let s = s.trim();
        if let Some(v) = Version::parse(s) {
            return Since::Version(v);
        }
        // `fn:Extension(X)`, `fn:Extension(X,2.0)`, `fn:Eval(fn:Extension(X,1.7) || 2.0)`.
        let name = s.find("fn:Extension(").and_then(|i| {
            let rest = s.get(i + "fn:Extension(".len()..)?;
            let end = rest.find([',', ')'])?;
            rest.get(..end).map(|n| n.trim().to_owned())
        });
        let versions = s.split(|c: char| !(c.is_ascii_digit() || c == '.')).filter_map(Version::parse).min();
        match name {
            Some(name) => Since::Extension { name, version: versions },
            None => match versions {
                Some(v) => Since::Version(v),
                None => Since::Unknown(s.to_owned()),
            },
        }
    }

    /// The earliest PDF version the key may appear in.
    pub fn version(&self) -> Option<Version> {
        match self {
            Since::Version(v) => Some(*v),
            Since::Extension { version, .. } => *version,
            Since::Unknown(_) => None,
        }
    }
}

/// Whether a key must be present.
#[derive(Clone, Debug, PartialEq)]
pub enum Required {
    Yes,
    No,
    /// `fn:IsRequired(condition)`.
    If(String),
}

/// Whether a value must be an indirect reference or a direct object.
#[derive(Clone, Debug, PartialEq)]
pub enum Indirect {
    Either,
    MustBeIndirect,
    MustBeDirect,
    /// `fn:MustBeIndirect(condition)` / `fn:MustBeDirect(condition)`.
    If {
        indirect: bool,
        cond: String,
    },
}

impl Indirect {
    fn parse(s: &str) -> Indirect {
        match s.trim() {
            "TRUE" => Indirect::MustBeIndirect,
            "FALSE" | "" => Indirect::Either,
            "fn:MustBeDirect()" => Indirect::MustBeDirect,
            "fn:MustBeIndirect()" => Indirect::MustBeIndirect,
            other => match call_parts(other) {
                Some((name @ ("fn:MustBeDirect" | "fn:MustBeIndirect"), args)) if args.len() == 1 => {
                    Indirect::If { indirect: name == "fn:MustBeIndirect", cond: args.first().map(|a| (*a).to_owned()).unwrap_or_default() }
                }
                _ => Indirect::Either,
            },
        }
    }
}

/// A literal possible value: a name, number or boolean as written, or a quoted string.
#[derive(Clone, Debug, PartialEq)]
pub enum Lit {
    Word(String),
    Str(String),
}

/// One entry of a key's PossibleValues.
#[derive(Clone, Debug, PartialEq)]
pub enum Possible {
    Lit(Lit),
    /// `*`: any other name is allowed too.
    Wildcard,
    /// A literal behind version predicates.
    Gated(Gate, Box<Possible>),
    /// `fn:RequiredValue(condition, value)`: when the condition holds, the value must be this one.
    RequiredValue(String, Box<Possible>),
    /// `fn:Eval(condition)`: a value is allowed when the condition holds for it.
    Eval(String),
    /// A predicate this crate doesn't model.
    Unsupported(String),
}

impl Possible {
    fn parse(s: &str, depth: usize) -> Possible {
        let s = s.trim();
        if depth > 8 {
            return Possible::Unsupported(s.to_owned());
        }
        if s == "*" {
            return Possible::Wildcard;
        }
        if let Some(inner) = s.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')) {
            return Possible::Lit(Lit::Str(inner.to_owned()));
        }
        if !s.starts_with("fn:") {
            return Possible::Lit(Lit::Word(s.to_owned()));
        }
        match call_parts(s) {
            Some(("fn:Eval", args)) if args.len() == 1 => Possible::Eval(args.first().map(|a| (*a).to_owned()).unwrap_or_default()),
            Some(("fn:RequiredValue", args)) if args.len() == 2 => match (args.first(), args.get(1)) {
                (Some(c), Some(v)) => Possible::RequiredValue((*c).to_owned(), Box::new(Possible::parse(v, depth + 1))),
                _ => Possible::Unsupported(s.to_owned()),
            },
            Some(("fn:SinceVersion" | "fn:BeforeVersion" | "fn:IsPDFVersion" | "fn:Deprecated" | "fn:Extension", _)) => {
                let (gate, inner) = parse_gated(s);
                if inner == s { Possible::Unsupported(s.to_owned()) } else { Possible::Gated(gate, Box::new(Possible::parse(&inner, depth + 1))) }
            }
            Some((name, _)) => Possible::Unsupported(name.to_owned()),
            None => Possible::Unsupported(s.to_owned()),
        }
    }
}

/// The verdict on a value against PossibleValues.
#[derive(Clone, Debug, PartialEq)]
pub enum ValueCheck {
    Allowed,
    /// Allowed, but deprecated since this version.
    Deprecated(Version),
    /// Only allowed from a later PDF version (or only in another version).
    Later(Gate),
    NotAllowed,
    /// Can't be decided: the predicates involved.
    NotChecked(Vec<String>),
}

fn lit_matches(lit: &Lit, v: &Val) -> bool {
    match (lit, v) {
        (Lit::Word(w), Val::Name(n)) => w == n,
        (Lit::Word(w), Val::Num(x)) => w.parse::<f64>().is_ok_and(|y| (x - y).abs() < 1e-9),
        (Lit::Word(w), Val::Bool(b)) => w == if *b { "true" } else { "false" },
        (Lit::Str(s), Val::Str(b)) => s.as_bytes() == b.as_slice(),
        _ => false,
    }
}

enum EntryResult {
    Match(Gate),
    NoMatch,
    Unknown(Vec<String>),
}

fn check_entry(p: &Possible, v: &Val, ctx: &dyn Context, gate: Gate, depth: usize) -> EntryResult {
    if depth > 8 {
        return EntryResult::Unknown(vec!["nesting".into()]);
    }
    match p {
        Possible::Lit(l) => {
            if lit_matches(l, v) {
                EntryResult::Match(gate)
            } else {
                EntryResult::NoMatch
            }
        }
        Possible::Wildcard => {
            if matches!(v, Val::Name(_)) {
                EntryResult::Match(gate)
            } else {
                EntryResult::NoMatch
            }
        }
        Possible::Gated(g, inner) => {
            let mut merged = gate;
            merged.since = merged.since.max(g.since);
            merged.before = g.before.or(merged.before);
            merged.only = g.only.or(merged.only);
            merged.deprecated = g.deprecated.or(merged.deprecated);
            merged.extension = g.extension.clone().or(merged.extension);
            check_entry(inner, v, ctx, merged, depth + 1)
        }
        // The value is one of the possible values; `required_value_broken` checks the condition.
        Possible::RequiredValue(_, inner) => check_entry(inner, v, ctx, gate, depth + 1),
        Possible::Eval(cond) => match expr::parse(cond) {
            Ok(e) => match expr::eval(&e, ctx) {
                Tri::True => EntryResult::Match(gate),
                Tri::False => EntryResult::NoMatch,
                Tri::Unknown => {
                    let u = e.unsupported();
                    EntryResult::Unknown(if u.is_empty() { vec!["fn:Eval".into()] } else { u })
                }
            },
            Err(_) => EntryResult::Unknown(vec!["fn:Eval".into()]),
        },
        Possible::Unsupported(name) => EntryResult::Unknown(vec![name.clone()]),
    }
}

/// `fn:RequiredValue(condition, value)` entries (possibly behind version predicates that allow
/// `version`) whose condition holds while the value is a different one.
fn required_value_broken(p: &Possible, v: &Val, ctx: &dyn Context, depth: usize) -> bool {
    match p {
        Possible::Gated(g, inner) if depth < 8 => g.allows(ctx.version()) && required_value_broken(inner, v, ctx, depth + 1),
        Possible::RequiredValue(cond, inner) => {
            let Ok(e) = expr::parse(cond) else { return false };
            expr::eval(&e, ctx) == Tri::True && matches!(check_entry(inner, v, ctx, Gate::default(), depth + 1), EntryResult::NoMatch)
        }
        _ => false,
    }
}

/// Check `v` against a key's possible values. `ctx` evaluates conditions on the object holding
/// the key.
pub fn check_possible(entries: &[Possible], v: &Val, ctx: &dyn Context) -> ValueCheck {
    let version = ctx.version();
    if entries.iter().any(|p| required_value_broken(p, v, ctx, 0)) {
        return ValueCheck::NotAllowed;
    }
    let mut later = None;
    let mut unknown = Vec::new();
    let mut deprecated = None;
    let mut any_literal_rule = false;
    for p in entries {
        match check_entry(p, v, ctx, Gate::default(), 0) {
            EntryResult::Match(g) if g.allows(version) => match g.deprecated_in(version) {
                Some(d) => deprecated = Some(d),
                None => return ValueCheck::Allowed,
            },
            EntryResult::Match(g) => later = later.or(Some(g)),
            EntryResult::NoMatch => any_literal_rule = true,
            EntryResult::Unknown(u) => unknown.extend(u),
        }
    }
    if let Some(d) = deprecated {
        return ValueCheck::Deprecated(d);
    }
    if let Some(g) = later {
        return ValueCheck::Later(g);
    }
    if !unknown.is_empty() || !any_literal_rule {
        unknown.sort();
        unknown.dedup();
        return ValueCheck::NotChecked(unknown);
    }
    ValueCheck::NotAllowed
}

/// One key (dictionary) or element (array) row of an object definition.
#[derive(Clone, Debug)]
pub struct Row {
    /// The key, an array index (`0`), a repeating index (`0*`) or the wildcard `*`.
    pub key: String,
    /// Allowed types, each possibly behind version predicates. `None` when the type text isn't one
    /// of the Arlington types (then the type isn't checked).
    pub types: Vec<(Gate, Option<BaseType>)>,
    pub since: Since,
    pub deprecated: Option<Version>,
    pub required: Required,
    /// Per type (the same length as `types`), or a single entry for all.
    pub indirect: Vec<Indirect>,
    pub inheritable: bool,
    /// Possible values per type; `None` = anything of that type.
    pub possible: Vec<Option<Vec<Possible>>>,
    /// Linked object definitions per type, each possibly behind version predicates.
    pub links: Vec<Vec<(Gate, String)>>,
}

impl Row {
    pub fn is_wildcard(&self) -> bool {
        self.key == "*"
    }

    /// The indirect-reference rule for the type at `i`.
    pub fn indirect_for(&self, i: usize) -> &Indirect {
        const EITHER: Indirect = Indirect::Either;
        if self.indirect.len() == 1 { self.indirect.first().unwrap_or(&EITHER) } else { self.indirect.get(i).unwrap_or(&EITHER) }
    }

    fn parse(line: &str) -> Option<Row> {
        let cols: Vec<&str> = line.split('\t').collect();
        let col = |i: usize| cols.get(i).copied().unwrap_or("").trim();
        let key = col(0);
        if key.is_empty() {
            return None;
        }
        let types: Vec<(Gate, Option<BaseType>)> = split_top(col(1), ';')
            .into_iter()
            .map(|t| {
                let (g, inner) = parse_gated(t);
                (g, BaseType::parse(&inner))
            })
            .collect();
        let n = types.len();
        fn per_type(text: &str, n: usize) -> Vec<&str> {
            if text.is_empty() {
                return Vec::new();
            }
            let parts = split_top(text, ';');
            if parts.len() == n { parts } else { vec![text] }
        }
        let required = match col(4) {
            "TRUE" => Required::Yes,
            "FALSE" | "" => Required::No,
            other => match call_parts(other) {
                Some(("fn:IsRequired", args)) if args.len() == 1 => Required::If(args.first().map(|a| (*a).to_owned()).unwrap_or_default()),
                _ => Required::If(other.to_owned()),
            },
        };
        let indirect_text = col(5);
        let indirect = if indirect_text.starts_with('[') {
            split_top(indirect_text, ';').into_iter().map(|p| Indirect::parse(unbracket(p))).collect()
        } else {
            vec![Indirect::parse(indirect_text)]
        };
        let possible_parts = per_type(col(8), n);
        let possible = (0..n)
            .map(|i| {
                let ty = types.get(i).and_then(|t| t.1);
                // Array-valued possible values (`[[0 1],[1 0]]`) aren't modelled.
                if matches!(ty, Some(BaseType::Array | BaseType::Rectangle | BaseType::Matrix) | None) {
                    return None;
                }
                let raw = possible_parts.get(i).map(|p| unbracket(p)).filter(|p| !p.trim().is_empty())?;
                Some(split_top(raw, ',').into_iter().map(|e| Possible::parse(e, 0)).collect())
            })
            .collect();
        let link_parts = per_type(col(10), n);
        let links = (0..n)
            .map(|i| {
                let Some(raw) = link_parts.get(i).map(|p| unbracket(p)).filter(|p| !p.trim().is_empty()) else {
                    return Vec::new();
                };
                split_top(raw, ',')
                    .into_iter()
                    .map(|l| {
                        let (g, inner) = parse_gated(l);
                        (g, inner.trim().to_owned())
                    })
                    .filter(|(_, name)| !name.is_empty())
                    .collect()
            })
            .collect();
        Some(Row {
            key: key.to_owned(),
            types,
            since: Since::parse(col(2)),
            deprecated: Version::parse(col(3)),
            required,
            indirect,
            inheritable: col(6) == "TRUE",
            possible,
            links,
        })
    }
}

/// One Arlington object definition (one TSV file).
#[derive(Clone, Debug)]
pub struct ObjectDef {
    pub name: String,
    pub rows: Vec<Row>,
    /// The definition describes an array (rows are element indices).
    pub is_array: bool,
}

impl ObjectDef {
    /// The row for a dictionary key: the exact key, else the wildcard row.
    pub fn key_row(&self, key: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.key == key).or_else(|| self.rows.iter().find(|r| r.is_wildcard()))
    }

    /// Whether `key` is named explicitly (not only matched by a wildcard).
    pub fn has_key(&self, key: &str) -> bool {
        self.rows.iter().any(|r| r.key == key)
    }

    /// The row for array element `i`: a fixed index, else the repeating group (`N*`), else `*`.
    pub fn element_row(&self, i: usize) -> Option<&Row> {
        let idx = |r: &Row| r.key.parse::<usize>().ok();
        if let Some(r) = self.rows.iter().find(|r| idx(r) == Some(i)) {
            return Some(r);
        }
        let repeating: Vec<(&Row, usize)> =
            self.rows.iter().filter_map(|r| r.key.strip_suffix('*').and_then(|d| d.parse::<usize>().ok()).map(|d| (r, d))).collect();
        if let Some(start) = repeating.iter().map(|(_, d)| *d).min() {
            let n = repeating.len();
            if i >= start && n > 0 {
                let slot = start + (i - start) % n;
                if let Some((r, _)) = repeating.iter().find(|(_, d)| *d == slot) {
                    return Some(r);
                }
            }
        }
        self.rows.iter().find(|r| r.is_wildcard())
    }
}

/// The whole model.
#[derive(Debug, Default)]
pub struct Model {
    defs: BTreeMap<String, ObjectDef>,
    /// Data lines that couldn't be read (should be 0 for the vendored data).
    pub skipped_lines: usize,
}

impl Model {
    /// Parse model data in the vendored format.
    pub fn parse(text: &str) -> Model {
        let mut model = Model::default();
        let mut current: Option<ObjectDef> = None;
        for line in text.lines() {
            if line.starts_with("##") || line.trim().is_empty() {
                continue;
            }
            if let Some(name) = line.strip_prefix('#') {
                if let Some(def) = current.take() {
                    model.insert(def);
                }
                current = Some(ObjectDef { name: name.trim().to_owned(), rows: Vec::new(), is_array: false });
                continue;
            }
            match (current.as_mut(), Row::parse(line)) {
                (Some(def), Some(row)) => def.rows.push(row),
                _ => model.skipped_lines += 1,
            }
        }
        if let Some(def) = current.take() {
            model.insert(def);
        }
        model
    }

    fn insert(&mut self, mut def: ObjectDef) {
        let digit_keys = def.rows.iter().any(|r| r.key.starts_with(|c: char| c.is_ascii_digit()));
        let only_wildcard = !def.rows.is_empty() && def.rows.iter().all(|r| r.is_wildcard());
        def.is_array =
            digit_keys || (only_wildcard && (def.name.starts_with("ArrayOf") || def.name.ends_with("Array") || def.name.ends_with("ColorSpace")));
        self.defs.insert(def.name.clone(), def);
    }

    pub fn get(&self, name: &str) -> Option<&ObjectDef> {
        self.defs.get(name)
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    pub fn defs(&self) -> impl Iterator<Item = &ObjectDef> {
        self.defs.values()
    }
}

/// The vendored model, parsed once.
pub fn model() -> &'static Model {
    static MODEL: OnceLock<Model> = OnceLock::new();
    MODEL.get_or_init(|| Model::parse(MODEL_TSV))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendored_model_parses_completely() {
        let m = model();
        assert!(m.len() > 550, "{}", m.len());
        assert_eq!(m.skipped_lines, 0);
        // Every link names a definition that exists.
        for def in m.defs() {
            for row in &def.rows {
                assert_eq!(row.links.len(), row.types.len(), "{} {}", def.name, row.key);
                for (_, link) in row.links.iter().flatten() {
                    assert!(m.get(link).is_some(), "{}/{} links to unknown {link}", def.name, row.key);
                }
                assert!(row.types.iter().all(|(_, t)| t.is_some()), "{} {} has an unknown type", def.name, row.key);
                assert!(row.since.version().is_some() || matches!(row.since, Since::Extension { .. }), "{} {}", def.name, row.key);
            }
        }
    }

    #[test]
    fn every_model_condition_parses() {
        fn possibles(p: &Possible, out: &mut Vec<String>) {
            match p {
                Possible::Eval(c) => out.push(c.clone()),
                Possible::RequiredValue(c, inner) => {
                    out.push(c.clone());
                    possibles(inner, out);
                }
                Possible::Gated(_, inner) => possibles(inner, out),
                _ => {}
            }
        }
        let mut conds = Vec::new();
        for def in model().defs() {
            for row in &def.rows {
                if let Required::If(c) = &row.required {
                    conds.push(c.clone());
                }
                for i in &row.indirect {
                    if let Indirect::If { cond, .. } = i {
                        conds.push(cond.clone());
                    }
                }
                for p in row.possible.iter().flatten().flatten() {
                    possibles(p, &mut conds);
                }
            }
        }
        assert!(conds.len() > 400, "{}", conds.len());
        let failed: Vec<(&String, String)> = conds.iter().filter_map(|c| expr::parse(c).err().map(|e| (c, e))).collect();
        assert!(failed.is_empty(), "{} of {} don't parse: {failed:#?}", failed.len(), conds.len());
    }

    #[test]
    fn core_definitions_are_read_as_documented() {
        let m = model();
        let pages = m.get("PageTreeNodeRoot").unwrap();
        let ty = pages.key_row("Type").unwrap();
        assert_eq!(ty.required, Required::Yes);
        assert_eq!(ty.possible[0], Some(vec![Possible::Lit(Lit::Word("Pages".into()))]));
        let cat = m.get("Catalog").unwrap();
        let oa = cat.key_row("OpenAction").unwrap();
        assert_eq!(oa.types.iter().map(|t| t.1).collect::<Vec<_>>(), vec![Some(BaseType::Array), Some(BaseType::Dictionary)]);
        assert!(oa.links[1].iter().any(|(g, l)| l == "ActionGoToE" && g.since == Version::parse("1.6")));
        assert!(matches!(cat.key_row("AF").unwrap().since, Since::Extension { version: Some(v), .. } if v == Version::parse("1.7").unwrap()));
        assert_eq!(cat.key_row("SpiderInfo").unwrap().deprecated, Version::parse("2.0"));
        let kids = m.get("ArrayOfPageTreeNodeKids").unwrap();
        assert!(kids.is_array);
        assert_eq!(kids.element_row(7).unwrap().key, "*");
        let page = m.get("PageObject").unwrap();
        assert!(page.key_row("MediaBox").unwrap().inheritable);
        assert!(matches!(page.key_row("Parent").unwrap().required, Required::If(_)));
        assert!(!page.has_key("Bogus"));
    }

    #[test]
    fn repeating_array_rows() {
        let m = Model::parse(
            "#Pairs\n0\tname\t1.0\t\tTRUE\tFALSE\tFALSE\t\t\t\t\n1*\tinteger\t1.0\t\tTRUE\tFALSE\tFALSE\t\t\t\t\n2*\tname\t1.0\t\tTRUE\tFALSE\tFALSE\t\t\t\t\n",
        );
        let d = m.get("Pairs").unwrap();
        assert!(d.is_array);
        assert_eq!(d.element_row(0).unwrap().key, "0");
        assert_eq!(d.element_row(1).unwrap().key, "1*");
        assert_eq!(d.element_row(4).unwrap().key, "2*");
        assert_eq!(d.element_row(5).unwrap().key, "1*");
    }

    #[test]
    fn hostile_model_text_is_skipped_not_fatal() {
        let m = Model::parse("orphan\trow\n#X\n\t\t\n[[[[\tfn:SinceVersion(\t)))\t\tfn:IsRequired(\t\t\t\t[;;;]\t[\t]]]]\n");
        assert!(m.skipped_lines >= 1);
        assert!(m.get("X").is_some());
        assert_eq!(parse_gated("fn:SinceVersion(1.5,fn:Extension(ADBE,Foo))").1, "Foo");
        assert_eq!(parse_gated("fn:SinceVersion(junk,Foo)").1, "fn:SinceVersion(junk,Foo)");
        assert_eq!(Version::parse("1.10"), None);
        assert_eq!(Version::parse("%PDF-1.7"), Version::parse("1.7"));
    }

    struct V(Version, Val);
    impl Context for V {
        fn version(&self) -> Version {
            self.0
        }
        fn lookup(&self, path: &[&str]) -> Lookup {
            if path == ["Count"] { Lookup::Found(self.1.clone()) } else { Lookup::Absent }
        }
    }

    #[test]
    fn possible_values() {
        let v14 = Version::parse("1.4").unwrap();
        let entries = vec![
            Possible::Lit(Lit::Word("A".into())),
            Possible::Gated(Gate { since: Version::parse("1.6"), ..Gate::default() }, Box::new(Possible::Lit(Lit::Word("B".into())))),
        ];
        let c = V(v14, Val::Null);
        assert_eq!(check_possible(&entries, &Val::Name("A".into()), &c), ValueCheck::Allowed);
        assert!(matches!(check_possible(&entries, &Val::Name("B".into()), &c), ValueCheck::Later(_)));
        assert_eq!(check_possible(&entries, &Val::Name("C".into()), &c), ValueCheck::NotAllowed);
        let eval = vec![Possible::Eval("@Count>0".into())];
        assert_eq!(check_possible(&eval, &Val::Num(0.0), &V(v14, Val::Num(0.0))), ValueCheck::NotAllowed);
        assert_eq!(check_possible(&eval, &Val::Num(3.0), &V(v14, Val::Num(3.0))), ValueCheck::Allowed);
        // fn:RequiredValue(@Count>5, 9): 9 is allowed; when the condition holds nothing else is.
        let rv =
            vec![Possible::Lit(Lit::Word("1".into())), Possible::RequiredValue("@Count>5".into(), Box::new(Possible::Lit(Lit::Word("9".into()))))];
        assert_eq!(check_possible(&rv, &Val::Num(9.0), &V(v14, Val::Num(9.0))), ValueCheck::Allowed);
        assert_eq!(check_possible(&rv, &Val::Num(1.0), &V(v14, Val::Num(1.0))), ValueCheck::Allowed);
        assert_eq!(check_possible(&rv, &Val::Num(6.0), &V(v14, Val::Num(6.0))), ValueCheck::NotAllowed);
        let odd = vec![Possible::Unsupported("fn:InNameTree".into())];
        assert!(matches!(check_possible(&odd, &Val::Num(3.0), &c), ValueCheck::NotChecked(_)));
    }
}

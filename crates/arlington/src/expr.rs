//! The Arlington predicate language (`fn:…` expressions), the subset this crate evaluates.
//!
//! Expressions are parsed into a small tree and evaluated with three-valued logic: a predicate
//! this crate does not implement, a value it cannot see (`parent::` paths) or a comparison between
//! values of different kinds yields [`Tri::Unknown`], and the caller treats the rule as "not
//! checked" rather than guessing. Parsing is bounded in input length, token count and nesting
//! depth, so a malformed or hostile expression returns an error instead of recursing without end.

use crate::Version;

/// Longest expression accepted (bytes); the model's longest is well under 1 KiB.
const MAX_EXPR_LEN: usize = 4096;
/// Deepest nesting of parentheses and calls accepted.
const MAX_DEPTH: usize = 32;

/// The predicates [`eval`] implements. Any other `fn:` name evaluates to [`Tri::Unknown`].
pub const EVALUATED: &[&str] = &[
    "fn:SinceVersion",
    "fn:BeforeVersion",
    "fn:IsPDFVersion",
    "fn:Deprecated",
    "fn:Not",
    "fn:IsPresent",
    "fn:NotPresent",
    "fn:Eval",
    "fn:ArrayLength",
    "fn:Contains",
    "fn:RectWidth",
    "fn:RectHeight",
    "fn:FileSize",
    "fn:StringLength",
];

/// A PDF value as predicates see it. Arrays keep at most a bounded number of items (`len` is the
/// real length); dictionaries and streams are opaque.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Null,
    Bool(bool),
    Num(f64),
    Name(String),
    Str(Vec<u8>),
    Array { len: usize, items: Vec<Val> },
    Dict,
    Stream,
}

/// The result of looking a key path up.
#[derive(Clone, Debug, PartialEq)]
pub enum Lookup {
    Absent,
    Found(Val),
    /// The path can't be followed here (for example `parent::`).
    Unknown,
}

/// What an expression can see: the PDF version being checked against and the object it is
/// evaluated on. `lookup` receives the `::`-separated path segments of a key path relative to
/// the current object; a first segment `trailer` starts at the file trailer. A segment `*`
/// means "any key" (present if the dictionary has at least one entry).
pub trait Context {
    fn version(&self) -> Version;
    fn lookup(&self, path: &[&str]) -> Lookup;
    fn file_size(&self) -> Option<u64> {
        None
    }
}

/// Three-valued truth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tri {
    True,
    False,
    Unknown,
}

impl Tri {
    fn from(b: bool) -> Tri {
        if b { Tri::True } else { Tri::False }
    }
    fn not(self) -> Tri {
        match self {
            Tri::True => Tri::False,
            Tri::False => Tri::True,
            Tri::Unknown => Tri::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Mod,
}

/// A parsed expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// A number literal, with its source text (`1.4` may also be compared with the name `/1.4`).
    Num(f64, String),
    /// A bare word: a name literal, or a key path when it is the argument of a key function.
    Word(String),
    Str(Vec<u8>),
    Bool(bool),
    /// `@path`: the value of a key.
    Key(String),
    Call(String, Vec<Expr>),
    Neg(Box<Expr>),
    Bin(Op, Box<Expr>, Box<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    LParen,
    RParen,
    Comma,
    Op(Op),
    Num(f64, String),
    Word(String),
    Func(String),
    Key(String),
    Str(Vec<u8>),
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':')
}

fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    if s.len() > MAX_EXPR_LEN {
        return Err(format!("expression longer than {MAX_EXPR_LEN} bytes"));
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(&c) = chars.get(i) {
        let next = chars.get(i + 1).copied();
        match c {
            ' ' | '\t' => i += 1,
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            '&' if next == Some('&') => {
                out.push(Tok::Op(Op::And));
                i += 2;
            }
            '|' if next == Some('|') => {
                out.push(Tok::Op(Op::Or));
                i += 2;
            }
            '=' if next == Some('=') => {
                out.push(Tok::Op(Op::Eq));
                i += 2;
            }
            '!' if next == Some('=') => {
                out.push(Tok::Op(Op::Ne));
                i += 2;
            }
            '<' | '>' => {
                let eq = next == Some('=');
                out.push(Tok::Op(match (c, eq) {
                    ('<', true) => Op::Le,
                    ('<', false) => Op::Lt,
                    (_, true) => Op::Ge,
                    _ => Op::Gt,
                }));
                i += if eq { 2 } else { 1 };
            }
            '+' => {
                out.push(Tok::Op(Op::Add));
                i += 1;
            }
            '-' => {
                out.push(Tok::Op(Op::Sub));
                i += 1;
            }
            '*' => {
                out.push(Tok::Op(Op::Mul));
                i += 1;
            }
            '\'' => {
                let mut j = i + 1;
                let mut bytes = Vec::new();
                loop {
                    match chars.get(j) {
                        None => return Err("unterminated string".into()),
                        Some('\'') => break,
                        Some(ch) => {
                            let mut buf = [0u8; 4];
                            bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                    }
                    j += 1;
                }
                out.push(Tok::Str(bytes));
                i = j + 1;
            }
            '@' => {
                let mut j = i + 1;
                let mut w = String::new();
                while let Some(&ch) = chars.get(j) {
                    // `@*` and `@1*` name the current wildcard or repeating element; `::*` any key.
                    if is_word_char(ch) || (ch == '*' && (w.is_empty() || w.ends_with("::") || w.chars().all(|d| d.is_ascii_digit()))) {
                        w.push(ch);
                        j += 1;
                    } else {
                        break;
                    }
                }
                if w.is_empty() {
                    return Err("`@` without a key".into());
                }
                out.push(Tok::Key(w));
                i = j;
            }
            c if is_word_char(c) => {
                let mut j = i;
                let mut w = String::new();
                while let Some(&ch) = chars.get(j) {
                    if is_word_char(ch) || (ch == '*' && w.ends_with("::")) {
                        w.push(ch);
                        j += 1;
                    } else {
                        break;
                    }
                }
                // `parent::@Key`, `Encrypt::@V`: the value of a key in another object.
                if w.ends_with("::") && chars.get(j) == Some(&'@') {
                    j += 1;
                    while let Some(&ch) = chars.get(j) {
                        if is_word_char(ch) {
                            w.push(ch);
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    out.push(Tok::Key(w));
                    i = j;
                    continue;
                }
                i = j;
                if w == "mod" {
                    out.push(Tok::Op(Op::Mod));
                } else if w.starts_with("fn:") {
                    out.push(Tok::Func(w));
                } else if let Some(n) = w.parse::<f64>().ok().filter(|n| n.is_finite() && w.starts_with(|d: char| d.is_ascii_digit())) {
                    out.push(Tok::Num(n, w));
                } else {
                    out.push(Tok::Word(w));
                }
            }
            other => return Err(format!("unexpected character {other:?}")),
        }
        if out.len() > MAX_EXPR_LEN {
            return Err("too many tokens".into());
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos = self.pos.saturating_add(1);
        t
    }
    fn binary(&mut self, depth: usize, level: u8) -> Result<Expr, String> {
        if depth > MAX_DEPTH {
            return Err("expression nested too deeply".into());
        }
        let ops: &[Op] = match level {
            0 => &[Op::Or],
            1 => &[Op::And],
            2 => &[Op::Eq, Op::Ne, Op::Lt, Op::Le, Op::Gt, Op::Ge],
            3 => &[Op::Add, Op::Sub],
            4 => &[Op::Mul, Op::Mod],
            _ => return self.unary(depth),
        };
        let mut lhs = self.binary(depth + 1, level + 1)?;
        while let Some(Tok::Op(op)) = self.peek() {
            let op = *op;
            if !ops.contains(&op) {
                break;
            }
            self.bump();
            let rhs = self.binary(depth + 1, level + 1)?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }
    fn unary(&mut self, depth: usize) -> Result<Expr, String> {
        if depth > MAX_DEPTH {
            return Err("expression nested too deeply".into());
        }
        match self.bump() {
            Some(Tok::Op(Op::Sub)) => Ok(Expr::Neg(Box::new(self.unary(depth + 1)?))),
            Some(Tok::LParen) => {
                let e = self.binary(depth + 1, 0)?;
                match self.bump() {
                    Some(Tok::RParen) => Ok(e),
                    _ => Err("missing `)`".into()),
                }
            }
            Some(Tok::Num(n, s)) => Ok(Expr::Num(n, s)),
            Some(Tok::Str(s)) => Ok(Expr::Str(s)),
            Some(Tok::Key(k)) => Ok(Expr::Key(k)),
            Some(Tok::Word(w)) => Ok(match w.as_str() {
                "true" => Expr::Bool(true),
                "false" => Expr::Bool(false),
                _ => Expr::Word(w),
            }),
            Some(Tok::Op(Op::Mul)) => Ok(Expr::Word("*".into())),
            Some(Tok::Func(name)) => {
                let mut args = Vec::new();
                if self.bump() != Some(Tok::LParen) {
                    return Err(format!("{name} without `(`"));
                }
                if self.peek() == Some(&Tok::RParen) {
                    self.bump();
                    return Ok(Expr::Call(name, args));
                }
                loop {
                    args.push(self.binary(depth + 1, 0)?);
                    match self.bump() {
                        Some(Tok::Comma) => {}
                        Some(Tok::RParen) => break,
                        _ => return Err(format!("{name}: missing `)`")),
                    }
                    if args.len() > 16 {
                        return Err(format!("{name}: too many arguments"));
                    }
                }
                Ok(Expr::Call(name, args))
            }
            other => Err(format!("unexpected {other:?}")),
        }
    }
}

/// Parse an expression.
pub fn parse(s: &str) -> Result<Expr, String> {
    let mut p = Parser { toks: tokenize(s.trim())?, pos: 0 };
    let e = p.binary(0, 0)?;
    if p.pos < p.toks.len() {
        return Err(format!("unexpected text after the expression in {s:?}"));
    }
    Ok(e)
}

impl Expr {
    /// The `fn:` names used anywhere in the expression that [`eval`] doesn't implement.
    pub fn unsupported(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_unsupported(&mut out, 0);
        out.sort();
        out.dedup();
        out
    }
    fn collect_unsupported(&self, out: &mut Vec<String>, depth: usize) {
        if depth > MAX_DEPTH * 2 {
            return;
        }
        match self {
            Expr::Call(name, args) => {
                if !EVALUATED.contains(&name.as_str()) {
                    out.push(name.clone());
                }
                for a in args {
                    a.collect_unsupported(out, depth + 1);
                }
            }
            Expr::Neg(e) => e.collect_unsupported(out, depth + 1),
            Expr::Bin(_, a, b) => {
                a.collect_unsupported(out, depth + 1);
                b.collect_unsupported(out, depth + 1);
            }
            _ => {}
        }
    }
}

/// A value an expression evaluates to.
#[derive(Clone, Debug, PartialEq)]
enum Ev {
    Tri(Tri),
    Val(Val),
    /// A number literal keeps its text for comparison with names.
    Lit(f64, String),
    Word(String),
    Absent,
}

fn truth(e: &Ev) -> Tri {
    match e {
        Ev::Tri(t) => *t,
        Ev::Val(Val::Bool(b)) => Tri::from(*b),
        _ => Tri::Unknown,
    }
}

fn path_of(e: &Expr) -> Option<String> {
    match e {
        Expr::Word(w) | Expr::Key(w) => Some(w.clone()),
        Expr::Num(_, s) => Some(s.clone()),
        _ => None,
    }
}

fn lookup(ctx: &dyn Context, path: &str) -> Lookup {
    let segs: Vec<&str> = path.split("::").collect();
    if segs.len() > 16 || segs.iter().any(|s| s.is_empty()) {
        return Lookup::Unknown;
    }
    if segs.first().is_some_and(|s| *s == "parent") {
        return Lookup::Unknown;
    }
    ctx.lookup(&segs)
}

fn version_arg(e: Option<&Expr>) -> Option<Version> {
    match e? {
        Expr::Num(_, s) | Expr::Word(s) => Version::parse(s),
        _ => None,
    }
}

fn num_of(e: &Ev) -> Option<f64> {
    match e {
        Ev::Val(Val::Num(n)) | Ev::Lit(n, _) => Some(*n),
        _ => None,
    }
}

fn text_of(e: &Ev) -> Option<String> {
    match e {
        Ev::Val(Val::Name(n)) | Ev::Word(n) => Some(n.clone()),
        Ev::Lit(_, s) => Some(s.clone()),
        Ev::Val(Val::Str(b)) => Some(String::from_utf8_lossy(b).into_owned()),
        _ => None,
    }
}

fn equal(a: &Ev, b: &Ev) -> Tri {
    if matches!(a, Ev::Absent) || matches!(b, Ev::Absent) {
        return Tri::Unknown;
    }
    let num_side = |e: &Ev| matches!(e, Ev::Val(Val::Num(_)));
    if let (Some(x), Some(y)) = (num_of(a), num_of(b))
        && (num_side(a) || num_side(b) || matches!((a, b), (Ev::Lit(..), Ev::Lit(..))))
    {
        return Tri::from((x - y).abs() < 1e-9);
    }
    if let (Ev::Val(Val::Bool(x)), Ev::Val(Val::Bool(y))) = (a, b) {
        return Tri::from(x == y);
    }
    let bool_word = |e: &Ev| match e {
        Ev::Val(Val::Bool(x)) => Some(*x),
        _ => None,
    };
    if let (Some(x), Some(w)) = (bool_word(a).or(bool_word(b)), text_of(a).or(text_of(b))) {
        return Tri::from(w == if x { "true" } else { "false" });
    }
    match (text_of(a), text_of(b)) {
        (Some(x), Some(y)) => Tri::from(x == y),
        _ => Tri::Unknown,
    }
}

/// Evaluate an expression to a truth value.
pub fn eval(e: &Expr, ctx: &dyn Context) -> Tri {
    truth(&eval_ev(e, ctx, 0))
}

fn version_gate(name: &str, ctx: &dyn Context, args: &[Expr], depth: usize) -> Ev {
    let Some(v) = version_arg(args.first()) else {
        return Ev::Tri(Tri::Unknown);
    };
    let cur = ctx.version();
    let ok = match name {
        "fn:SinceVersion" => cur >= v,
        "fn:BeforeVersion" => cur < v,
        _ => cur == v,
    };
    match args.get(1) {
        None => Ev::Tri(Tri::from(ok)),
        Some(_) if !ok => Ev::Tri(Tri::False),
        Some(inner) => eval_ev(inner, ctx, depth + 1),
    }
}

fn eval_ev(e: &Expr, ctx: &dyn Context, depth: usize) -> Ev {
    if depth > MAX_DEPTH * 2 {
        return Ev::Tri(Tri::Unknown);
    }
    match e {
        Expr::Num(n, s) => Ev::Lit(*n, s.clone()),
        Expr::Word(w) => Ev::Word(w.clone()),
        Expr::Str(s) => Ev::Val(Val::Str(s.clone())),
        Expr::Bool(b) => Ev::Val(Val::Bool(*b)),
        Expr::Key(k) => match lookup(ctx, k) {
            Lookup::Found(v) => Ev::Val(v),
            Lookup::Absent => Ev::Absent,
            Lookup::Unknown => Ev::Tri(Tri::Unknown),
        },
        Expr::Neg(inner) => match num_of(&eval_ev(inner, ctx, depth + 1)) {
            Some(n) => Ev::Val(Val::Num(-n)),
            None => Ev::Tri(Tri::Unknown),
        },
        Expr::Bin(op, a, b) => {
            let x = eval_ev(a, ctx, depth + 1);
            match op {
                Op::And => {
                    let tx = truth(&x);
                    if tx == Tri::False {
                        return Ev::Tri(Tri::False);
                    }
                    let ty = truth(&eval_ev(b, ctx, depth + 1));
                    Ev::Tri(match (tx, ty) {
                        (_, Tri::False) => Tri::False,
                        (Tri::True, Tri::True) => Tri::True,
                        _ => Tri::Unknown,
                    })
                }
                Op::Or => {
                    let tx = truth(&x);
                    if tx == Tri::True {
                        return Ev::Tri(Tri::True);
                    }
                    let ty = truth(&eval_ev(b, ctx, depth + 1));
                    Ev::Tri(match (tx, ty) {
                        (_, Tri::True) => Tri::True,
                        (Tri::False, Tri::False) => Tri::False,
                        _ => Tri::Unknown,
                    })
                }
                Op::Eq | Op::Ne => {
                    let t = equal(&x, &eval_ev(b, ctx, depth + 1));
                    Ev::Tri(if *op == Op::Ne { t.not() } else { t })
                }
                Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                    let y = eval_ev(b, ctx, depth + 1);
                    match (num_of(&x), num_of(&y)) {
                        (Some(p), Some(q)) => Ev::Tri(Tri::from(match op {
                            Op::Lt => p < q,
                            Op::Le => p <= q,
                            Op::Gt => p > q,
                            _ => p >= q,
                        })),
                        _ => Ev::Tri(Tri::Unknown),
                    }
                }
                Op::Add | Op::Sub | Op::Mul | Op::Mod => {
                    let y = eval_ev(b, ctx, depth + 1);
                    let (Some(p), Some(q)) = (num_of(&x), num_of(&y)) else {
                        return Ev::Tri(Tri::Unknown);
                    };
                    let r = match op {
                        Op::Add => p + q,
                        Op::Sub => p - q,
                        Op::Mul => p * q,
                        _ if q == 0.0 => return Ev::Tri(Tri::Unknown),
                        _ => p % q,
                    };
                    if r.is_finite() { Ev::Val(Val::Num(r)) } else { Ev::Tri(Tri::Unknown) }
                }
            }
        }
        Expr::Call(name, args) => call(name, args, ctx, depth),
    }
}

fn present(ctx: &dyn Context, arg: Option<&Expr>) -> Tri {
    let Some(path) = arg.and_then(path_of) else {
        return Tri::Unknown;
    };
    match lookup(ctx, &path) {
        Lookup::Found(Val::Null) | Lookup::Absent => Tri::False,
        Lookup::Found(_) => Tri::True,
        Lookup::Unknown => Tri::Unknown,
    }
}

fn found(ctx: &dyn Context, arg: Option<&Expr>, depth: usize) -> Option<Val> {
    let a = arg?;
    match a {
        Expr::Word(w) => match lookup(ctx, w) {
            Lookup::Found(v) => Some(v),
            _ => None,
        },
        other => match eval_ev(other, ctx, depth + 1) {
            Ev::Val(v) => Some(v),
            _ => None,
        },
    }
}

fn call(name: &str, args: &[Expr], ctx: &dyn Context, depth: usize) -> Ev {
    let unknown = Ev::Tri(Tri::Unknown);
    match name {
        "fn:SinceVersion" | "fn:BeforeVersion" | "fn:IsPDFVersion" => version_gate(name, ctx, args, depth),
        "fn:Deprecated" => match args.get(1) {
            Some(inner) => eval_ev(inner, ctx, depth + 1),
            None => unknown,
        },
        "fn:Eval" => match args.first() {
            Some(inner) => eval_ev(inner, ctx, depth + 1),
            None => unknown,
        },
        "fn:Not" => match args.first() {
            Some(inner) => Ev::Tri(truth(&eval_ev(inner, ctx, depth + 1)).not()),
            None => unknown,
        },
        "fn:IsPresent" => {
            let p = present(ctx, args.first());
            match args.get(1) {
                // fn:IsPresent(key, condition): when key is present the condition must hold.
                Some(cond) => match p {
                    Tri::False => Ev::Tri(Tri::True),
                    Tri::True => Ev::Tri(truth(&eval_ev(cond, ctx, depth + 1))),
                    Tri::Unknown => unknown,
                },
                None => Ev::Tri(p),
            }
        }
        "fn:NotPresent" => Ev::Tri(present(ctx, args.first()).not()),
        "fn:ArrayLength" => match found(ctx, args.first(), depth) {
            Some(Val::Array { len, .. }) => Ev::Val(Val::Num(len as f64)),
            _ => unknown,
        },
        "fn:StringLength" => match found(ctx, args.first(), depth) {
            Some(Val::Str(s)) => Ev::Val(Val::Num(s.len() as f64)),
            _ => unknown,
        },
        "fn:RectWidth" | "fn:RectHeight" => match found(ctx, args.first(), depth) {
            Some(Val::Array { len: 4, items }) => {
                let n = |i: usize| match items.get(i) {
                    Some(Val::Num(x)) => Some(*x),
                    _ => None,
                };
                let (a, b) = if name == "fn:RectWidth" { (n(0), n(2)) } else { (n(1), n(3)) };
                match (a, b) {
                    (Some(a), Some(b)) => Ev::Val(Val::Num((b - a).abs())),
                    _ => unknown,
                }
            }
            _ => unknown,
        },
        "fn:FileSize" => match ctx.file_size() {
            Some(n) => Ev::Val(Val::Num(n as f64)),
            None => unknown,
        },
        "fn:Contains" => {
            let Some(hay) = found(ctx, args.first(), depth) else {
                return unknown;
            };
            let Some(needle) = args.get(1).map(|a| eval_ev(a, ctx, depth + 1)) else {
                return unknown;
            };
            match hay {
                Val::Array { len, items } => {
                    let mut any_unknown = items.len() < len;
                    for it in items {
                        match equal(&Ev::Val(it), &needle) {
                            Tri::True => return Ev::Tri(Tri::True),
                            Tri::Unknown => any_unknown = true,
                            Tri::False => {}
                        }
                    }
                    Ev::Tri(if any_unknown { Tri::Unknown } else { Tri::False })
                }
                v => Ev::Tri(equal(&Ev::Val(v), &needle)),
            }
        }
        _ => unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Ctx {
        v: Version,
        keys: HashMap<&'static str, Val>,
    }
    impl Context for Ctx {
        fn version(&self) -> Version {
            self.v
        }
        fn lookup(&self, path: &[&str]) -> Lookup {
            match self.keys.get(path.join("::").as_str()) {
                Some(v) => Lookup::Found(v.clone()),
                None => Lookup::Absent,
            }
        }
        fn file_size(&self) -> Option<u64> {
            Some(1000)
        }
    }

    fn ctx(v: &str, keys: &[(&'static str, Val)]) -> Ctx {
        Ctx { v: Version::parse(v).unwrap(), keys: keys.iter().cloned().collect() }
    }

    fn ev(s: &str, c: &Ctx) -> Tri {
        eval(&parse(s).unwrap(), c)
    }

    #[test]
    fn versions_and_presence() {
        let c = ctx("1.4", &[("Encrypt", Val::Dict), ("Type", Val::Name("Page".into()))]);
        assert_eq!(ev("fn:SinceVersion(2.0) || fn:IsPresent(Encrypt)", &c), Tri::True);
        assert_eq!(ev("fn:SinceVersion(2.0) || fn:IsPresent(ID)", &c), Tri::False);
        assert_eq!(ev("fn:SinceVersion(1.6,fn:IsPresent(RT))", &c), Tri::False);
        assert_eq!(ev("fn:BeforeVersion(1.5)", &c), Tri::True);
        assert_eq!(ev("fn:IsPDFVersion(1.4)", &c), Tri::True);
        assert_eq!(ev("@Type!=Template", &c), Tri::True);
        assert_eq!(ev("fn:Not(fn:IsPresent(Type))", &c), Tri::False);
        // Unknown predicates and parent paths stay unknown, but don't spoil a decided `||`.
        assert_eq!(ev("fn:SinceVersion(2.0) || fn:NotStandard14Font()", &c), Tri::Unknown);
        assert_eq!(ev("fn:IsPresent(Encrypt) || fn:NotStandard14Font()", &c), Tri::True);
        assert_eq!(ev("parent::@HalftoneType==5", &c), Tri::Unknown);
    }

    #[test]
    fn arithmetic_and_comparisons() {
        let c = ctx("2.0", &[("Rotate", Val::Num(270.0)), ("CA", Val::Num(1.5)), ("Length", Val::Num(40.0))]);
        assert_eq!(ev("(@Rotate mod 90)==0", &c), Tri::True);
        assert_eq!(ev("(@CA>=0.0) && (@CA<=1.0)", &c), Tri::False);
        assert_eq!(ev("(@Length>=40) && (@Length<=128) && ((@Length mod 8)==0)", &c), Tri::True);
        assert_eq!(ev("(@Length>=0) && (@Length<=fn:FileSize())", &c), Tri::True);
        assert_eq!(ev("@Length>=-1", &c), Tri::True);
        assert_eq!(ev("(@Rotate mod 0)==0", &c), Tri::Unknown);
    }

    #[test]
    fn arrays_and_names() {
        let arr = Val::Array { len: 2, items: vec![Val::Name("FlateDecode".into()), Val::Name("JPXDecode".into())] };
        let c = ctx("1.7", &[("Filter", arr), ("Version", Val::Name("1.4".into())), ("AP::N::Off", Val::Stream)]);
        assert_eq!(ev("fn:Contains(@Filter,JPXDecode)", &c), Tri::True);
        assert_eq!(ev("fn:ArrayLength(Filter)>1", &c), Tri::True);
        assert_eq!(ev("@Version==1.4", &c), Tri::True);
        assert_eq!(ev("fn:IsPresent(AP::N::Off)", &c), Tri::True);
    }

    #[test]
    fn hostile_expressions_are_errors() {
        assert!(parse(&"(".repeat(200)).is_err());
        assert!(parse(&"fn:Not(".repeat(100)).is_err());
        assert!(parse("'open").is_err());
        assert!(parse("@").is_err());
        assert!(parse(&"a || ".repeat(2000)).is_err());
        assert!(parse("fn:X(a b)").is_err());
        assert_eq!(parse("fn:Foo(1) && fn:SinceVersion(1.4)").unwrap().unsupported(), vec!["fn:Foo".to_string()]);
    }
}

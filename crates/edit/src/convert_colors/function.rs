//! PDF functions (ISO 32000-2 §7.10): the tint transforms of spot colours and the colour
//! functions of shadings, evaluated so their colours can be converted. Types 0 (sampled, up to
//! four inputs), 2 (exponential), 3 (stitching) and 4 (PostScript calculator) are read; all
//! bounded (depth, sizes, steps) and never panic on hostile input.

use pdfcraft_cos::{Document, Object};

/// Functions nested in stitching functions or arrays, at most this deep.
const MAX_DEPTH: usize = 8;
/// Sampled function: inputs, outputs and samples (all outputs) at most.
const MAX_INPUTS: usize = 4;
const MAX_OUTPUTS: usize = 32;
const MAX_SAMPLES: usize = 1 << 20;
/// PostScript calculator: program size, stack depth and operators run per evaluation.
const MAX_PROGRAM: usize = 64 << 10;
const MAX_STACK: usize = 100;
const MAX_STEPS: usize = 20_000;

#[derive(Clone, Debug)]
pub(super) enum Func {
    Sampled {
        domain: Vec<[f64; 2]>,
        range: Vec<[f64; 2]>,
        size: Vec<usize>,
        encode: Vec<[f64; 2]>,
        /// Decoded samples, `outputs` per grid point, first input varying fastest.
        samples: Vec<f64>,
    },
    Exponential {
        domain: [f64; 2],
        range: Option<Vec<[f64; 2]>>,
        c0: Vec<f64>,
        c1: Vec<f64>,
        n: f64,
    },
    Stitching {
        domain: [f64; 2],
        range: Option<Vec<[f64; 2]>>,
        functions: Vec<Func>,
        bounds: Vec<f64>,
        encode: Vec<[f64; 2]>,
    },
    PostScript {
        domain: Vec<[f64; 2]>,
        range: Vec<[f64; 2]>,
        program: Vec<Ps>,
    },
    /// An array of one-output functions, one per colour component (shadings).
    Array(Vec<Func>),
}

/// A PDF function read for evaluation, for callers outside Convert Colors (the Ink Manager's
/// spot-to-process preview evaluates tint transforms with it).
#[derive(Clone, Debug)]
pub struct Function(Func);

impl Function {
    /// Read a function object (Types 0, 2, 3, 4, or an array of them); `None` when it can't be
    /// evaluated.
    pub fn parse(doc: &Document, o: &Object) -> Option<Self> {
        Func::parse(doc, o, 0).map(Function)
    }

    /// Its outputs for `input` (clipped to its domain and range); `None` when evaluation fails.
    pub fn eval(&self, input: &[f64]) -> Option<Vec<f64>> {
        self.0.eval(input)
    }
}

fn finite(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

fn clip(v: f64, [a, b]: [f64; 2]) -> f64 {
    let v = finite(v);
    v.clamp(a.min(b), a.max(b))
}

fn interpolate(x: f64, [x0, x1]: [f64; 2], [y0, y1]: [f64; 2]) -> f64 {
    let span = x1 - x0;
    if span.abs() < 1e-12 { y0 } else { finite(y0 + (x - x0) * (y1 - y0) / span) }
}

fn nums(doc: &Document, o: Option<&Object>) -> Option<Vec<f64>> {
    let o = doc.resolve(o?);
    let a = o.as_array()?;
    if a.len() > 2 * MAX_OUTPUTS.max(64) {
        return None;
    }
    a.iter().map(|x| doc.resolve(x).as_f64().filter(|v| v.is_finite())).collect()
}

fn pairs(v: &[f64]) -> Vec<[f64; 2]> {
    v.as_chunks::<2>().0.to_vec()
}

impl Func {
    /// Read a function object (or an array of functions); `None` when it can't be evaluated.
    pub(super) fn parse(doc: &Document, o: &Object, depth: usize) -> Option<Func> {
        if depth > MAX_DEPTH {
            return None;
        }
        let obj = doc.resolve(o);
        if let Object::Array(list) = &*obj {
            if list.is_empty() || list.len() > MAX_OUTPUTS {
                return None;
            }
            return list.iter().map(|f| Func::parse(doc, f, depth + 1)).collect::<Option<Vec<_>>>().map(Func::Array);
        }
        let d = obj.as_dict()?;
        let domain = pairs(&nums(doc, d.get(b"Domain"))?);
        let range = nums(doc, d.get(b"Range")).map(|r| pairs(&r));
        match d.get(b"FunctionType").and_then(|t| doc.resolve(t).as_int())? {
            0 => {
                let Object::Stream(s) = &*obj else { return None };
                let range = range?;
                let m = domain.len();
                let n = range.len();
                if m == 0 || m > MAX_INPUTS || n == 0 || n > MAX_OUTPUTS {
                    return None;
                }
                let size: Vec<usize> = nums(doc, d.get(b"Size"))?
                    .into_iter()
                    .map(|v| if v >= 1.0 && v <= MAX_SAMPLES as f64 { Some(v as usize) } else { None })
                    .collect::<Option<_>>()?;
                if size.len() != m {
                    return None;
                }
                let points = size.iter().try_fold(1usize, |acc, s| acc.checked_mul(*s))?;
                let total = points.checked_mul(n)?;
                if total > MAX_SAMPLES {
                    return None;
                }
                let bps = d.get(b"BitsPerSample").and_then(|b| doc.resolve(b).as_int())?;
                if ![1, 2, 4, 8, 12, 16, 24, 32].contains(&bps) {
                    return None;
                }
                let bits = total.checked_mul(bps as usize)?;
                let data = s.decoded_within(bits.div_ceil(8).saturating_add(64)).ok()?;
                let encode = match nums(doc, d.get(b"Encode")) {
                    Some(e) if e.len() == 2 * m => pairs(&e),
                    _ => size.iter().map(|s| [0.0, s.saturating_sub(1) as f64]).collect(),
                };
                let decode = match nums(doc, d.get(b"Decode")) {
                    Some(e) if e.len() == 2 * n => pairs(&e),
                    _ => range.clone(),
                };
                let max = ((1u64 << bps) - 1) as f64;
                let mut samples = Vec::with_capacity(total);
                let mut reader = Bits::new(&data);
                for i in 0..total {
                    let raw = reader.read(bps as u32)? as f64;
                    let dec = decode.get(i % n).copied().unwrap_or([0.0, 1.0]);
                    samples.push(interpolate(raw, [0.0, max], dec));
                }
                Some(Func::Sampled { domain, range, size, encode, samples })
            }
            2 => {
                let domain = *domain.first()?;
                let c0 = nums(doc, d.get(b"C0")).unwrap_or_else(|| vec![0.0]);
                let c1 = nums(doc, d.get(b"C1")).unwrap_or_else(|| vec![1.0]);
                let n = d.get(b"N").and_then(|n| doc.resolve(n).as_f64()).filter(|n| n.is_finite())?;
                if c0.len() != c1.len() || c0.is_empty() || c0.len() > MAX_OUTPUTS {
                    return None;
                }
                Some(Func::Exponential { domain, range, c0, c1, n })
            }
            3 => {
                let domain = *domain.first()?;
                let list = doc.resolve(d.get(b"Functions")?);
                let list = list.as_array()?;
                if list.is_empty() || list.len() > 256 {
                    return None;
                }
                let functions = list.iter().map(|f| Func::parse(doc, f, depth + 1)).collect::<Option<Vec<_>>>()?;
                let bounds = nums(doc, d.get(b"Bounds")).unwrap_or_default();
                let encode = pairs(&nums(doc, d.get(b"Encode"))?);
                if bounds.len() + 1 != functions.len() || encode.len() != functions.len() {
                    return None;
                }
                Some(Func::Stitching { domain, range, functions, bounds, encode })
            }
            4 => {
                let Object::Stream(s) = &*obj else { return None };
                let range = range?;
                if domain.is_empty() || domain.len() > MAX_OUTPUTS || range.is_empty() || range.len() > MAX_OUTPUTS {
                    return None;
                }
                let code = s.decoded_within(MAX_PROGRAM).ok()?;
                let program = parse_ps(&code)?;
                Some(Func::PostScript { domain, range, program })
            }
            _ => None,
        }
    }

    /// The number of outputs, when known without evaluating.
    pub(super) fn outputs(&self) -> Option<usize> {
        match self {
            Func::Sampled { range, .. } | Func::PostScript { range, .. } => Some(range.len()),
            Func::Exponential { c0, .. } => Some(c0.len()),
            Func::Stitching { functions, .. } => functions.first()?.outputs(),
            Func::Array(list) => Some(list.len()),
        }
    }

    /// The input domain of the first input.
    pub(super) fn domain(&self) -> [f64; 2] {
        match self {
            Func::Sampled { domain, .. } | Func::PostScript { domain, .. } => domain.first().copied().unwrap_or([0.0, 1.0]),
            Func::Exponential { domain, .. } | Func::Stitching { domain, .. } => *domain,
            Func::Array(list) => list.first().map_or([0.0, 1.0], Func::domain),
        }
    }

    pub(super) fn eval(&self, input: &[f64]) -> Option<Vec<f64>> {
        self.eval_at(input, 0)
    }

    fn eval_at(&self, input: &[f64], depth: usize) -> Option<Vec<f64>> {
        if depth > MAX_DEPTH {
            return None;
        }
        let clip_range = |out: Vec<f64>, range: Option<&Vec<[f64; 2]>>| match range {
            Some(r) => out.iter().enumerate().map(|(i, v)| r.get(i).map_or(finite(*v), |r| clip(*v, *r))).collect(),
            None => out.into_iter().map(finite).collect(),
        };
        match self {
            Func::Array(list) => {
                let mut out = Vec::with_capacity(list.len());
                for f in list {
                    out.push(*f.eval_at(input, depth + 1)?.first()?);
                }
                Some(out)
            }
            Func::Exponential { domain, range, c0, c1, n } => {
                let x = clip(*input.first()?, *domain);
                let p = finite(x.powf(*n));
                let out = c0.iter().zip(c1).map(|(a, b)| a + p * (b - a)).collect();
                Some(clip_range(out, range.as_ref()))
            }
            Func::Stitching { domain, range, functions, bounds, encode } => {
                let x = clip(*input.first()?, *domain);
                let k = bounds.iter().take_while(|b| x >= **b).count();
                let lo = if k == 0 { domain[0] } else { bounds.get(k - 1).copied().unwrap_or(domain[0]) };
                let hi = bounds.get(k).copied().unwrap_or(domain[1]);
                let e = encode.get(k).copied().unwrap_or([0.0, 1.0]);
                let out = functions.get(k)?.eval_at(&[interpolate(x, [lo, hi], e)], depth + 1)?;
                Some(clip_range(out, range.as_ref()))
            }
            Func::Sampled { domain, range, size, encode, samples } => {
                let n = range.len();
                let m = domain.len();
                // Position in sample space per input, and the cell it falls in.
                let mut cell = Vec::with_capacity(m);
                for i in 0..m {
                    let d = domain.get(i).copied().unwrap_or([0.0, 1.0]);
                    let x = clip(input.get(i).copied().unwrap_or(d[0]), d);
                    let s = size.get(i).copied().unwrap_or(1);
                    let e = interpolate(x, d, encode.get(i).copied().unwrap_or([0.0, s.saturating_sub(1) as f64]));
                    let e = clip(e, [0.0, s.saturating_sub(1) as f64]);
                    let lo = e.floor() as usize;
                    let hi = (lo + 1).min(s.saturating_sub(1));
                    cell.push((lo, hi, e - e.floor()));
                }
                let mut out = vec![0.0; n];
                // Multilinear interpolation over the 2^m corners.
                for corner in 0..(1usize << m) {
                    let mut weight = 1.0;
                    let mut index = 0usize;
                    let mut stride = 1usize;
                    for (i, (lo, hi, f)) in cell.iter().enumerate() {
                        let up = corner >> i & 1 == 1;
                        weight *= if up { *f } else { 1.0 - f };
                        index = index.checked_add(if up { *hi } else { *lo }.checked_mul(stride)?)?;
                        stride = stride.checked_mul(size.get(i).copied().unwrap_or(1))?;
                    }
                    if weight == 0.0 {
                        continue;
                    }
                    for (j, o) in out.iter_mut().enumerate() {
                        *o += weight * samples.get(index.checked_mul(n)?.checked_add(j)?).copied().unwrap_or(0.0);
                    }
                }
                Some(out.iter().enumerate().map(|(i, v)| clip(*v, range.get(i).copied().unwrap_or([0.0, 1.0]))).collect())
            }
            Func::PostScript { domain, range, program } => {
                let mut stack: Vec<Val> = Vec::with_capacity(16);
                for (i, d) in domain.iter().enumerate() {
                    stack.push(Val::Num(clip(input.get(i).copied().unwrap_or(d[0]), *d)));
                }
                let mut steps = 0;
                run_ps(program, &mut stack, &mut steps, 0)?;
                if stack.len() < range.len() {
                    return None;
                }
                let tail = stack.get(stack.len() - range.len()..)?;
                tail.iter().zip(range).map(|(v, r)| Some(clip(v.num()?, *r))).collect()
            }
        }
    }
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read(&mut self, bits: u32) -> Option<u64> {
        let mut v = 0u64;
        for _ in 0..bits {
            let byte = *self.data.get(self.pos / 8)?;
            let bit = (byte >> (7 - (self.pos % 8))) & 1;
            v = (v << 1) | u64::from(bit);
            self.pos += 1;
        }
        Some(v)
    }
}

// ---- PostScript calculator (ISO 32000-2 §7.10.5) ----------------------------------------

#[derive(Clone, Debug)]
pub(super) enum Ps {
    Int(i64),
    Num(f64),
    Bool(bool),
    Op(Vec<u8>),
    /// `{ then } if` and `{ then } { else } ifelse`.
    If(Vec<Ps>, Option<Vec<Ps>>),
}

#[derive(Clone, Copy, Debug)]
enum Val {
    Num(f64),
    Int(i64),
    Bool(bool),
}

impl Val {
    fn num(self) -> Option<f64> {
        match self {
            Val::Num(v) => Some(v),
            Val::Int(i) => Some(i as f64),
            Val::Bool(_) => None,
        }
    }
    fn int(self) -> Option<i64> {
        match self {
            Val::Int(i) => Some(i),
            Val::Num(v) if v.fract() == 0.0 && v.abs() < 1e15 => Some(v as i64),
            _ => None,
        }
    }
}

fn parse_ps(code: &[u8]) -> Option<Vec<Ps>> {
    let mut pos = 0;
    // Skip to the outer `{`.
    while code.get(pos).is_some_and(|b| *b != b'{') {
        pos += 1;
    }
    pos += 1;
    let (prog, _) = parse_block(code, pos, 0)?;
    Some(prog)
}

/// Parse until the matching `}`; returns the block and the position after it.
fn parse_block(code: &[u8], mut pos: usize, depth: usize) -> Option<(Vec<Ps>, usize)> {
    if depth > 32 {
        return None;
    }
    let mut out: Vec<Ps> = Vec::new();
    let mut pending: Vec<Vec<Ps>> = Vec::new();
    loop {
        while code.get(pos).is_some_and(|b| b.is_ascii_whitespace()) {
            pos += 1;
        }
        let b = *code.get(pos)?;
        match b {
            b'}' => {
                if !pending.is_empty() {
                    return None;
                }
                return Some((out, pos + 1));
            }
            b'{' => {
                let (block, next) = parse_block(code, pos + 1, depth + 1)?;
                pending.push(block);
                if pending.len() > 2 {
                    return None;
                }
                pos = next;
            }
            b'%' => {
                while code.get(pos).is_some_and(|b| *b != b'\n' && *b != b'\r') {
                    pos += 1;
                }
            }
            _ => {
                let start = pos;
                while code.get(pos).is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'{' | b'}' | b'%')) {
                    pos += 1;
                }
                let tok = code.get(start..pos)?;
                if tok.is_empty() {
                    return None;
                }
                let item = match tok {
                    b"true" => Ps::Bool(true),
                    b"false" => Ps::Bool(false),
                    b"if" => {
                        let then = pending.pop()?;
                        if !pending.is_empty() {
                            return None;
                        }
                        Ps::If(then, None)
                    }
                    b"ifelse" => {
                        let otherwise = pending.pop()?;
                        let then = pending.pop()?;
                        Ps::If(then, Some(otherwise))
                    }
                    _ => {
                        let text = std::str::from_utf8(tok).ok();
                        match (text.and_then(|s| s.parse::<i64>().ok()), text.and_then(|s| s.parse::<f64>().ok())) {
                            (Some(i), _) => Ps::Int(i),
                            (None, Some(v)) if v.is_finite() => Ps::Num(v),
                            _ => Ps::Op(tok.to_vec()),
                        }
                    }
                };
                if !pending.is_empty() {
                    return None;
                }
                out.push(item);
                if out.len() > MAX_PROGRAM {
                    return None;
                }
            }
        }
    }
}

fn run_ps(program: &[Ps], st: &mut Vec<Val>, steps: &mut usize, depth: usize) -> Option<()> {
    if depth > 32 {
        return None;
    }
    for item in program {
        *steps += 1;
        if *steps > MAX_STEPS || st.len() > MAX_STACK {
            return None;
        }
        match item {
            Ps::Int(i) => st.push(Val::Int(*i)),
            Ps::Num(v) => st.push(Val::Num(*v)),
            Ps::Bool(b) => st.push(Val::Bool(*b)),
            Ps::If(then, otherwise) => {
                let Val::Bool(c) = st.pop()? else { return None };
                if c {
                    run_ps(then, st, steps, depth + 1)?;
                } else if let Some(e) = otherwise {
                    run_ps(e, st, steps, depth + 1)?;
                }
            }
            Ps::Op(op) => ps_op(op, st)?,
        }
    }
    Some(())
}

fn ps_op(op: &[u8], st: &mut Vec<Val>) -> Option<()> {
    let num = |v: f64| if v.is_finite() { Some(Val::Num(v)) } else { None };
    macro_rules! pop {
        () => {
            st.pop()?
        };
    }
    match op {
        b"add" | b"sub" | b"mul" | b"div" | b"atan" | b"exp" => {
            let b = pop!();
            let a = pop!();
            let r = match op {
                b"add" | b"sub" | b"mul" => match (a, b) {
                    (Val::Int(x), Val::Int(y)) => {
                        let v = match op {
                            b"add" => x.checked_add(y),
                            b"sub" => x.checked_sub(y),
                            _ => x.checked_mul(y),
                        };
                        Val::Int(v?)
                    }
                    _ => {
                        let (x, y) = (a.num()?, b.num()?);
                        num(match op {
                            b"add" => x + y,
                            b"sub" => x - y,
                            _ => x * y,
                        })?
                    }
                },
                b"div" => {
                    let y = b.num()?;
                    if y == 0.0 {
                        return None;
                    }
                    num(a.num()? / y)?
                }
                b"atan" => {
                    let (n, d) = (a.num()?, b.num()?);
                    let mut deg = n.atan2(d).to_degrees();
                    if deg < 0.0 {
                        deg += 360.0;
                    }
                    num(deg)?
                }
                _ => num(a.num()?.powf(b.num()?))?,
            };
            st.push(r);
        }
        b"idiv" | b"mod" => {
            let b = pop!().int()?;
            let a = pop!().int()?;
            if b == 0 {
                return None;
            }
            st.push(Val::Int(if op == b"idiv" { a.checked_div(b)? } else { a.checked_rem(b)? }));
        }
        b"neg" => {
            let a = pop!();
            st.push(match a {
                Val::Int(i) => Val::Int(i.checked_neg()?),
                other => num(-other.num()?)?,
            });
        }
        b"abs" => {
            let a = pop!();
            st.push(match a {
                Val::Int(i) => Val::Int(i.checked_abs()?),
                other => num(other.num()?.abs())?,
            });
        }
        b"ceiling" | b"floor" | b"round" | b"truncate" => {
            let a = pop!();
            st.push(match a {
                Val::Int(i) => Val::Int(i),
                other => {
                    let v = other.num()?;
                    num(match op {
                        b"ceiling" => v.ceil(),
                        b"floor" => v.floor(),
                        b"round" => (v + 0.5).floor(),
                        _ => v.trunc(),
                    })?
                }
            });
        }
        b"sqrt" | b"sin" | b"cos" | b"ln" | b"log" | b"cvr" => {
            let v = pop!().num()?;
            let r = match op {
                b"sqrt" if v >= 0.0 => v.sqrt(),
                b"sin" => v.to_radians().sin(),
                b"cos" => v.to_radians().cos(),
                b"ln" if v > 0.0 => v.ln(),
                b"log" if v > 0.0 => v.log10(),
                b"cvr" => v,
                _ => return None,
            };
            st.push(num(r)?);
        }
        b"cvi" => {
            let v = pop!().num()?.trunc();
            if v.abs() > 1e15 {
                return None;
            }
            st.push(Val::Int(v as i64));
        }
        b"eq" | b"ne" | b"gt" | b"ge" | b"lt" | b"le" => {
            let b = pop!();
            let a = pop!();
            let r = match (a, b) {
                (Val::Bool(x), Val::Bool(y)) => match op {
                    b"eq" => x == y,
                    b"ne" => x != y,
                    _ => return None,
                },
                _ => {
                    let (x, y) = (a.num()?, b.num()?);
                    match op {
                        b"eq" => x == y,
                        b"ne" => x != y,
                        b"gt" => x > y,
                        b"ge" => x >= y,
                        b"lt" => x < y,
                        _ => x <= y,
                    }
                }
            };
            st.push(Val::Bool(r));
        }
        b"and" | b"or" | b"xor" => {
            let b = pop!();
            let a = pop!();
            st.push(match (a, b) {
                (Val::Bool(x), Val::Bool(y)) => Val::Bool(match op {
                    b"and" => x && y,
                    b"or" => x || y,
                    _ => x ^ y,
                }),
                _ => {
                    let (x, y) = (a.int()?, b.int()?);
                    Val::Int(match op {
                        b"and" => x & y,
                        b"or" => x | y,
                        _ => x ^ y,
                    })
                }
            });
        }
        b"not" => {
            let a = pop!();
            st.push(match a {
                Val::Bool(b) => Val::Bool(!b),
                other => Val::Int(!other.int()?),
            });
        }
        b"bitshift" => {
            let s = pop!().int()?;
            let a = pop!().int()?;
            if s.abs() > 62 {
                return None;
            }
            st.push(Val::Int(if s >= 0 { a.checked_shl(s as u32)? } else { a >> (-s) }));
        }
        b"true" => st.push(Val::Bool(true)),
        b"false" => st.push(Val::Bool(false)),
        b"pop" => {
            pop!();
        }
        b"exch" => {
            let b = pop!();
            let a = pop!();
            st.push(b);
            st.push(a);
        }
        b"dup" => {
            let a = *st.last()?;
            st.push(a);
        }
        b"copy" => {
            let n = usize::try_from(pop!().int()?).ok()?;
            if n > st.len() || st.len() + n > MAX_STACK {
                return None;
            }
            let from = st.len() - n;
            let tail: Vec<Val> = st.get(from..)?.to_vec();
            st.extend(tail);
        }
        b"index" => {
            let n = usize::try_from(pop!().int()?).ok()?;
            let at = st.len().checked_sub(n.checked_add(1)?)?;
            let v = *st.get(at)?;
            st.push(v);
        }
        b"roll" => {
            let j = pop!().int()?;
            let n = usize::try_from(pop!().int()?).ok()?;
            if n > st.len() {
                return None;
            }
            if n > 0 {
                let from = st.len() - n;
                let slice = st.get_mut(from..)?;
                let k = j.rem_euclid(n as i64) as usize;
                slice.rotate_right(k);
            }
        }
        _ => return None,
    }
    Some(())
}

/// A Type 0 sampled function object (16-bit samples) for `f` over `domain` (one or two
/// inputs), `size` points per input, with outputs clipped to 0–1 or the given ranges.
pub(super) fn sampled_stream(domain: &[[f64; 2]], size: usize, range: &[[f64; 2]], samples: &[Vec<f64>]) -> pdfcraft_cos::Stream {
    use pdfcraft_cos::{Dict, Stream};
    let flat = |v: &[[f64; 2]]| Object::Array(v.iter().flat_map(|p| [Object::Real(p[0]), Object::Real(p[1])]).collect());
    let mut data = Vec::with_capacity(samples.len().saturating_mul(range.len()).saturating_mul(2));
    for s in samples {
        for (i, r) in range.iter().enumerate() {
            let v = s.get(i).copied().unwrap_or(0.0);
            let span = r[1] - r[0];
            let t = if span.abs() < 1e-12 { 0.0 } else { ((v - r[0]) / span).clamp(0.0, 1.0) };
            data.extend(((t * 65535.0).round() as u16).to_be_bytes());
        }
    }
    let mut d = Dict::new();
    d.set(b"FunctionType".to_vec(), Object::Int(0));
    d.set(b"Domain".to_vec(), flat(domain));
    d.set(b"Range".to_vec(), flat(range));
    d.set(b"Size".to_vec(), Object::Array(domain.iter().map(|_| Object::Int(size as i64)).collect()));
    d.set(b"BitsPerSample".to_vec(), Object::Int(16));
    Stream::flate(d, &data)
}

//! Prepare a form ▸ automatic field detection: find where a printed form expects answers —
//! underscore runs, rules (horizontal lines), empty boxes, small squares, rows of touching cells
//! (comb fields), small circles (radio buttons, grouped by row or column) and the empty cells of
//! tables ruled with lines — and name each new field from the label next to it (Acrobat's
//! auto-naming; table cells from their column heading and row).
//!
//! [`detect`] is a pure function over the page's words and drawn shapes; [`page_shapes`] reads
//! the shapes from a page's content.

use pdfcraft_content::{Matrix, parse};
use pdfcraft_cos::{Document, Object};

/// A piece of text on the page, in user space.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    /// [x0, y0, x1, y1], y up.
    pub rect: [f64; 4],
}

/// Shapes drawn on the page, in user space: rectangles (`re`), straight horizontal segments
/// (`rules`), straight vertical segments (`columns`, as [x, y0, x, y1]) and small closed curves
/// (`circles`, their bounding boxes).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shapes {
    pub boxes: Vec<[f64; 4]>,
    pub rules: Vec<[f64; 4]>,
    pub columns: Vec<[f64; 4]>,
    pub circles: Vec<[f64; 4]>,
}

/// At most this many shapes of each kind are read from one page (a hostile or generated page
/// with millions of segments stops there).
pub const MAX_SHAPES: usize = 20_000;

/// What detection proposes.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Text,
    CheckBox,
    /// A text field spread over a row of `cells` touching boxes (a comb of characters).
    Comb {
        cells: usize,
    },
    /// One button of radio group `group` (buttons of one group share it and their name):
    /// `export` is this button's value (from the label next to it).
    Radio {
        group: usize,
        export: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub kind: Kind,
    pub rect: [f64; 4],
    /// From the nearby label, made a valid field name.
    pub name: String,
}

/// The rectangles and horizontal segments a page draws (stroked or filled), in user space.
pub fn page_shapes(doc: &Document, page: usize) -> Shapes {
    let mut out = Shapes::default();
    let Some(p) = pdfcraft_model::pages(doc).into_iter().nth(page) else { return out };
    let list: Vec<Object> = match p.dict.get(b"Contents").map(|c| doc.resolve(c)) {
        Some(c) => match &*c {
            Object::Array(a) => a.clone(),
            Object::Stream(_) => vec![p.dict.get(b"Contents").cloned().unwrap_or(Object::Null)],
            _ => Vec::new(),
        },
        None => Vec::new(),
    };
    let mut data = Vec::new();
    for o in list {
        if let Object::Stream(s) = &*doc.resolve(&o) {
            data.extend(s.decoded().unwrap_or_default());
            data.push(b'\n');
        }
    }
    let mut ctm = Matrix::IDENTITY;
    let mut stack = Vec::new();
    let mut path_boxes: Vec<[f64; 4]> = Vec::new();
    let mut path_rules: Vec<[f64; 4]> = Vec::new();
    let mut path_columns: Vec<[f64; 4]> = Vec::new();
    let mut cur: Option<[f64; 2]> = None;
    // The current path's curves: their points' bounding box, and how many segments it has.
    let mut curve: Option<[f64; 4]> = None;
    let grow = |b: &mut Option<[f64; 4]>, p: [f64; 2]| {
        *b = Some(match *b {
            None => [p[0], p[1], p[0], p[1]],
            Some(r) => [r[0].min(p[0]), r[1].min(p[1]), r[2].max(p[0]), r[3].max(p[1])],
        });
    };
    let cap = |v: &mut Vec<[f64; 4]>, r: [f64; 4]| {
        if v.len() < MAX_SHAPES && r.iter().all(|x| x.is_finite()) {
            v.push(r);
        }
    };
    for op in parse(&data).ops {
        match op.op.as_slice() {
            b"q" => {
                if stack.len() < 256 {
                    stack.push(ctm);
                }
            }
            b"Q" => ctm = stack.pop().unwrap_or(ctm),
            b"cm" => {
                if let Some(m) = op.nums::<6>() {
                    ctm = Matrix(m).then(&ctm);
                }
            }
            b"re" => {
                if let Some([x, y, w, h]) = op.nums::<4>() {
                    path_boxes.push(ctm.bbox([x.min(x + w), y.min(y + h), x.max(x + w), y.max(y + h)]));
                }
            }
            b"m" => {
                cur = op.nums::<2>().map(|[x, y]| {
                    let (a, b) = ctm.apply(x, y);
                    [a, b]
                })
            }
            b"l" => {
                if let (Some(a), Some([x, y])) = (cur, op.nums::<2>()) {
                    let (bx, by) = ctm.apply(x, y);
                    let b = [bx, by];
                    if (a[1] - b[1]).abs() < 0.5 && (a[0] - b[0]).abs() > 1.0 {
                        path_rules.push([a[0].min(b[0]), a[1], a[0].max(b[0]), a[1]]);
                    } else if (a[0] - b[0]).abs() < 0.5 && (a[1] - b[1]).abs() > 1.0 {
                        path_columns.push([a[0], a[1].min(b[1]), a[0], a[1].max(b[1])]);
                    }
                    if curve.is_some() {
                        grow(&mut curve, b);
                    }
                    cur = Some(b);
                }
            }
            b"c" | b"v" | b"y" => {
                let pts: Vec<f64> = op.operands.iter().filter_map(|o| o.as_f64()).collect();
                if let Some(start) = cur
                    && pts.len() >= 4
                {
                    grow(&mut curve, start);
                    for p in pts.chunks(2) {
                        if let [x, y] = p {
                            let (a, b) = ctm.apply(*x, *y);
                            grow(&mut curve, [a, b]);
                        }
                    }
                    if let [.., x, y] = pts[..] {
                        let (a, b) = ctm.apply(x, y);
                        cur = Some([a, b]);
                    }
                }
            }
            b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" => {
                for r in path_boxes.drain(..) {
                    // A very flat filled rectangle is a rule; a very narrow one a column line.
                    if r[3] - r[1] < 1.5 && r[2] - r[0] > 1.0 {
                        cap(&mut out.rules, [r[0], (r[1] + r[3]) / 2.0, r[2], (r[1] + r[3]) / 2.0]);
                    } else if r[2] - r[0] < 1.5 && r[3] - r[1] > 1.0 {
                        cap(&mut out.columns, [(r[0] + r[2]) / 2.0, r[1], (r[0] + r[2]) / 2.0, r[3]]);
                    } else {
                        cap(&mut out.boxes, r);
                    }
                }
                for r in path_rules.drain(..) {
                    cap(&mut out.rules, r);
                }
                for r in path_columns.drain(..) {
                    cap(&mut out.columns, r);
                }
                if let Some(c) = curve.take() {
                    cap(&mut out.circles, c);
                }
                cur = None;
            }
            b"n" => {
                path_boxes.clear();
                path_rules.clear();
                path_columns.clear();
                curve = None;
                cur = None;
            }
            _ => {}
        }
    }
    out
}

fn mid(r: [f64; 4]) -> f64 {
    (r[1] + r[3]) / 2.0
}

/// A label's words made into a field name: letters, digits and spaces, no trailing colon.
fn clean(label: &str) -> String {
    let s: String = label.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '/' { c } else { ' ' }).collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    s.chars().take(40).collect::<String>().trim().to_string()
}

/// The label for a field at `rect`: the words to its left on the same line, else the words
/// just above it.
fn label_for(words: &[Word], rect: [f64; 4], right_of: bool) -> String {
    let h = (rect[3] - rect[1]).max(8.0);
    let same_line = |w: &Word| (mid(w.rect) - mid(rect)).abs() < h.max(10.0) * 0.7;
    let mut left: Vec<&Word> = if right_of {
        words.iter().filter(|w| same_line(w) && w.rect[0] >= rect[2] - 1.0 && w.rect[0] - rect[2] < 200.0).collect()
    } else {
        words.iter().filter(|w| same_line(w) && w.rect[2] <= rect[0] + 1.0 && rect[0] - w.rect[2] < 250.0).collect()
    };
    left.sort_by(|a, b| a.rect[0].total_cmp(&b.rect[0]));
    if right_of {
        // The words that follow the box, up to the next gap wider than a few spaces.
        let mut picked = Vec::new();
        let mut x = rect[2];
        for w in left {
            if w.rect[0] - x > 24.0 {
                break;
            }
            picked.push(w.text.as_str());
            x = w.rect[2];
        }
        return clean(&picked.join(" "));
    }
    // The words that end nearest the field, back to a wide gap.
    let mut picked: Vec<&str> = Vec::new();
    let mut x = rect[0];
    for w in left.iter().rev() {
        if x - w.rect[2] > 24.0 && !picked.is_empty() {
            break;
        }
        if w.text.chars().all(|c| c == '_') {
            break;
        }
        picked.insert(0, w.text.as_str());
        x = w.rect[0];
    }
    let l = clean(&picked.join(" "));
    if !l.is_empty() {
        return l;
    }
    // Above: the nearest line of words over the field's left part.
    let above: Vec<&Word> = words
        .iter()
        .filter(|w| w.rect[1] >= rect[3] - 1.0 && w.rect[1] - rect[3] < 24.0 && w.rect[0] < rect[2] && w.rect[2] > rect[0] - 4.0)
        .collect();
    let Some(line) = above.iter().map(|w| mid(w.rect)).min_by(|a, b| a.total_cmp(b)) else { return String::new() };
    let mut on: Vec<&&Word> = above.iter().filter(|w| (mid(w.rect) - line).abs() < 3.0).collect();
    on.sort_by(|a, b| a.rect[0].total_cmp(&b.rect[0]));
    clean(&on.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "))
}

/// Rows of four or more touching boxes of one size on one line: comb fields. Returns each
/// comb's rectangle and cell count, and marks the boxes used.
fn combs(boxes: &[[f64; 4]], used: &mut [bool]) -> Vec<([f64; 4], usize)> {
    let cell = |r: &[f64; 4]| {
        let (w, h) = (r[2] - r[0], r[3] - r[1]);
        (6.0..=36.0).contains(&w) && (8.0..=40.0).contains(&h)
    };
    let mut order: Vec<usize> = (0..boxes.len()).filter(|i| boxes.get(*i).is_some_and(cell)).collect();
    // Line by line (top first), then left to right.
    order.sort_by(|a, b| {
        let (ra, rb) = (boxes[*a], boxes[*b]);
        rb[1].total_cmp(&ra[1]).then(ra[0].total_cmp(&rb[0]))
    });
    let mut out = Vec::new();
    let mut k = 0;
    while k < order.len() {
        let first = boxes[order[k]];
        let mut run = vec![order[k]];
        let mut j = k + 1;
        while let Some(&n) = order.get(j) {
            let (prev, next) = (boxes[*run.last().unwrap_or(&n)], boxes[n]);
            let same_line = (next[1] - first[1]).abs() < 1.0 && (next[3] - first[3]).abs() < 1.0;
            let same_size = ((next[2] - next[0]) - (first[2] - first[0])).abs() < 1.5;
            let touching = (-1.0..=2.0).contains(&(next[0] - prev[2]));
            if !(same_line && same_size && touching) {
                break;
            }
            run.push(n);
            j += 1;
        }
        if run.len() >= 4 && run.len() <= 256 {
            let last = boxes[*run.last().unwrap_or(&order[k])];
            out.push(([first[0], first[1], last[2], first[3]], run.len()));
            for i in &run {
                if let Some(u) = used.get_mut(*i) {
                    *u = true;
                }
            }
        }
        k = j.max(k + 1);
    }
    out
}

/// Radio buttons: small circles grouped with their neighbours on one row (or in one column).
/// Returns each group's buttons, top to bottom and left to right.
fn radio_groups(circles: &[[f64; 4]]) -> Vec<Vec<[f64; 4]>> {
    let round: Vec<[f64; 4]> = circles
        .iter()
        .copied()
        .filter(|r| {
            let (w, h) = (r[2] - r[0], r[3] - r[1]);
            (5.0..=20.0).contains(&w) && (5.0..=20.0).contains(&h) && (w / h - 1.0).abs() < 0.25
        })
        .take(2000)
        .collect();
    let mut group_of: Vec<usize> = (0..round.len()).collect();
    fn root(g: &mut [usize], mut i: usize) -> usize {
        let mut steps = 0;
        while let Some(&p) = g.get(i) {
            if p == i || steps > g.len() {
                break;
            }
            i = p;
            steps += 1;
        }
        i
    }
    for a in 0..round.len() {
        for b in a + 1..round.len() {
            let (ra, rb) = (round[a], round[b]);
            let row = (mid(ra) - mid(rb)).abs() < 4.0 && (ra[0] - rb[0]).abs() < 260.0;
            let column = (ra[0] - rb[0]).abs() < 3.0 && (mid(ra) - mid(rb)).abs() < 30.0;
            if row || column {
                let (x, y) = (root(&mut group_of, a), root(&mut group_of, b));
                if let Some(g) = group_of.get_mut(x.max(y)) {
                    *g = x.min(y);
                }
            }
        }
    }
    let mut groups: Vec<(usize, Vec<[f64; 4]>)> = Vec::new();
    for (i, r) in round.iter().enumerate() {
        let g = root(&mut group_of, i);
        match groups.iter_mut().find(|(k, _)| *k == g) {
            Some((_, list)) => list.push(*r),
            None => groups.push((g, vec![*r])),
        }
    }
    groups
        .into_iter()
        .map(|(_, mut list)| {
            list.sort_by(|a, b| mid(*b).total_cmp(&mid(*a)).then(a[0].total_cmp(&b[0])));
            list
        })
        .collect()
}

/// Tables ruled with lines: rows between horizontal rules of one width, columns at the vertical
/// lines crossing them. Returns the empty data cells (the first row is the heading) with their
/// names, and marks the rules used.
fn table_cells(rules: &[[f64; 4]], columns: &[[f64; 4]], labels: &[Word], used: &mut [bool]) -> Vec<([f64; 4], String)> {
    let mut out = Vec::new();
    let wide: Vec<usize> = (0..rules.len().min(2000)).filter(|i| rules.get(*i).is_some_and(|r| r[2] - r[0] >= 60.0)).collect();
    let mut taken = vec![false; rules.len()];
    for &i in &wide {
        if taken.get(i).copied().unwrap_or(true) {
            continue;
        }
        let r = rules[i];
        // Rules of this table: same left and right ends.
        let mut rows: Vec<usize> = wide.iter().copied().filter(|j| (rules[*j][0] - r[0]).abs() < 3.0 && (rules[*j][2] - r[2]).abs() < 3.0).collect();
        // Each table is looked at once, from its first rule.
        for j in &rows {
            if let Some(t) = taken.get_mut(*j) {
                *t = true;
            }
        }
        rows.sort_by(|a, b| rules[*b][1].total_cmp(&rules[*a][1]));
        rows.dedup_by(|a, b| (rules[*a][1] - rules[*b][1]).abs() < 1.0);
        if rows.len() < 3 {
            continue;
        }
        let (top, bottom) = (rules[rows[0]][1], rules[rows[rows.len() - 1]][1]);
        // Column lines inside the table that span most of it.
        let mut xs: Vec<f64> = columns
            .iter()
            .take(MAX_SHAPES)
            .filter(|c| c[0] > r[0] + 2.0 && c[0] < r[2] - 2.0 && c[1] <= bottom + 2.0 && c[3] >= top - 2.0)
            .map(|c| c[0])
            .collect();
        xs.extend([r[0], r[2]]);
        xs.sort_by(f64::total_cmp);
        xs.dedup_by(|a, b| (*a - *b).abs() < 2.0);
        if xs.len() < 3 {
            continue;
        }
        let bands: Vec<(f64, f64)> = rows.windows(2).map(|w| (rules[w[1]][1], rules[w[0]][1])).collect();
        if bands.iter().any(|(lo, hi)| !(10.0..=80.0).contains(&(hi - lo))) {
            continue;
        }
        for j in &rows {
            if let Some(u) = used.get_mut(*j) {
                *u = true;
            }
        }
        let inside = |cell: [f64; 4]| -> Vec<&Word> {
            labels.iter().filter(|w| w.rect[0] < cell[2] && w.rect[2] > cell[0] && mid(w.rect) > cell[1] && mid(w.rect) < cell[3]).collect()
        };
        let text = |cell: [f64; 4]| clean(&inside(cell).iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "));
        let headings: Vec<String> = xs.windows(2).map(|c| bands.first().map(|(lo, hi)| text([c[0], *lo, c[1], *hi])).unwrap_or_default()).collect();
        for (row, (lo, hi)) in bands.iter().enumerate().skip(1) {
            for (col, c) in xs.windows(2).enumerate() {
                let cell = [c[0] + 1.0, lo + 1.0, c[1] - 1.0, hi - 1.0];
                if cell[2] - cell[0] < 20.0 || !inside(cell).is_empty() {
                    continue;
                }
                let heading = headings.get(col).cloned().unwrap_or_default();
                let name = if heading.is_empty() { String::new() } else { format!("{heading} Row{row}") };
                out.push((cell, name));
            }
        }
    }
    out
}

/// Propose fields for a page. `existing` are the page's widget rectangles (no candidate
/// overlaps them); names are unique against `taken`.
pub fn detect(words: &[Word], shapes: &Shapes, existing: &[[f64; 4]], taken: &[String]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let overlaps = |a: [f64; 4], b: [f64; 4]| a[0] < b[2] - 1.0 && b[0] < a[2] - 1.0 && a[1] < b[3] - 1.0 && b[1] < a[3] - 1.0;
    let push = |kind: Kind, rect: [f64; 4], label: String, out: &mut Vec<Candidate>| {
        if existing.iter().chain(out.iter().map(|c| &c.rect)).any(|e| overlaps(*e, rect)) {
            return;
        }
        out.push(Candidate { kind, rect, name: label });
    };
    // Text that isn't underscores (labels), and underscore runs (blanks).
    let (blanks, labels): (Vec<&Word>, Vec<&Word>) = words.iter().partition(|w| w.text.len() >= 3 && w.text.chars().all(|c| c == '_'));
    let labels: Vec<Word> = labels.into_iter().cloned().collect();
    for b in blanks {
        let h = (b.rect[3] - b.rect[1]).max(10.0);
        let rect = [b.rect[0], b.rect[1], b.rect[2], b.rect[1] + (h * 1.4).clamp(12.0, 22.0)];
        let l = label_for(&labels, rect, false);
        push(Kind::Text, rect, l, &mut out);
    }
    // Rows of touching cells: comb fields (before check boxes, whose size the cells share).
    let mut box_used = vec![false; shapes.boxes.len()];
    for (rect, cells) in combs(&shapes.boxes, &mut box_used) {
        if !labels.iter().any(|x| overlaps(x.rect, rect)) {
            let l = label_for(&labels, rect, false);
            push(Kind::Comb { cells }, rect, l, &mut out);
        }
    }
    // Small circles: radio groups, named from the label before the first button (the
    // question), each button's value from the label after it.
    for (g, group) in radio_groups(&shapes.circles).into_iter().enumerate() {
        let Some(first) = group.first().copied() else { continue };
        if group.len() < 2 {
            let l = label_for(&labels, first, true);
            push(Kind::CheckBox, first, l, &mut out);
            continue;
        }
        let question = label_for(&labels, first, false);
        let question = if question.is_empty() { "Radio Group".to_string() } else { question };
        for (n, r) in group.iter().enumerate() {
            let export = label_for(&labels, *r, true);
            let export = if export.is_empty() { format!("Choice{}", n + 1) } else { export };
            push(Kind::Radio { group: g, export }, *r, question.clone(), &mut out);
        }
    }
    // Small squares: check boxes, labelled by the text to their right.
    for (i, r) in shapes.boxes.iter().enumerate() {
        if box_used.get(i).copied().unwrap_or(false) {
            continue;
        }
        let (w, h) = (r[2] - r[0], r[3] - r[1]);
        if (5.0..=20.0).contains(&w) && (5.0..=20.0).contains(&h) && (w / h - 1.0).abs() < 0.25 {
            let l = label_for(&labels, *r, true);
            push(Kind::CheckBox, *r, l, &mut out);
        }
    }
    // Empty boxes big enough to write in: text fields inside them.
    for r in &shapes.boxes {
        let (w, h) = (r[2] - r[0], r[3] - r[1]);
        let has_text = labels.iter().any(|x| overlaps(x.rect, *r));
        if w >= 40.0 && (12.0..=60.0).contains(&h) && !has_text {
            let inner = [r[0] + 1.0, r[1] + 1.0, r[2] - 1.0, r[3] - 1.0];
            let l = label_for(&labels, *r, false);
            push(Kind::Text, inner, l, &mut out);
        }
    }
    // Tables ruled with lines: a text field per empty data cell.
    let mut rule_used = vec![false; shapes.rules.len()];
    for (rect, name) in table_cells(&shapes.rules, &shapes.columns, &labels, &mut rule_used) {
        push(Kind::Text, rect, name, &mut out);
    }
    // Rules: a text field sitting on the line.
    for (i, r) in shapes.rules.iter().enumerate() {
        let w = r[2] - r[0];
        if w < 40.0 || rule_used.get(i).copied().unwrap_or(false) {
            continue;
        }
        // A line that underlines text (or a table border with text on it) isn't a blank.
        let rect = [r[0], r[1] + 1.0, r[2], r[1] + 17.0];
        if labels.iter().any(|x| overlaps(x.rect, rect)) {
            continue;
        }
        let l = label_for(&labels, rect, false);
        push(Kind::Text, rect, l, &mut out);
    }
    // Reading order: top to bottom, left to right.
    // Rows: fields whose middles are within 8 pt of the row's first.
    out.sort_by(|a, b| mid(b.rect).total_cmp(&mid(a.rect)));
    let mut rows = Vec::with_capacity(out.len());
    let (mut row, mut start) = (0usize, f64::INFINITY);
    for c in &out {
        if start - mid(c.rect) > 8.0 {
            row += 1;
            start = mid(c.rect);
        }
        rows.push(row);
    }
    let mut keyed: Vec<(usize, Candidate)> = rows.into_iter().zip(out).collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.rect[0].total_cmp(&b.1.rect[0])));
    let mut out: Vec<Candidate> = keyed.into_iter().map(|(_, c)| c).collect();
    // Unique names (Acrobat numbers repeats); unlabelled fields get Text1, Check Box1, …
    // The buttons of one radio group share their group's name.
    let mut used: Vec<String> = taken.to_vec();
    let mut groups: Vec<(usize, String)> = Vec::new();
    for c in &mut out {
        if let Kind::Radio { group, .. } = c.kind
            && let Some((_, name)) = groups.iter().find(|(g, _)| *g == group)
        {
            c.name = name.clone();
            continue;
        }
        let base = if c.name.is_empty() {
            match c.kind {
                Kind::Text | Kind::Comb { .. } => "Text".to_string(),
                Kind::CheckBox => "Check Box".to_string(),
                Kind::Radio { .. } => "Radio Group".to_string(),
            }
        } else {
            c.name.clone()
        };
        let mut name = if c.name.is_empty() { format!("{base}1") } else { base.clone() };
        let mut n = 1;
        while used.contains(&name) {
            n += 1;
            name = format!("{base}{n}");
        }
        used.push(name.clone());
        if let Kind::Radio { group, .. } = c.kind {
            groups.push((group, name.clone()));
        }
        c.name = name;
    }
    out
}

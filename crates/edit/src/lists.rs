//! Edit a PDF ▸ bulleted and numbered lists.
//!
//! A list lives in an added text box ([`crate::added`]): each paragraph of the box (a line of its
//! text, split at `\n`) may be a list item with a [`ListStyle`] and a nesting level. Labels are
//! not part of the text; they are generated when the box is drawn, so numbering always follows
//! the paragraphs: a paragraph added inside a list continues it, and removing one renumbers the
//! rest. Items are drawn with a hanging indent: the label at the item's indent and every line of
//! the paragraph at the body indent past it.
//!
//! Existing page paragraphs become a list by moving them into such a box
//! ([`paragraphs_to_list`]): their source text operators are removed and the paragraphs are
//! drawn again in the closest standard font, as one editable box.
//!
//! The drawn label and body of every item are wrapped in `/PCList` marked content naming their
//! role, list, item and level, so a tagged document's structure can be brought in step with
//! `L` / `LI` / `Lbl` / `LBody` elements (`pdfcraft_a11y::keep_list_tags`).

use pdfcraft_cos::{Dict, Document, Object};

use crate::added::{AddedText, Content, Family};
use crate::text::TextBlock;
use crate::{EditError, page_list};

/// Deepest nesting level (0 is the outermost).
pub const MAX_LIST_LEVEL: u8 = 8;
/// Largest first number of a numbered list.
pub const MAX_LIST_START: u32 = 100_000;
/// Most paragraphs one list edit (or one box's list) covers.
pub const MAX_LIST_PARAGRAPHS: usize = 10_000;

/// A bullet glyph. Every one is in WinAnsiEncoding, so it is drawn in the box's own standard font
/// and extracted as the character it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bullet {
    /// • U+2022
    Disc,
    /// – U+2013
    Dash,
    /// » U+00BB
    Arrow,
    /// * U+002A
    Asterisk,
    /// · U+00B7
    Dot,
}

/// A numbering style; labels end in a full stop (`1.`, `a.`, `A.`, `i.`, `I.`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Numbering {
    Decimal,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ListStyle {
    Bullet(Bullet),
    Number(Numbering),
}

impl ListStyle {
    /// Every style, by its name.
    pub const ALL: [ListStyle; 10] = [
        ListStyle::Bullet(Bullet::Disc),
        ListStyle::Bullet(Bullet::Dash),
        ListStyle::Bullet(Bullet::Arrow),
        ListStyle::Bullet(Bullet::Asterisk),
        ListStyle::Bullet(Bullet::Dot),
        ListStyle::Number(Numbering::Decimal),
        ListStyle::Number(Numbering::LowerAlpha),
        ListStyle::Number(Numbering::UpperAlpha),
        ListStyle::Number(Numbering::LowerRoman),
        ListStyle::Number(Numbering::UpperRoman),
    ];

    /// The style's name (`disc`, `dash`, `arrow`, `asterisk`, `dot`, `decimal`, `lower-alpha`,
    /// `upper-alpha`, `lower-roman`, `upper-roman`).
    pub fn name(self) -> &'static str {
        match self {
            ListStyle::Bullet(Bullet::Disc) => "disc",
            ListStyle::Bullet(Bullet::Dash) => "dash",
            ListStyle::Bullet(Bullet::Arrow) => "arrow",
            ListStyle::Bullet(Bullet::Asterisk) => "asterisk",
            ListStyle::Bullet(Bullet::Dot) => "dot",
            ListStyle::Number(Numbering::Decimal) => "decimal",
            ListStyle::Number(Numbering::LowerAlpha) => "lower-alpha",
            ListStyle::Number(Numbering::UpperAlpha) => "upper-alpha",
            ListStyle::Number(Numbering::LowerRoman) => "lower-roman",
            ListStyle::Number(Numbering::UpperRoman) => "upper-roman",
        }
    }

    pub fn from_name(name: &str) -> Option<ListStyle> {
        ListStyle::ALL.into_iter().find(|s| s.name() == name)
    }

    pub fn numbered(self) -> bool {
        matches!(self, ListStyle::Number(_))
    }
}

/// One paragraph's list formatting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListItem {
    pub style: ListStyle,
    /// Nesting level, 0 (outermost) to [`MAX_LIST_LEVEL`].
    pub level: u8,
    /// A numbered item that restarts the numbering at this value (otherwise it continues from the
    /// item before it at its level, or starts at 1).
    pub start: Option<u32>,
}

/// A change to the list formatting of some paragraphs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListChange {
    /// Make the paragraphs list items in `style` (changing the type of those already in a list;
    /// their levels stay). `start` restarts the numbering at the first of them.
    Set { style: ListStyle, start: Option<u32> },
    /// Make them ordinary paragraphs again.
    Remove,
    /// Change their nesting level by this many levels (negative: outdent), within 0 to
    /// [`MAX_LIST_LEVEL`].
    Indent(i8),
}

/// Which paragraphs a list edit applies to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListTarget {
    /// Paragraphs `paragraphs` (0-based, inclusive; `None`: all) of added text box `index` (among
    /// the page's added items).
    Added { index: usize, paragraphs: Option<[usize; 2]> },
    /// `count` consecutive page paragraphs from `first` (indexes into [`crate::text_blocks`]):
    /// they move into a new list box. Only [`ListChange::Set`] applies.
    Paragraphs { first: usize, count: usize },
}

/// The paragraphs of a text box's text.
pub(crate) fn paragraphs(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n')
}

/// The box's list formatting, one entry per paragraph (padded or cut to the paragraph count).
pub fn items(t: &AddedText) -> Vec<Option<ListItem>> {
    let n = paragraphs(&t.text).count();
    let mut out: Vec<Option<ListItem>> = t.list.iter().take(n).copied().collect();
    out.resize(n, None);
    out
}

fn alpha(mut n: u32, upper: bool) -> String {
    // Bijective base 26: 1 → a, 26 → z, 27 → aa.
    let mut out = Vec::new();
    while n > 0 && out.len() < 8 {
        n -= 1;
        let base = if upper { b'A' } else { b'a' };
        out.push(char::from(base + (n % 26) as u8));
        n /= 26;
    }
    out.iter().rev().collect()
}

fn roman(n: u32, upper: bool) -> String {
    if !(1..=3999).contains(&n) {
        return n.to_string();
    }
    const TABLE: [(u32, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut rest = n;
    let mut out = String::new();
    for (value, digits) in TABLE {
        while rest >= value {
            out.push_str(digits);
            rest -= value;
        }
    }
    if upper { out.to_ascii_uppercase() } else { out }
}

/// The label for number `n` in `style`.
pub fn number_label(style: Numbering, n: u32) -> String {
    let body = match style {
        Numbering::Decimal => n.to_string(),
        Numbering::LowerAlpha => alpha(n, false),
        Numbering::UpperAlpha => alpha(n, true),
        Numbering::LowerRoman => roman(n, false),
        Numbering::UpperRoman => roman(n, true),
    };
    format!("{body}.")
}

fn bullet_label(b: Bullet) -> &'static str {
    match b {
        Bullet::Disc => "\u{2022}",
        Bullet::Dash => "\u{2013}",
        Bullet::Arrow => "\u{00BB}",
        Bullet::Asterisk => "*",
        Bullet::Dot => "\u{00B7}",
    }
}

/// Each paragraph's label (`None` for ordinary paragraphs). Numbering counts the items at one
/// level of a run of consecutive list paragraphs: an item continues from the previous numbered
/// item at its level (deeper items in between don't interrupt it), a shallower item ends the
/// deeper levels' counts, an ordinary paragraph ends the list, and `start` restarts the count.
pub fn labels(items: &[Option<ListItem>]) -> Vec<Option<String>> {
    let mut counters: [Option<u32>; MAX_LIST_LEVEL as usize + 1] = [None; MAX_LIST_LEVEL as usize + 1];
    items
        .iter()
        .map(|item| {
            let Some(item) = item else {
                counters = [None; MAX_LIST_LEVEL as usize + 1];
                return None;
            };
            let level = usize::from(item.level.min(MAX_LIST_LEVEL));
            for c in counters.iter_mut().skip(level + 1) {
                *c = None;
            }
            match item.style {
                ListStyle::Bullet(b) => {
                    if let Some(c) = counters.get_mut(level) {
                        *c = None;
                    }
                    Some(bullet_label(b).to_owned())
                }
                ListStyle::Number(style) => {
                    let previous = counters.get(level).copied().flatten();
                    let n = item.start.unwrap_or_else(|| previous.map_or(1, |p| p.saturating_add(1))).clamp(1, MAX_LIST_START.saturating_mul(10));
                    if let Some(c) = counters.get_mut(level) {
                        *c = Some(n);
                    }
                    Some(number_label(style, n))
                }
            }
        })
        .collect()
}

/// The list formatting for `new_text` after retyping a box whose text was `old_text`: paragraphs
/// kept at the start and end keep theirs, and new paragraphs typed between them take the
/// formatting of the paragraph they were typed after (or into), so pressing Enter inside a list
/// continues it. A paragraph that keeps its start number in the middle loses it to the first.
pub fn reconcile(old_text: &str, old: &[Option<ListItem>], new_text: &str) -> Vec<Option<ListItem>> {
    let old_p: Vec<&str> = paragraphs(old_text).collect();
    let new_p: Vec<&str> = paragraphs(new_text).collect();
    let item = |i: usize| old.get(i).copied().flatten();
    let prefix = old_p.iter().zip(&new_p).take_while(|(a, b)| a == b).count();
    let max_suffix = old_p.len().min(new_p.len()).saturating_sub(prefix);
    let suffix = old_p.iter().rev().zip(new_p.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
    let old_mid = prefix..old_p.len().saturating_sub(suffix);
    let new_mid_len = new_p.len().saturating_sub(suffix).saturating_sub(prefix);
    let mut out: Vec<Option<ListItem>> = (0..prefix).map(item).collect();
    // The paragraph being edited: the first changed one, else the one before the insertion.
    let carried = if old_mid.is_empty() { prefix.checked_sub(1).and_then(item) } else { item(old_mid.start) };
    for j in 0..new_mid_len {
        let from_old = old_mid.start.checked_add(j).filter(|i| old_mid.contains(i)).and_then(item);
        let mut it = if old_mid.is_empty() { carried } else { from_old.or(carried) };
        // Only the first paragraph of the edit can keep a restart.
        if j > 0
            && let Some(i) = it.as_mut()
            && from_old.is_none()
        {
            i.start = None;
        }
        out.push(it);
    }
    out.extend((old_p.len().saturating_sub(suffix)..old_p.len()).map(item));
    out.truncate(new_p.len());
    out.resize(new_p.len(), None);
    trimmed(out)
}

/// Without trailing ordinary paragraphs (nothing stored when a box has no list).
fn trimmed(mut list: Vec<Option<ListItem>>) -> Vec<Option<ListItem>> {
    while list.last().is_some_and(Option::is_none) {
        list.pop();
    }
    list
}

fn range_of(t: &AddedText, paragraphs: Option<[usize; 2]>) -> Result<std::ops::RangeInclusive<usize>, EditError> {
    let n = self::paragraphs(&t.text).count();
    let [first, last] = paragraphs.unwrap_or([0, n.saturating_sub(1)]);
    if first > last || last >= n {
        return Err(EditError::Invalid(format!(
            "paragraphs {}–{} are out of range: the text box has {n} paragraph{}",
            first.saturating_add(1),
            last.saturating_add(1),
            if n == 1 { "" } else { "s" }
        )));
    }
    if n > MAX_LIST_PARAGRAPHS {
        return Err(EditError::Invalid(format!("lists take text boxes with at most {MAX_LIST_PARAGRAPHS} paragraphs")));
    }
    Ok(first..=last)
}

fn check_start(start: Option<u32>) -> Result<(), EditError> {
    if start.is_some_and(|s| !(1..=MAX_LIST_START).contains(&s)) {
        return Err(EditError::Invalid(format!("start must be a whole number from 1 to {MAX_LIST_START}")));
    }
    Ok(())
}

/// The box `t` with `change` applied to `paragraphs` (0-based, inclusive; `None`: all of them).
/// Refuses a change that can't apply (paragraphs out of range, removing or indenting paragraphs
/// none of which is in a list) with what is allowed; a change that leaves everything as it was
/// returns an equal box.
pub fn change_list(t: &AddedText, paragraphs: Option<[usize; 2]>, change: &ListChange) -> Result<AddedText, EditError> {
    let range = range_of(t, paragraphs)?;
    if t.text.chars().any(crate::added::is_arabic) {
        return Err(EditError::Invalid("lists are not available for text boxes with Arabic text yet".into()));
    }
    let mut list = items(t);
    match *change {
        ListChange::Set { style, start } => {
            check_start(start)?;
            for (k, i) in range.enumerate() {
                if let Some(slot) = list.get_mut(i) {
                    let level = slot.map_or(0, |it| it.level);
                    let keep = slot.and_then(|it| it.start);
                    let start = match start {
                        Some(s) if k == 0 => Some(s),
                        Some(_) => None,
                        None => keep.filter(|_| style.numbered()),
                    };
                    *slot = Some(ListItem { style, level, start });
                }
            }
        }
        ListChange::Remove => {
            if !list.get(range.clone()).unwrap_or_default().iter().any(Option::is_some) {
                return Err(EditError::Invalid("none of those paragraphs is in a list".into()));
            }
            for i in range {
                if let Some(slot) = list.get_mut(i) {
                    *slot = None;
                }
            }
        }
        ListChange::Indent(by) => {
            if !list.get(range.clone()).unwrap_or_default().iter().any(Option::is_some) {
                return Err(EditError::Invalid("none of those paragraphs is in a list; make them a list first".into()));
            }
            for i in range {
                if let Some(Some(it)) = list.get_mut(i) {
                    it.level = (i16::from(it.level) + i16::from(by)).clamp(0, i16::from(MAX_LIST_LEVEL)) as u8;
                }
            }
        }
    }
    Ok(AddedText { list: trimmed(list), ..t.clone() })
}

/// Whether `change` on `target` would change the document (a request already satisfied adds no
/// undo step). Invalid requests are errors.
pub fn list_edit_changes(doc: &Document, page: usize, target: &ListTarget, change: &ListChange) -> Result<bool, EditError> {
    match target {
        ListTarget::Added { index, paragraphs } => {
            let t = added_text(doc, page, *index)?;
            Ok(change_list(&t, *paragraphs, change)? != t)
        }
        ListTarget::Paragraphs { first, count } => {
            selected_blocks(doc, page, *first, *count, change)?;
            Ok(true)
        }
    }
}

fn added_text(doc: &Document, page: usize, index: usize) -> Result<AddedText, EditError> {
    let item = crate::list_added(doc).into_iter().filter(|a| a.page == page).nth(index);
    match item.map(|a| a.content) {
        Some(Content::Text(t)) => Ok(t),
        Some(Content::Image(_)) => Err(EditError::Invalid(format!("added item {} on page {} is an image, not text", index + 1, page + 1))),
        None => Err(EditError::Invalid(format!("page {} has no added item {}", page + 1, index + 1))),
    }
}

/// Apply a list edit (see [`ListTarget`] and [`ListChange`]). Returns the list box's index among
/// the page's added items. On an error the document is unchanged.
pub fn edit_list(doc: &mut Document, page: usize, target: &ListTarget, change: &ListChange) -> Result<usize, EditError> {
    let mut next = doc.clone();
    let index = match target {
        ListTarget::Added { index, paragraphs } => {
            let t = added_text(&next, page, *index)?;
            let changed = change_list(&t, *paragraphs, change)?;
            crate::update_content(&mut next, page, *index, &Content::Text(changed))?;
            *index
        }
        ListTarget::Paragraphs { first, count } => paragraphs_to_list(&mut next, page, *first, *count, change)?,
    };
    *doc = next;
    Ok(index)
}

/// A leading bullet or number the paragraph already shows (`• `, `- `, `3. `, `b) `, `iv. `),
/// which the generated label replaces.
fn strip_marker(text: &str) -> &str {
    let t = text.trim_start();
    for b in ['\u{2022}', '\u{2013}', '\u{00BB}', '\u{00B7}', '*', '-', '\u{25AA}', '\u{25CF}', '\u{25E6}'] {
        if let Some(rest) = t.strip_prefix(b)
            && rest.starts_with(' ')
        {
            return rest.trim_start();
        }
    }
    let marker_len = t.find(['.', ')']).filter(|&i| {
        let head = t.get(..i).unwrap_or_default();
        (1..=6).contains(&head.len())
            && (head.chars().all(|c| c.is_ascii_digit()) || (head.len() == 1 && head.chars().all(|c| c.is_ascii_alphabetic())) || is_roman(head))
    });
    if let Some(i) = marker_len
        && let Some(rest) = t.get(i + 1..)
        && rest.starts_with(' ')
    {
        return rest.trim_start();
    }
    t
}

/// A roman numeral written the canonical way (`iv`, not `iiii`; `mild` is a word).
fn is_roman(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if s != lower && s != s.to_ascii_uppercase() {
        return false;
    }
    let value = |c: char| match c {
        'i' => Some(1u32),
        'v' => Some(5),
        'x' => Some(10),
        'l' => Some(50),
        'c' => Some(100),
        'd' => Some(500),
        'm' => Some(1000),
        _ => None,
    };
    let Some(digits) = lower.chars().map(value).collect::<Option<Vec<u32>>>() else { return false };
    let mut total = 0i64;
    for (i, d) in digits.iter().enumerate() {
        if digits.get(i + 1).is_some_and(|next| next > d) {
            total -= i64::from(*d);
        } else {
            total += i64::from(*d);
        }
    }
    u32::try_from(total).is_ok_and(|t| roman(t, false) == lower)
}

/// The page paragraphs a conversion takes, checked.
fn selected_blocks(doc: &Document, page: usize, first: usize, count: usize, change: &ListChange) -> Result<Vec<TextBlock>, EditError> {
    if !matches!(change, ListChange::Set { .. }) {
        return Err(EditError::Invalid(
            "page paragraphs that aren't in a list can only be made into one (text_list_set); remove or indent the items of a list box".into(),
        ));
    }
    if let ListChange::Set { start, .. } = change {
        check_start(*start)?;
    }
    if count == 0 || count > MAX_LIST_PARAGRAPHS {
        return Err(EditError::Invalid(format!("choose 1 to {MAX_LIST_PARAGRAPHS} paragraphs")));
    }
    let blocks = crate::text_blocks(doc, page)?;
    let end = first.checked_add(count).filter(|e| *e <= blocks.len()).ok_or_else(|| {
        EditError::Invalid(format!(
            "paragraphs {}–{} are out of range: page {} has {} paragraphs",
            first.saturating_add(1),
            first.saturating_add(count),
            page + 1,
            blocks.len()
        ))
    })?;
    let selected = blocks.get(first..end).unwrap_or_default().to_vec();
    let lines = crate::text_lines(doc, page)?;
    for (k, b) in selected.iter().enumerate() {
        let (upright, plain) = crate::text::upright_and_plain(&lines, b);
        if !upright {
            return Err(EditError::Invalid(format!(
                "paragraph {} is turned, skewed or reflected; lists can be made from upright paragraphs only",
                first + k + 1
            )));
        }
        if !plain {
            return Err(EditError::Invalid(format!(
                "paragraph {} uses stroking, invisible text, clipping or an extended graphics state; it is preserved unchanged",
                first + k + 1
            )));
        }
    }
    Ok(selected)
}

/// Move `count` page paragraphs from `first` into a new added text box formatted as a list (only
/// [`ListChange::Set`]): the paragraphs' text operators leave the page and the box is drawn where
/// they were, in the closest standard font (family, weight and slant from the first paragraph's
/// font name) at the first paragraph's size and colour. A bullet or number a paragraph already
/// starts with is replaced by the list's label. Returns the box's index among the page's added
/// items.
pub fn paragraphs_to_list(doc: &mut Document, page: usize, first: usize, count: usize, change: &ListChange) -> Result<usize, EditError> {
    let selected = selected_blocks(doc, page, first, count, change)?;
    let ListChange::Set { style, start } = *change else {
        return Err(EditError::Invalid("page paragraphs can only be made into a list".into()));
    };
    let lead = selected.first().ok_or_else(|| EditError::Invalid("choose at least one paragraph".into()))?;
    let p = page_list(doc).into_iter().nth(page).ok_or(EditError::NoSuchPage(page))?;
    // Display space → user space; the box is placed in display space, so the page must show its
    // user space upright.
    let [va, vb, vc, vd, ve, vf] = p.view_matrix(doc);
    if !(va > 0.0 && vd > 0.0 && vb.abs() < 1e-9 && vc.abs() < 1e-9) {
        return Err(EditError::Invalid("lists can't be made from paragraphs on a rotated page yet".into()));
    }
    let to_display = |x: f64, y: f64| ((x - ve) / va, (y - vf) / vd);
    let union = selected.iter().map(|b| b.rect).reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]).unwrap_or(lead.rect);
    let (x0, y0) = to_display(union[0], union[1]);
    let (x1, y1) = to_display(union[2], union[3]);
    let size = if lead.size.is_finite() { lead.size.clamp(1.0, 500.0) } else { 12.0 };
    let text: Vec<&str> = selected.iter().map(|b| strip_marker(&b.text)).collect();
    let family: Family = crate::text::source_family(&lead.base_font);
    let width = (x1 - x0).abs().max(size * 6.0);
    // The first baseline is drawn 0.95 em below the box top; the source line's top is about its
    // ascent above the baseline.
    let top = y0.max(y1);
    let mut t = AddedText {
        rect: [x0.min(x1), y0.min(y1), x0.min(x1) + width, top],
        text: text.join("\n"),
        family,
        bold: lead.bold,
        italic: lead.italic,
        size,
        color: lead.color.map(|c| if c.is_finite() { c.clamp(0.0, 1.0) } else { 0.0 }),
        ..AddedText::default()
    };
    if let Some(c) = crate::first_undrawable(&t) {
        return Err(EditError::Invalid(format!("the paragraphs have a character ({c:?}) the standard fonts can't show; they were left as they are")));
    }
    t = change_list(&t, None, &ListChange::Set { style, start })?;
    // Remove the source paragraphs, the last first so earlier indexes stay put; each removal is
    // checked against the paragraph it was meant to take.
    for k in (0..selected.len()).rev() {
        let lines = crate::text_lines(doc, page)?;
        let blocks = crate::text::group_blocks_pub(&lines);
        let at = first.saturating_add(k);
        let (Some(b), Some(want)) = (blocks.get(at), selected.get(k)) else {
            return Err(EditError::Invalid("the paragraphs moved while they were being made into a list; nothing was changed".into()));
        };
        if b.text != want.text {
            return Err(EditError::Invalid("the paragraphs could not be separated from the text around them; nothing was changed".into()));
        }
        crate::text::remove_block(doc, page, &lines, b)?;
    }
    crate::add_content(doc, page, &Content::Text(t))
}

/// The `/List` array stored with an added text box (`None` when it has no list).
pub(crate) fn list_param(t: &AddedText) -> Option<Object> {
    let list = trimmed(items(t));
    if list.is_empty() {
        return None;
    }
    Some(Object::Array(
        list.iter()
            .map(|it| match it {
                None => Object::Null,
                Some(it) => {
                    let mut d = Dict::new();
                    d.set(b"Style".to_vec(), Object::name(it.style.name()));
                    d.set(b"Level".to_vec(), Object::Int(i64::from(it.level)));
                    if let Some(s) = it.start {
                        d.set(b"Start".to_vec(), Object::Int(i64::from(s)));
                    }
                    Object::Dict(d)
                }
            })
            .collect(),
    ))
}

/// Read a stored `/List` array leniently: unknown styles are ordinary paragraphs, levels and
/// starts are clamped, and at most [`MAX_LIST_PARAGRAPHS`] entries are read.
pub(crate) fn parse_list(doc: &Document, o: Option<&Object>) -> Vec<Option<ListItem>> {
    let Some(o) = o else { return Vec::new() };
    let resolved = doc.resolve(o);
    let Some(a) = resolved.as_array() else { return Vec::new() };
    let list = a
        .iter()
        .take(MAX_LIST_PARAGRAPHS)
        .map(|e| {
            let e = doc.resolve(e);
            let d = e.as_dict()?;
            let style = ListStyle::from_name(std::str::from_utf8(d.name(b"Style")?).ok()?)?;
            let level = d.int(b"Level").unwrap_or(0).clamp(0, i64::from(MAX_LIST_LEVEL)) as u8;
            let start = d.int(b"Start").and_then(|s| u32::try_from(s.clamp(1, i64::from(MAX_LIST_START))).ok());
            Some(ListItem { style, level, start })
        })
        .collect();
    trimmed(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(style: ListStyle, level: u8) -> Option<ListItem> {
        Some(ListItem { style, level, start: None })
    }

    const DEC: ListStyle = ListStyle::Number(Numbering::Decimal);
    const DISC: ListStyle = ListStyle::Bullet(Bullet::Disc);

    #[test]
    fn labels_number_by_level_and_restart() {
        let lower = ListStyle::Number(Numbering::LowerAlpha);
        let items = [item(DEC, 0), item(lower, 1), item(lower, 1), item(DEC, 0), None, item(DEC, 0), item(DISC, 1)];
        let got: Vec<Option<String>> = labels(&items);
        let want = [Some("1."), Some("a."), Some("b."), Some("2."), None, Some("1."), Some("\u{2022}")];
        assert_eq!(got, want.map(|s| s.map(str::to_owned)));
        let restarted = [Some(ListItem { style: DEC, level: 0, start: Some(7) }), item(DEC, 0)];
        assert_eq!(labels(&restarted), [Some("7.".to_owned()), Some("8.".to_owned())]);
    }

    #[test]
    fn number_styles() {
        assert_eq!(number_label(Numbering::LowerRoman, 14), "xiv.");
        assert_eq!(number_label(Numbering::UpperRoman, 1994), "MCMXCIV.");
        assert_eq!(number_label(Numbering::UpperRoman, 4000), "4000.");
        assert_eq!(number_label(Numbering::LowerAlpha, 27), "aa.");
        assert_eq!(number_label(Numbering::UpperAlpha, 26), "Z.");
        assert_eq!(number_label(Numbering::UpperAlpha, u32::MAX).len(), 8);
    }

    #[test]
    fn new_paragraphs_continue_the_list_they_are_typed_in() {
        let old = [item(DEC, 0), item(DEC, 1), None];
        // Enter at the end of the second item.
        assert_eq!(reconcile("a\nb\nc", &old, "a\nb\nnew\nc"), vec![item(DEC, 0), item(DEC, 1), item(DEC, 1)]);
        // A paragraph removed.
        assert_eq!(reconcile("a\nb\nc", &old, "a\nc"), vec![item(DEC, 0)]);
        // Everything retyped keeps the items in order.
        assert_eq!(reconcile("a\nb", &old[..2], "x\ny\nz"), vec![item(DEC, 0), item(DEC, 1), item(DEC, 0)]);
        assert_eq!(reconcile("", &[], "x"), Vec::<Option<ListItem>>::new());
    }

    #[test]
    fn markers_already_typed_are_replaced() {
        assert_eq!(strip_marker("• Apples"), "Apples");
        assert_eq!(strip_marker("12. Pears"), "Pears");
        assert_eq!(strip_marker("b) Plums"), "Plums");
        assert_eq!(strip_marker("iv. Figs"), "Figs");
        assert_eq!(strip_marker("Dr. Who"), "Dr. Who");
        assert_eq!(strip_marker("3.5 litres"), "3.5 litres");
        assert_eq!(strip_marker("é. x"), "é. x");
        assert_eq!(strip_marker("mild. weather"), "mild. weather");
        assert_eq!(strip_marker("IV. Kings"), "Kings");
    }
}

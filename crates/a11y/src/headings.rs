//! Headings in the structure tree (H, H1–H6, and PDF 2.0's Hn), for New Bookmarks from
//! Structure: each heading's level, its alternate or actual text, where it starts and the
//! marked content it covers (whose text the caller extracts from the page).
//!
//! Works on [`Tags`], which is already bounded and cycle-safe; the walks here visit each node at
//! most once.

use crate::tags::{TagKid, Tags};

/// The highest heading level read from a type name (`H200` reads as this).
pub const MAX_HEADING_LEVEL: usize = 64;

/// One heading, in document order.
#[derive(Clone, Debug, PartialEq)]
pub struct Heading {
    /// Index into [`Tags::nodes`].
    pub node: usize,
    /// 1-based: `H1`–`H6` (or `Hn`) give theirs; a plain `H` is one level deeper than the
    /// sections (Sect, Part, Art) around it, at least 1.
    pub level: usize,
    /// The heading's own non-blank `/ActualText`, else its `/Alt`.
    pub text: Option<String>,
    /// 0-based page where the heading starts (its first marked content, else its `/Pg`).
    pub page: Option<usize>,
    /// Its marked content and that of the elements inside it, in order: (page, MCID). Content
    /// in another stream (a form XObject's MCIDs) is left out.
    pub content: Vec<(usize, i64)>,
}

/// The level of a heading type, if it is one.
pub fn heading_level(role: &str) -> Option<HeadingKind> {
    let rest = role.strip_prefix('H')?;
    if rest.is_empty() {
        return Some(HeadingKind::Nested);
    }
    if !rest.bytes().all(|b| b.is_ascii_digit()) || rest.starts_with('0') {
        return None;
    }
    let n = rest.parse::<usize>().unwrap_or(MAX_HEADING_LEVEL);
    Some(HeadingKind::Numbered(n.clamp(1, MAX_HEADING_LEVEL)))
}

/// A heading type: numbered (`H1`…) or a plain `H`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeadingKind {
    Numbered(usize),
    /// `H`: the level comes from the nesting of sections.
    Nested,
}

fn non_blank(s: &Option<String>) -> Option<String> {
    s.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string)
}

/// Every heading in the tree, in document order.
pub fn headings(tags: &Tags) -> Vec<Heading> {
    let mut out = Vec::new();
    for (i, node) in tags.nodes.iter().enumerate() {
        let Some(kind) = heading_level(node.role()) else { continue };
        let level = match kind {
            HeadingKind::Numbered(n) => n,
            HeadingKind::Nested => {
                let sections = tags.ancestors(i).filter(|a| tags.nodes.get(*a).is_some_and(|n| matches!(n.role(), "Sect" | "Part" | "Art"))).count();
                sections.clamp(1, MAX_HEADING_LEVEL)
            }
        };
        let content = content_of(tags, i);
        let page = content.first().map(|c| c.0).or(node.page);
        out.push(Heading { node: i, level, text: non_blank(&node.actual_text).or_else(|| non_blank(&node.alt)), page, content });
    }
    out
}

/// The marked content of node `i` and its descendants, in order (page content only).
fn content_of(tags: &Tags, i: usize) -> Vec<(usize, i64)> {
    enum Item {
        Node(usize),
        Content(usize, i64),
    }
    let mut out = Vec::new();
    // Kids are pushed in reverse so they pop in order. Every element has a single parent in
    // `Tags` and comes after it, so each node is reached once; the budget is a second guard.
    let mut stack = vec![Item::Node(i)];
    let mut budget = tags.nodes.len().saturating_add(1);
    while let Some(item) = stack.pop() {
        let n = match item {
            Item::Content(p, m) => {
                out.push((p, m));
                continue;
            }
            Item::Node(n) => n,
        };
        if budget == 0 {
            break;
        }
        budget -= 1;
        let Some(node) = tags.nodes.get(n) else { continue };
        for kid in node.kids.iter().rev() {
            match kid {
                TagKid::Element(e) if *e > n => stack.push(Item::Node(*e)),
                TagKid::Content { page: Some(p), mcid, stream: None } => stack.push(Item::Content(*p, *mcid)),
                _ => {}
            }
        }
    }
    out
}

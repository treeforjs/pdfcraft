//! Accessible text: the structure tree read as a screen reader reads it, for Save As ▸
//! Accessible Text. Each element in logical order gives its `/ActualText`, else its `/Alt`, else
//! its marked content (whose text the caller extracts from the page); block elements (P,
//! headings, list items, table rows, figures, captions …) become separate blocks. Artifacts are
//! left out: content marked `/Artifact` has no MCID, so no element reaches it, and PDF 2.0
//! `Artifact` structure elements are skipped with everything inside them.
//!
//! Works on [`Tags`], which is already bounded and cycle-safe; the walk visits each node at most
//! once.

use crate::headings::{heading_level, level_of, non_blank};
use crate::tags::{TagKid, Tags};

/// What a block is, for laying it out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    /// A heading of this level (as [`crate::headings`] computes it).
    Heading(usize),
    /// A paragraph, or content written directly inside a grouping element (Sect, Div …).
    Paragraph,
    /// A list item (LI) or a table of contents item (TOCI).
    ListItem,
    /// A table row (TR); its cells are separated by [`Piece::Cell`].
    TableRow,
    /// A figure or formula: its alternate text, else the text inside it.
    Figure,
    /// A caption.
    Caption,
}

/// One piece of a block's text, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    /// Replacement text: an element's `/ActualText` or `/Alt`.
    Text(String),
    /// Marked content on a page (0-based page, MCID); content in another stream (a form
    /// XObject's MCIDs) is left out.
    Content { page: usize, mcid: i64 },
    /// The start of another table cell in a row.
    Cell,
}

/// One block of accessible text.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub kind: BlockKind,
    /// The element the block belongs to (index into [`Tags::nodes`]).
    pub node: Option<usize>,
    /// Never empty.
    pub pieces: Vec<Piece>,
}

/// How an element takes part in the text.
enum Role {
    /// Left out with everything inside it.
    Skip,
    /// Starts a block of its own.
    Block(BlockKind),
    /// Groups blocks (Document, Sect, Div, L, Table …): content directly inside it forms
    /// paragraphs.
    Group,
    /// A table cell: inline, separated from the previous cell of its row.
    Cell,
    /// Text inside a block (Span, Link, Lbl, LBody …).
    Inline,
}

fn role_of(tags: &Tags, i: usize, role: &str) -> Role {
    if let Some(kind) = heading_level(role) {
        return Role::Block(BlockKind::Heading(level_of(tags, i, kind)));
    }
    match role {
        "Artifact" => Role::Skip,
        "P" | "Title" | "Note" | "FENote" | "Code" | "BibEntry" | "Aside" => Role::Block(BlockKind::Paragraph),
        "LI" | "TOCI" => Role::Block(BlockKind::ListItem),
        "TR" => Role::Block(BlockKind::TableRow),
        "Figure" | "Formula" => Role::Block(BlockKind::Figure),
        "Caption" => Role::Block(BlockKind::Caption),
        "TH" | "TD" => Role::Cell,
        "Document" | "DocumentFragment" | "Part" | "Art" | "Sect" | "Div" | "BlockQuote" | "TOC" | "Index" | "L" | "Table" | "THead" | "TBody"
        | "TFoot" | "NonStruct" | "Private" | "Form" => Role::Group,
        _ => Role::Inline,
    }
}

/// The blocks being built: those open, innermost last, and the pieces gathered for the
/// innermost one (or for an implicit paragraph when none is open).
struct Builder {
    out: Vec<Block>,
    open: Vec<(BlockKind, usize)>,
    pieces: Vec<Piece>,
    /// The element the pending pieces belong to when no block is open.
    group: Option<usize>,
}

impl Builder {
    fn flush(&mut self) {
        // A cell marker with nothing after it adds nothing.
        while matches!(self.pieces.last(), Some(Piece::Cell)) {
            self.pieces.pop();
        }
        if self.pieces.iter().all(|p| matches!(p, Piece::Cell)) {
            self.pieces.clear();
            return;
        }
        let (kind, node) = match self.open.last() {
            Some((k, n)) => (*k, Some(*n)),
            None => (BlockKind::Paragraph, self.group),
        };
        self.out.push(Block { kind, node, pieces: std::mem::take(&mut self.pieces) });
    }

    fn push(&mut self, piece: Piece, group: Option<usize>) {
        if self.open.is_empty() && self.pieces.is_empty() {
            self.group = group;
        }
        self.pieces.push(piece);
    }
}

/// The accessible text of the tree as blocks, in logical order.
pub fn reading_blocks(tags: &Tags) -> Vec<Block> {
    enum Item {
        Enter(usize, Option<usize>),
        /// Leaving an element that opened a block.
        ExitBlock,
        /// Leaving a grouping element.
        ExitGroup,
        Piece(Piece, Option<usize>),
    }
    let mut b = Builder { out: Vec::new(), open: Vec::new(), pieces: Vec::new(), group: None };
    let mut stack: Vec<Item> = tags.roots.iter().rev().map(|r| Item::Enter(*r, None)).collect();
    // Every element has a single parent in `Tags` and comes after it, so each node is entered
    // once; the budget is a second guard.
    let mut budget = tags.nodes.len().saturating_add(1);
    while let Some(item) = stack.pop() {
        let (n, group) = match item {
            Item::Piece(p, g) => {
                b.push(p, g);
                continue;
            }
            Item::ExitBlock => {
                b.flush();
                b.open.pop();
                continue;
            }
            Item::ExitGroup => {
                if b.open.is_empty() {
                    b.flush();
                }
                continue;
            }
            Item::Enter(n, g) => (n, g),
        };
        if budget == 0 {
            break;
        }
        budget -= 1;
        let Some(node) = tags.nodes.get(n) else { continue };
        let role = role_of(tags, n, node.role());
        // The exit, then the kids in reverse, so they pop in order.
        let mut group = group;
        match role {
            Role::Skip => continue,
            Role::Block(kind) => {
                // A block inside another one reads as part of it, except list items and table
                // rows (a nested list, a table inside a cell), which stay blocks of their own.
                let own = b.open.is_empty() || matches!(kind, BlockKind::ListItem | BlockKind::TableRow);
                if own {
                    b.flush();
                    b.open.push((kind, n));
                    stack.push(Item::ExitBlock);
                }
            }
            Role::Group => {
                if b.open.is_empty() {
                    b.flush();
                }
                group = Some(n);
                stack.push(Item::ExitGroup);
            }
            Role::Cell => {
                if !b.pieces.is_empty() {
                    b.pieces.push(Piece::Cell);
                }
            }
            Role::Inline => {}
        }
        // Replacement text stands for the element and everything inside it.
        if let Some(t) = non_blank(&node.actual_text).or_else(|| non_blank(&node.alt)) {
            stack.push(Item::Piece(Piece::Text(t), group));
            continue;
        }
        for kid in node.kids.iter().rev() {
            match kid {
                TagKid::Element(e) if *e > n => stack.push(Item::Enter(*e, group)),
                TagKid::Content { page: Some(p), mcid, stream: None } => stack.push(Item::Piece(Piece::Content { page: *p, mcid: *mcid }, group)),
                _ => {}
            }
        }
    }
    b.flush();
    b.out
}

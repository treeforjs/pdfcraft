//! Associations for underlines authored by this editor. A PDF has no general text underline
//! operator: foreign paths remain artwork. Private marked-content pairs let us update our own
//! paths without using overlap guesses that could erase a rule, image or unrelated glyph.

use super::*;
use std::collections::{HashMap, HashSet};

const TEXT: &[u8] = b"PdfCraftUnderlineText";
const PATH: &[u8] = b"PdfCraftUnderlinePath";
const MAX_PAIRS: usize = 1_000_000;

struct Entry {
    text: Vec<std::ops::Range<usize>>,
    paths: Vec<std::ops::Range<usize>>,
}

pub(super) struct Index {
    entries: HashMap<i64, Entry>,
    text: HashMap<usize, i64>,
    invalid: HashSet<usize>,
    overflow: bool,
    max_id: i64,
}

fn tag(op: &Op) -> bool {
    op.is("BDC") && matches!(op.name(0), Some(TEXT | PATH))
}

fn id(op: &Op) -> Option<i64> {
    let dict = op.operands.get(1)?.as_dict()?;
    (dict.int(b"Version") == Some(1)).then(|| dict.int(b"ID")).flatten().filter(|id| *id > 0)
}

fn path(ops: &[Op]) -> bool {
    let names: &[&[u8]] = &[b"q", b"cm", b"RG", b"w", b"J", b"d", b"m", b"l", b"S", b"Q"];
    let operands = [0, 6, 3, 1, 1, 2, 2, 2, 0, 0];
    ops.len() == names.len()
        && ops.iter().zip(names).all(|(op, name)| op.op == *name)
        && ops.iter().zip(operands).all(|(op, count)| op.operands.len() == count)
        && ops.get(1).and_then(Op::nums::<6>).is_some_and(|v| v.into_iter().all(f64::is_finite))
        && ops.get(2).and_then(Op::nums::<3>).is_some_and(|v| v.into_iter().all(|n| n.is_finite() && (0.0..=1.0).contains(&n)))
        && ops.get(3).and_then(|op| op.num(0)).is_some_and(|n| n.is_finite() && n > 0.0)
        && ops.get(4).and_then(|op| op.num(0)) == Some(0.0)
        && ops.get(5).is_some_and(|op| op.operands.first().and_then(Object::as_array).is_some_and(Vec::is_empty) && op.num(1) == Some(0.0))
        && [6, 7].into_iter().all(|i| ops.get(i).and_then(Op::nums::<2>).is_some_and(|v| v.into_iter().all(f64::is_finite)))
}

impl Index {
    pub(super) fn read(ops: &[Op]) -> Self {
        let mut result = Self { entries: HashMap::new(), text: HashMap::new(), invalid: HashSet::new(), overflow: false, max_id: 0 };
        let mut stack = Vec::new();
        let mut invalid_ranges = Vec::new();
        for (end, op) in ops.iter().enumerate() {
            if matches!(op.op.as_slice(), b"BDC" | b"BMC") {
                stack.push(end);
                if tag(op) {
                    result.max_id = result.max_id.max(id(op).unwrap_or(0));
                }
            } else if op.is("EMC")
                && let Some(start) = stack.pop()
            {
                let Some(marker) = ops.get(start).filter(|op| tag(op)) else { continue };
                let body = ops.get(start + 1..end).unwrap_or_default();
                let is_text = marker.name(0) == Some(TEXT);
                let valid = if is_text {
                    body.len() == 1
                        && body
                            .first()
                            .is_some_and(|op| op.is("Tj") && op.operands.len() == 1 && op.operands.first().and_then(Object::as_string).is_some())
                } else {
                    path(body)
                };
                if let Some(id) = id(marker).filter(|_| valid) {
                    if result.entries.len() >= MAX_PAIRS && !result.entries.contains_key(&id) {
                        result.overflow = true;
                        continue;
                    }
                    let entry = result.entries.entry(id).or_insert_with(|| Entry { text: Vec::new(), paths: Vec::new() });
                    if is_text {
                        entry.text.push(start..end + 1);
                        result.text.insert(start + 1, id);
                    } else {
                        entry.paths.push(start..end + 1);
                    }
                } else {
                    invalid_ranges.push(start + 1..end);
                }
            }
        }
        // Unterminated private content is not a reliable association.
        for start in stack {
            if ops.get(start).is_some_and(tag) {
                invalid_ranges.push(start + 1..ops.len());
            }
        }
        // Nested malformed markers must not cause repeated scans of the same body.
        invalid_ranges.sort_unstable_by_key(|range| range.start);
        let mut ranges = invalid_ranges.into_iter().peekable();
        let mut invalid_until = 0;
        for (i, op) in ops.iter().enumerate() {
            while let Some(range) = ranges.next_if(|range| range.start <= i) {
                invalid_until = invalid_until.max(range.end);
            }
            if i < invalid_until && matches!(op.op.as_slice(), b"Tj" | b"TJ" | b"'" | b"\"") {
                result.invalid.insert(i);
            }
        }
        result
    }

    pub(super) fn at(&self, op: usize) -> Result<Option<i64>, ()> {
        if self.overflow || self.invalid.contains(&op) {
            return Err(());
        }
        match self.text.get(&op) {
            Some(id) if self.entries.get(id).is_some_and(|e| e.text.len() == 1 && e.paths.len() == 1) => Ok(Some(*id)),
            Some(_) => Err(()),
            None => Ok(None),
        }
    }

    pub(super) fn next_id(&self) -> Result<i64, EditError> {
        self.max_id.checked_add(1).ok_or_else(|| EditError::Invalid("the page has exhausted underline association IDs".into()))
    }

    /// Remove a complete pair only when its entire text-showing operation is being replaced.
    /// Duplicate IDs, missing paths and malformed paths are refused, never guessed.
    pub(super) fn remove_for(&self, text_ops: &HashSet<usize>, drop: &mut HashSet<usize>) -> Result<(), EditError> {
        for op in text_ops {
            match self.at(*op) {
                Err(()) => {
                    return Err(EditError::Invalid(
                        "this paragraph has an ambiguous underline association; its artwork is preserved unchanged".into(),
                    ));
                }
                Ok(Some(id)) => {
                    if let Some(entry) = self.entries.get(&id) {
                        for range in entry.text.iter().chain(&entry.paths) {
                            drop.extend(range.clone());
                        }
                    }
                }
                Ok(None) => {}
            }
        }
        Ok(())
    }
}

pub(super) fn begin(id: i64, path: bool) -> Op {
    let mut props = Dict::new();
    props.set(b"Version".to_vec(), Object::Int(1));
    props.set(b"ID".to_vec(), Object::Int(id));
    Op::new("BDC", vec![Object::Name(if path { PATH } else { TEXT }.to_vec()), Object::Dict(props)])
}

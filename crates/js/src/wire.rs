//! Only these messages cross the interpreter boundary; no host handles are exposed.
use std::borrow::Cow;
use std::io::{self, Write};

use serde::{Deserialize, Serialize};

use crate::xfa_protocol::{XfaDoc, XfaEvent, XfaNode, XfaOutcome};
use crate::{Dialogs, DocInfo, Event, FieldState, Limits, Outcome};

pub(crate) const VERSION: u32 = 1;
pub(crate) const MAX_INPUT: usize = 4 * 1024 * 1024;
pub(crate) const MAX_OUTPUT: usize = 4 * 1024 * 1024;
pub(crate) const MAX_SCRIPT: usize = 256 * 1024;
#[cfg(not(target_os = "wasi"))]
pub(crate) const MAX_EFFECTS: usize = 10_000;

#[derive(Serialize, Deserialize)]
pub(crate) struct Envelope<'a> {
    pub version: u32,
    pub platform: String,
    pub job: Job<'a>,
}

#[derive(Serialize, Deserialize)]
pub(crate) enum Job<'a> {
    Field {
        script: Cow<'a, str>,
        event: Cow<'a, Event>,
        doc: Cow<'a, DocInfo>,
        fields: Cow<'a, [FieldState]>,
        doc_scripts: Cow<'a, [String]>,
        limits: Limits,
        dialogs: Cow<'a, Dialogs>,
    },
    Xfa {
        script: Cow<'a, str>,
        event: Cow<'a, XfaEvent>,
        doc: Cow<'a, XfaDoc>,
        root: Cow<'a, XfaNode>,
        limits: Limits,
        formcalc: bool,
    },
}

#[derive(Serialize, Deserialize)]
pub(crate) enum Reply {
    Field(Outcome),
    Xfa(XfaOutcome),
}

/// Refuse before growing the backing allocation past the encoded byte budget.
pub(crate) struct Buffer {
    pub bytes: Vec<u8>,
    pub limit: usize,
}

impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("script message exceeds its byte budget"));
        }
        let needed = self.bytes.len().saturating_add(bytes.len());
        if needed > self.bytes.capacity() {
            let capacity = self.bytes.capacity().max(256).saturating_mul(2).min(self.limit).max(needed);
            self.bytes.try_reserve_exact(capacity.saturating_sub(self.bytes.len())).map_err(io::Error::other)?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn encode(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, String> {
    // Refuse huge strings/containers by length before serde_json scans them for escaping.
    // This pass has no growing allocation and stops once the raw byte/item budget is spent.
    value.serialize(&mut crate::encode_budget::Budget::new(limit)).map_err(|e| e.to_string())?;
    let mut buffer = Buffer { bytes: Vec::new(), limit };
    serde_json::to_writer(&mut buffer, value).map_err(|e| format!("script message refused: {e}"))?;
    Ok(buffer.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiny_writes_grow_geometrically_without_crossing_the_cap() {
        let mut buffer = Buffer { bytes: Vec::new(), limit: 20_000 };
        let mut changes = 0;
        for _ in 0..20_000 {
            let before = buffer.bytes.capacity();
            buffer.write_all(b"x").unwrap();
            changes += usize::from(before != buffer.bytes.capacity());
            assert!(buffer.bytes.capacity() <= buffer.limit);
        }
        assert!(changes <= 8, "{changes} reallocations");
        assert!(buffer.write_all(b"y").is_err());
        assert_eq!(buffer.bytes, vec![b'x'; 20_000]);
        assert_eq!(encode(&"\n", 4).unwrap(), b"\"\\n\"");
        assert!(encode(&"\n", 3).is_err(), "escaped bytes must obey the final cap");
    }
}

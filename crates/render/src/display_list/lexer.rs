//! A strict linear reader for the native vector subset. Do not invoke the tolerant COS/content
//! lexer here: it repairs malformed numeric prefixes and is not a bounded raster candidate scan.

use super::{Object, Op, Result, Unsupported};

pub(super) struct Reader<'a> {
    data: &'a [u8],
    at: usize,
    cancelled: &'a dyn Fn() -> bool,
}

fn white(byte: u8) -> bool {
    matches!(byte, 0 | 9 | 10 | 12 | 13 | 32)
}
fn delimiter(byte: u8) -> bool {
    white(byte) || b"()<>[]{}/%".contains(&byte)
}
fn numeric(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.')
}
fn error(reason: &str) -> Unsupported {
    Unsupported(reason.into())
}

impl<'a> Reader<'a> {
    pub(super) fn new(data: &'a [u8], cancelled: &'a dyn Fn() -> bool) -> Self {
        Self { data, at: 0, cancelled }
    }

    fn skip_space(&mut self) -> Result<()> {
        let mut comment = false;
        while let Some(&byte) = self.data.get(self.at) {
            if self.at.is_multiple_of(1024) && (self.cancelled)() {
                return Err(error("request cancelled"));
            }
            if comment {
                if matches!(byte, b'\r' | b'\n') {
                    comment = false;
                }
            } else if byte == b'%' {
                comment = true;
            } else if !white(byte) {
                break;
            }
            self.at += 1;
        }
        Ok(())
    }

    fn token(&mut self, max: usize) -> Result<&'a [u8]> {
        let start = self.at;
        while self.data.get(self.at).is_some_and(|byte| !delimiter(*byte)) {
            if self.at - start >= max {
                return Err(error("lexical token exceeds the native vector budget"));
            }
            self.at += 1;
        }
        self.data.get(start..self.at).filter(|token| !token.is_empty()).ok_or_else(|| error("unsupported vector syntax"))
    }

    fn number(&mut self) -> Result<Object> {
        let token = self.token(64)?;
        let unsigned = if matches!(token.first(), Some(b'+' | b'-')) { token.get(1..).unwrap_or_default() } else { token };
        let mut dots = 0;
        let mut digits = 0;
        for byte in unsigned {
            if byte.is_ascii_digit() {
                digits += 1;
            } else if *byte == b'.' {
                dots += 1;
            } else {
                return Err(error("invalid PDF number"));
            }
        }
        if digits == 0 || dots > 1 {
            return Err(error("invalid PDF number"));
        }
        let value = std::str::from_utf8(token)
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|n| n.is_finite())
            .ok_or_else(|| error("invalid PDF number"))?;
        Ok(Object::Real(value))
    }

    fn array(&mut self) -> Result<Object> {
        self.at += 1; // Caller observed '['.
        let mut values = Vec::new();
        loop {
            if (self.cancelled)() {
                return Err(error("request cancelled"));
            }
            self.skip_space()?;
            let Some(&byte) = self.data.get(self.at) else {
                return Err(error("unfinished dash array"));
            };
            if byte == b']' {
                self.at += 1;
                return Ok(Object::Array(values));
            }
            if values.len() >= 256 || !numeric(byte) {
                return Err(error("invalid or oversized dash array"));
            }
            values.push(self.number()?);
        }
    }

    pub(super) fn next(&mut self) -> Result<Option<Op>> {
        let start = self.at;
        let mut operands = Vec::new();
        loop {
            if (self.cancelled)() {
                return Err(error("request cancelled"));
            }
            self.skip_space()?;
            let Some(&byte) = self.data.get(self.at) else {
                return if operands.is_empty() { Ok(None) } else { Err(error("operands without an operator")) };
            };
            if numeric(byte) || byte == b'[' {
                if operands.len() >= 6 {
                    return Err(error("too many vector operands"));
                }
                operands.push(if byte == b'[' { self.array()? } else { self.number()? });
            } else if byte.is_ascii_alphabetic() {
                let token = self.token(3)?;
                return Ok(Some(Op { op: token.to_vec(), operands, inline: None, span: start..self.at }));
            } else {
                // Named reasons, so fallbacks can be counted by what the page needs.
                return Err(error(match byte {
                    b'/' => "name operand (resources, text, images or graphics state)",
                    b'(' | b'<' => "string or dictionary operand",
                    _ => "unsupported vector syntax",
                }));
            }
        }
    }
}

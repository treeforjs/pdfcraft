//! CCITT Group 4 (ITU-T T.6, "MMR") encoding of bi-level images, for `CCITTFaxDecode` with
//! `/K -1` (ISO 32000-2 §7.4.6). Written from ITU-T T.4 §4.2 (two-dimensional coding, the
//! mode and run-length code tables) and T.6 §2 (no EOLs, an imaginary all-white first
//! reference line, end-of-facsimile-block terminator).
//!
//! Only an encoder: PdfCraft does not decode CCITT itself (rendering goes through the
//! bootstrap renderer). Input is PDF image-sample layout: rows of `width` one-bit samples,
//! most significant bit first, each row padded to a whole byte.

use crate::FilterError;

const NAME: &str = "CCITTFaxDecode";

/// Terminating codes for white runs 0–63 (T.4 Table 2): (code, bit length).
const WHITE_TERM: [(u16, u8); 64] = [
    (0b00110101, 8),
    (0b000111, 6),
    (0b0111, 4),
    (0b1000, 4),
    (0b1011, 4),
    (0b1100, 4),
    (0b1110, 4),
    (0b1111, 4),
    (0b10011, 5),
    (0b10100, 5),
    (0b00111, 5),
    (0b01000, 5),
    (0b001000, 6),
    (0b000011, 6),
    (0b110100, 6),
    (0b110101, 6),
    (0b101010, 6),
    (0b101011, 6),
    (0b0100111, 7),
    (0b0001100, 7),
    (0b0001000, 7),
    (0b0010111, 7),
    (0b0000011, 7),
    (0b0000100, 7),
    (0b0101000, 7),
    (0b0101011, 7),
    (0b0010011, 7),
    (0b0100100, 7),
    (0b0011000, 7),
    (0b00000010, 8),
    (0b00000011, 8),
    (0b00011010, 8),
    (0b00011011, 8),
    (0b00010010, 8),
    (0b00010011, 8),
    (0b00010100, 8),
    (0b00010101, 8),
    (0b00010110, 8),
    (0b00010111, 8),
    (0b00101000, 8),
    (0b00101001, 8),
    (0b00101010, 8),
    (0b00101011, 8),
    (0b00101100, 8),
    (0b00101101, 8),
    (0b00000100, 8),
    (0b00000101, 8),
    (0b00001010, 8),
    (0b00001011, 8),
    (0b01010010, 8),
    (0b01010011, 8),
    (0b01010100, 8),
    (0b01010101, 8),
    (0b00100100, 8),
    (0b00100101, 8),
    (0b01011000, 8),
    (0b01011001, 8),
    (0b01011010, 8),
    (0b01011011, 8),
    (0b01001010, 8),
    (0b01001011, 8),
    (0b00110010, 8),
    (0b00110011, 8),
    (0b00110100, 8),
];

/// Terminating codes for black runs 0–63 (T.4 Table 2).
const BLACK_TERM: [(u16, u8); 64] = [
    (0b0000110111, 10),
    (0b010, 3),
    (0b11, 2),
    (0b10, 2),
    (0b011, 3),
    (0b0011, 4),
    (0b0010, 4),
    (0b00011, 5),
    (0b000101, 6),
    (0b000100, 6),
    (0b0000100, 7),
    (0b0000101, 7),
    (0b0000111, 7),
    (0b00000100, 8),
    (0b00000111, 8),
    (0b000011000, 9),
    (0b0000010111, 10),
    (0b0000011000, 10),
    (0b0000001000, 10),
    (0b00001100111, 11),
    (0b00001101000, 11),
    (0b00001101100, 11),
    (0b00000110111, 11),
    (0b00000101000, 11),
    (0b00000010111, 11),
    (0b00000011000, 11),
    (0b000011001010, 12),
    (0b000011001011, 12),
    (0b000011001100, 12),
    (0b000011001101, 12),
    (0b000001101000, 12),
    (0b000001101001, 12),
    (0b000001101010, 12),
    (0b000001101011, 12),
    (0b000011010010, 12),
    (0b000011010011, 12),
    (0b000011010100, 12),
    (0b000011010101, 12),
    (0b000011010110, 12),
    (0b000011010111, 12),
    (0b000001101100, 12),
    (0b000001101101, 12),
    (0b000011011010, 12),
    (0b000011011011, 12),
    (0b000001010100, 12),
    (0b000001010101, 12),
    (0b000001010110, 12),
    (0b000001010111, 12),
    (0b000001100100, 12),
    (0b000001100101, 12),
    (0b000001010010, 12),
    (0b000001010011, 12),
    (0b000000100100, 12),
    (0b000000110111, 12),
    (0b000000111000, 12),
    (0b000000100111, 12),
    (0b000000101000, 12),
    (0b000001011000, 12),
    (0b000001011001, 12),
    (0b000000101011, 12),
    (0b000000101100, 12),
    (0b000001011010, 12),
    (0b000001100110, 12),
    (0b000001100111, 12),
];

/// Make-up codes for white runs 64, 128, …, 1728 (T.4 Table 3).
const WHITE_MAKEUP: [(u16, u8); 27] = [
    (0b11011, 5),
    (0b10010, 5),
    (0b010111, 6),
    (0b0110111, 7),
    (0b00110110, 8),
    (0b00110111, 8),
    (0b01100100, 8),
    (0b01100101, 8),
    (0b01101000, 8),
    (0b01100111, 8),
    (0b011001100, 9),
    (0b011001101, 9),
    (0b011010010, 9),
    (0b011010011, 9),
    (0b011010100, 9),
    (0b011010101, 9),
    (0b011010110, 9),
    (0b011010111, 9),
    (0b011011000, 9),
    (0b011011001, 9),
    (0b011011010, 9),
    (0b011011011, 9),
    (0b010011000, 9),
    (0b010011001, 9),
    (0b010011010, 9),
    (0b011000, 6),
    (0b010011011, 9),
];

/// Make-up codes for black runs 64, 128, …, 1728 (T.4 Table 3).
const BLACK_MAKEUP: [(u16, u8); 27] = [
    (0b0000001111, 10),
    (0b000011001000, 12),
    (0b000011001001, 12),
    (0b000001011011, 12),
    (0b000000110011, 12),
    (0b000000110100, 12),
    (0b000000110101, 12),
    (0b0000001101100, 13),
    (0b0000001101101, 13),
    (0b0000001001010, 13),
    (0b0000001001011, 13),
    (0b0000001001100, 13),
    (0b0000001001101, 13),
    (0b0000001110010, 13),
    (0b0000001110011, 13),
    (0b0000001110100, 13),
    (0b0000001110101, 13),
    (0b0000001110110, 13),
    (0b0000001110111, 13),
    (0b0000001010010, 13),
    (0b0000001010011, 13),
    (0b0000001010100, 13),
    (0b0000001010101, 13),
    (0b0000001011010, 13),
    (0b0000001011011, 13),
    (0b0000001100100, 13),
    (0b0000001100101, 13),
];

/// Extended make-up codes for runs 1792, 1856, …, 2560, shared by both colours (T.4 Table 4).
const EXT_MAKEUP: [(u16, u8); 13] = [
    (0b00000001000, 11),
    (0b00000001100, 11),
    (0b00000001101, 11),
    (0b000000010010, 12),
    (0b000000010011, 12),
    (0b000000010100, 12),
    (0b000000010101, 12),
    (0b000000010110, 12),
    (0b000000010111, 12),
    (0b000000011100, 12),
    (0b000000011101, 12),
    (0b000000011110, 12),
    (0b000000011111, 12),
];

/// Two-dimensional mode codes (T.4 Table 4).
const PASS: (u16, u8) = (0b0001, 4);
const HORIZONTAL: (u16, u8) = (0b001, 3);
/// Vertical modes VL3, VL2, VL1, V0, VR1, VR2, VR3 (a1 − b1 = −3 … 3).
const VERTICAL: [(u16, u8); 7] = [(0b0000010, 7), (0b000010, 6), (0b010, 3), (0b1, 1), (0b011, 3), (0b000011, 6), (0b0000011, 7)];
const EOL: (u16, u8) = (0b000000000001, 12);

/// The largest image width accepted (PDF's `/Columns` is an integer; real fax and scan widths
/// are far below this).
pub(crate) const MAX_COLUMNS: usize = 1 << 20;

struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    bits: u32,
    max: usize,
}

impl BitWriter {
    fn put(&mut self, (code, len): (u16, u8)) -> Result<(), FilterError> {
        // `len` ≤ 13 and `bits` < 8 before the call, so the accumulator never overflows.
        self.acc = (self.acc << len) | u32::from(code);
        self.bits += u32::from(len);
        while self.bits >= 8 {
            self.bits -= 8;
            if self.out.len() >= self.max {
                return Err(FilterError::LimitExceeded(self.max));
            }
            self.out.push((self.acc >> self.bits) as u8);
        }
        self.acc &= (1 << self.bits) - 1;
        Ok(())
    }

    fn finish(mut self) -> Result<Vec<u8>, FilterError> {
        if self.bits > 0 {
            if self.out.len() >= self.max {
                return Err(FilterError::LimitExceeded(self.max));
            }
            self.out.push((self.acc << (8 - self.bits)) as u8);
        }
        Ok(self.out)
    }

    /// One run of `run` pixels of the given colour: 2560 make-ups, a make-up, a terminating code.
    fn run(&mut self, mut run: usize, black: bool) -> Result<(), FilterError> {
        let (term, makeup) = if black { (&BLACK_TERM, &BLACK_MAKEUP) } else { (&WHITE_TERM, &WHITE_MAKEUP) };
        while run >= 2560 {
            self.put(EXT_MAKEUP[12])?;
            run -= 2560;
        }
        if run >= 64 {
            let m = run / 64; // 1..=39
            let code = if m <= 27 { makeup.get(m - 1) } else { EXT_MAKEUP.get(m - 28) };
            self.put(*code.ok_or_else(|| corrupt("run length table"))?)?;
            run %= 64;
        }
        self.put(*term.get(run).ok_or_else(|| corrupt("run length table"))?)
    }
}

fn corrupt(detail: &str) -> FilterError {
    FilterError::Corrupt { filter: NAME, detail: detail.into() }
}

/// The changing elements of one row: positions where a pixel's colour differs from its left
/// neighbour's (the pixel left of the row is an imaginary white one). Even entries change to
/// black, odd entries back to white.
fn changes(row: &[u8], width: usize, black_is_1: bool, out: &mut Vec<u32>) {
    out.clear();
    let mut black = false;
    for x in 0..width {
        let byte = row.get(x / 8).copied().unwrap_or(0);
        let bit = (byte >> (7 - x % 8)) & 1 == 1;
        if bit == black_is_1 {
            if !black {
                out.push(x as u32);
                black = true;
            }
        } else if black {
            out.push(x as u32);
            black = false;
        }
    }
}

/// Group 4 encoding of a `width` × `height` image (`rows` as described in the module docs).
/// `black_is_1` says which sample value is black (the decoder's `/BlackIs1`); the bitmap round
/// trips either way. The output (ending with EOFB, byte-padded) may not exceed `max_output`.
pub(crate) fn encode_g4(rows: &[u8], width: usize, height: usize, black_is_1: bool, max_output: usize) -> Result<Vec<u8>, FilterError> {
    if width == 0 || height == 0 || width > MAX_COLUMNS {
        return Err(corrupt("image size out of range"));
    }
    let stride = width.div_ceil(8);
    let needed = stride.checked_mul(height).ok_or_else(|| corrupt("image size out of range"))?;
    if rows.len() < needed {
        return Err(corrupt("fewer samples than the image size"));
    }
    let mut w = BitWriter { out: Vec::with_capacity(needed.min(max_output) / 4 + 16), acc: 0, bits: 0, max: max_output };
    let end = width as i64;
    let (mut reference, mut coding) = (Vec::new(), Vec::new());
    for row in rows.chunks_exact(stride).take(height) {
        changes(row, width, black_is_1, &mut coding);
        // Position of change `i` of a line, or the end of the line past its last change.
        let at = |line: &[u32], i: usize| line.get(i).map_or(end, |&p| i64::from(p));
        let mut a0: i64 = -1;
        let mut black = false;
        // Index of the first coding / reference change right of a0. Both only move forward.
        let (mut ci, mut ri) = (0usize, 0usize);
        while a0 < end {
            while ci < coding.len() && at(&coding, ci) <= a0 {
                ci += 1;
            }
            while ri < reference.len() && at(&reference, ri) <= a0 {
                ri += 1;
            }
            let a1 = at(&coding, ci);
            // b1 changes to the colour opposite a0's: to black at even indices.
            let bi = if ri.is_multiple_of(2) != black { ri } else { ri + 1 };
            let (b1, b2) = (at(&reference, bi), at(&reference, bi + 1));
            if b2 < a1 {
                w.put(PASS)?;
                a0 = b2;
            } else if (a1 - b1).abs() <= 3 {
                w.put(VERTICAL[(a1 - b1 + 3) as usize])?;
                a0 = a1;
                black = !black;
            } else {
                let a2 = at(&coding, ci + 1);
                w.put(HORIZONTAL)?;
                w.run((a1 - a0.max(0)) as usize, black)?;
                w.run((a2 - a1) as usize, !black)?;
                a0 = a2;
            }
        }
        std::mem::swap(&mut reference, &mut coding);
    }
    w.put(EOL)?;
    w.put(EOL)?;
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode with an independent decoder (hayro-ccitt, MIT/Apache) into "is black" pixels.
    fn decode(data: &[u8], width: usize, height: usize) -> Result<Vec<bool>, hayro_ccitt::DecodeError> {
        struct Sink(Vec<bool>, usize);
        impl hayro_ccitt::Decoder for Sink {
            fn push_pixel(&mut self, white: bool) {
                self.0.push(!white);
            }
            fn push_pixel_chunk(&mut self, white: bool, chunks: u32) {
                for _ in 0..chunks * 8 {
                    self.0.push(!white);
                }
            }
            fn next_line(&mut self) {
                self.1 += 1;
            }
        }
        let settings = hayro_ccitt::DecodeSettings {
            columns: width as u32,
            rows: height as u32,
            end_of_block: true,
            end_of_line: false,
            rows_are_byte_aligned: false,
            encoding: hayro_ccitt::EncodingMode::Group4,
            invert_black: false,
        };
        let mut sink = Sink(Vec::new(), 0);
        hayro_ccitt::decode(data, &mut sink, &mut hayro_ccitt::DecoderContext::new(settings))?;
        assert_eq!(sink.1, height, "rows decoded");
        Ok(sink.0)
    }

    fn pack(pixels: &[bool], width: usize, black_is_1: bool) -> Vec<u8> {
        let stride = width.div_ceil(8);
        let height = pixels.len() / width;
        // Padding bits are set to the opposite of black, then flipped to the other value on odd
        // rows: the encoder must ignore them either way.
        let mut out = vec![0u8; stride * height];
        for (y, row) in pixels.chunks(width).enumerate() {
            for x in 0..stride * 8 {
                let bit = match row.get(x) {
                    Some(&b) => b == black_is_1,
                    None => y % 2 == 1,
                };
                if bit {
                    out[y * stride + x / 8] |= 0x80 >> (x % 8);
                }
            }
        }
        out
    }

    fn round_trip(pixels: &[bool], width: usize) {
        let height = pixels.len() / width;
        for black_is_1 in [false, true] {
            let enc = encode_g4(&pack(pixels, width, black_is_1), width, height, black_is_1, usize::MAX).unwrap();
            let dec = decode(&enc, width, height).unwrap_or_else(|e| panic!("{width}x{height}: {e}"));
            assert_eq!(dec, pixels, "{width}x{height} black_is_1={black_is_1}");
        }
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) as u32
        }
    }

    #[test]
    fn code_tables_are_prefix_free() {
        let mode: Vec<(u16, u8)> = [PASS, HORIZONTAL, EOL].into_iter().chain(VERTICAL).collect();
        let white: Vec<(u16, u8)> = WHITE_TERM.iter().chain(&WHITE_MAKEUP).chain(&EXT_MAKEUP).copied().chain([EOL]).collect();
        let black: Vec<(u16, u8)> = BLACK_TERM.iter().chain(&BLACK_MAKEUP).chain(&EXT_MAKEUP).copied().chain([EOL]).collect();
        for table in [&mode, &white, &black] {
            for (i, &(a, la)) in table.iter().enumerate() {
                assert!(u32::from(a) < (1 << la), "code {a:b} longer than {la}");
                for &(b, lb) in &table[i + 1..] {
                    let l = la.min(lb);
                    assert_ne!(a >> (la - l), b >> (lb - l), "{a:0la$b} / {b:0lb$b} share a prefix", la = la as usize, lb = lb as usize);
                }
            }
        }
        // Complete outside the 00000000 prefix: every 13-bit word not starting with eight zeros
        // starts with exactly one run code; under that prefix only EOL is used. Kraft sum
        // (in units of 2^-13) = 8192 − 32 + 2 for white and black alike. A mistyped code
        // breaks this or the prefix check above.
        for table in [&white, &black] {
            let sum: u64 = table.iter().map(|&(_, l)| 1u64 << (13 - l)).sum();
            assert_eq!(sum, 8192 - 32 + 2, "Kraft sum");
        }
    }

    #[test]
    fn all_white_line_is_v0_then_eofb() {
        // V0 (1), then EOL EOL: 1 000000000001 000000000001, zero-padded.
        assert_eq!(encode_g4(&[0xff], 8, 1, false, 100).unwrap(), [0x80, 0x08, 0x00, 0x80]);
        assert_eq!(encode_g4(&[0x00], 8, 1, true, 100).unwrap(), [0x80, 0x08, 0x00, 0x80]);
    }

    #[test]
    fn every_small_image_round_trips() {
        // Every bitmap up to 12 pixels, in every shape (widths 1–12).
        for width in 1..=12usize {
            for height in 1..=(12 / width) {
                let n = width * height;
                for bits in 0u32..(1 << n) {
                    let px: Vec<bool> = (0..n).map(|i| bits >> i & 1 == 1).collect();
                    round_trip(&px, width);
                }
            }
        }
    }

    #[test]
    fn random_images_of_awkward_widths_round_trip() {
        let mut r = Lcg(7);
        for &width in &[1usize, 2, 3, 7, 8, 9, 15, 17, 31, 63, 64, 65, 127, 129, 640, 1727, 1728, 1729, 2559, 2560, 2561, 2623, 2624, 4000, 5121] {
            // Densities from almost-white to almost-black, plus structured stripes.
            for density in [1u32, 50, 500, 900, 999] {
                let height = 9;
                let px: Vec<bool> = (0..width * height).map(|_| r.next() % 1000 < density).collect();
                round_trip(&px, width);
            }
            let height = 6;
            let px: Vec<bool> = (0..width * height).map(|i| ((i % width) / (1 + i / width)).is_multiple_of(2)).collect();
            round_trip(&px, width);
        }
    }

    #[test]
    fn long_runs_use_make_up_and_extended_codes() {
        let width = 9000;
        let mut px = vec![false; width * 6];
        px[width..2 * width].fill(true); // an all-black row
        px[2 * width + 1792..2 * width + 4352].fill(true); // 2560 black after 1792 white
        px[3 * width + 64..4 * width].fill(true); // 64 white, then black to the end
        px[4 * width + 5000] = true; // one isolated pixel
        // Row 5 white: the reference row's changes make long vertical/pass sequences.
        round_trip(&px, width);
        // Every run length 0..=2700 as a white run followed by a black run of the same length.
        for len in (0..=2700usize).step_by(7).chain([63, 64, 65, 127, 128, 1727, 1728, 1729, 1791, 1792, 2559, 2560, 2561, 2624]) {
            let width = 2 * len + 3;
            let row: Vec<bool> = (0..width).map(|x| x >= len && x < 2 * len).collect();
            let mut px = row.clone();
            px.extend(row.iter().map(|b| !b));
            px.extend(row);
            round_trip(&px, width);
        }
    }

    #[test]
    fn scan_like_pages_compress() {
        // Text-like blobs on white: G4 should beat raw 1-bit storage by a wide margin.
        let (width, height) = (1700usize, 220usize);
        let mut px = vec![false; width * height];
        for y in 0..height {
            for x in 0..width {
                let (cx, cy) = (x % 24, y % 30);
                // Glyph-like boxes with 2-pixel strokes: left stem, top bar, a middle bar.
                px[y * width + x] = (4..18).contains(&cx) && (6..22).contains(&cy) && (cx < 6 || cy < 8 || (13..15).contains(&cy));
            }
        }
        let raw = pack(&px, width, false);
        let enc = encode_g4(&raw, width, height, false, usize::MAX).unwrap();
        assert!(enc.len() * 4 < raw.len(), "{} vs {}", enc.len(), raw.len());
        assert_eq!(decode(&enc, width, height).unwrap(), px);
    }

    #[test]
    fn hostile_sizes_and_limits_are_errors() {
        assert!(encode_g4(&[], 0, 1, false, 100).is_err());
        assert!(encode_g4(&[], 1, 0, false, 100).is_err());
        assert!(encode_g4(&[0; 4], MAX_COLUMNS + 1, 1, false, 100).is_err());
        assert!(encode_g4(&[0; 3], 9, 2, false, 100).is_err(), "short input");
        assert!(encode_g4(&[0; 16], usize::MAX, usize::MAX, false, 100).is_err(), "overflow");
        assert!(encode_g4(&[0; 16], 1 << 20, usize::MAX / 1000, false, 100).is_err(), "overflow");
        // Noise compresses badly: a small output limit stops the encoder.
        let mut r = Lcg(3);
        let noise: Vec<u8> = (0..4096).map(|_| r.next() as u8).collect();
        assert!(matches!(encode_g4(&noise, 512, 64, false, 1000), Err(FilterError::LimitExceeded(1000))));
        assert!(encode_g4(&noise, 512, 64, false, 1 << 20).is_ok());
        // Trailing data past the last row is ignored.
        let mut more = noise.clone();
        more.extend([0xAA; 100]);
        assert_eq!(encode_g4(&more, 512, 64, false, 1 << 20).unwrap(), encode_g4(&noise, 512, 64, false, 1 << 20).unwrap());
    }
}

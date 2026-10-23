//! JBIG2 generic region encoding (ITU-T T.88 / ISO/IEC 14492) for bi-level images, written for
//! PDF's `JBIG2Decode` filter (ISO 32000-2 §7.4.7) from the public specification.
//!
//! The stream uses the embedded organisation PDF asks for (no file header, no end-of-page or
//! end-of-file segments): a page information segment and one immediate generic region segment
//! that covers the page, coded with the MQ arithmetic coder (§6.2.5, Annex E), template 0 with
//! its nominal adaptive pixels and typical prediction (TPGDON), so rows that repeat the row above
//! cost one decision. It is lossless: decoding gives back every sample. No symbol dictionary
//! (text region) coding, so there are no `/JBIG2Globals`.
//!
//! JBIG2 codes black as 1 while PDF image samples use 0 for black (and for the painted part of
//! an image mask), so the samples are inverted on the way in.

use crate::FilterError;

/// (Qe, NMPS, NLPS, SWITCH) for each of the 47 probability states (T.88 Table E.1).
const QE: [(u32, u8, u8, bool); 47] = [
    (0x5601, 1, 1, true),
    (0x3401, 2, 6, false),
    (0x1801, 3, 9, false),
    (0x0AC1, 4, 12, false),
    (0x0521, 5, 29, false),
    (0x0221, 38, 33, false),
    (0x5601, 7, 6, true),
    (0x5401, 8, 14, false),
    (0x4801, 9, 14, false),
    (0x3801, 10, 14, false),
    (0x3001, 11, 17, false),
    (0x2401, 12, 18, false),
    (0x1C01, 13, 20, false),
    (0x1601, 29, 21, false),
    (0x5601, 15, 14, true),
    (0x5401, 16, 14, false),
    (0x5101, 17, 15, false),
    (0x4801, 18, 16, false),
    (0x3801, 19, 17, false),
    (0x3401, 20, 18, false),
    (0x3001, 21, 19, false),
    (0x2801, 22, 19, false),
    (0x2401, 23, 20, false),
    (0x2201, 24, 21, false),
    (0x1C01, 25, 22, false),
    (0x1801, 26, 23, false),
    (0x1601, 27, 24, false),
    (0x1401, 28, 25, false),
    (0x1201, 29, 26, false),
    (0x1101, 30, 27, false),
    (0x0AC1, 31, 28, false),
    (0x09C1, 32, 29, false),
    (0x08A1, 33, 30, false),
    (0x0521, 34, 31, false),
    (0x0441, 35, 32, false),
    (0x02A1, 36, 33, false),
    (0x0221, 37, 34, false),
    (0x0141, 38, 35, false),
    (0x0111, 39, 36, false),
    (0x0085, 40, 37, false),
    (0x0049, 41, 38, false),
    (0x0025, 42, 39, false),
    (0x0015, 43, 40, false),
    (0x0009, 44, 41, false),
    (0x0005, 45, 42, false),
    (0x0001, 45, 43, false),
    (0x5601, 46, 46, false),
];

/// The context of the TPGDON decision for template 0 (T.88 Figure 8).
const SLTP_CONTEXT: usize = 0x9B25;

/// The MQ arithmetic encoder (T.88 Annex E.2), with one (state, MPS) pair per context.
struct Mq {
    a: u32,
    c: u32,
    ct: u32,
    /// The output; byte 0 is the spare byte before the code (BPST - 1), dropped at the end.
    out: Vec<u8>,
    states: Vec<(u8, u8)>,
    limit: usize,
}

impl Mq {
    fn new(contexts: usize, limit: usize) -> Mq {
        Mq { a: 0x8000, c: 0, ct: 12, out: vec![0], states: vec![(0, 0); contexts], limit }
    }

    fn encode(&mut self, cx: usize, bit: u8) -> Result<(), FilterError> {
        let Some(&(index, mps)) = self.states.get(cx) else {
            return Err(FilterError::Corrupt { filter: "JBIG2Encode", detail: "context out of range".into() });
        };
        let Some(&(qe, nmps, nlps, switch)) = QE.get(usize::from(index)) else {
            return Err(FilterError::Corrupt { filter: "JBIG2Encode", detail: "probability state out of range".into() });
        };
        self.a -= qe;
        if bit == mps {
            // CODEMPS
            if self.a & 0x8000 == 0 {
                if self.a < qe {
                    self.a = qe;
                } else {
                    self.c += qe;
                }
                self.set(cx, nmps, mps);
                self.renormalize()?;
            } else {
                self.c += qe;
            }
        } else {
            // CODELPS
            if self.a < qe {
                self.c += qe;
            } else {
                self.a = qe;
            }
            self.set(cx, nlps, if switch { 1 - mps } else { mps });
            self.renormalize()?;
        }
        Ok(())
    }

    fn set(&mut self, cx: usize, index: u8, mps: u8) {
        if let Some(s) = self.states.get_mut(cx) {
            *s = (index, mps);
        }
    }

    fn renormalize(&mut self) -> Result<(), FilterError> {
        loop {
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if self.ct == 0 {
                self.byte_out()?;
            }
            if self.a & 0x8000 != 0 {
                return Ok(());
            }
        }
    }

    fn byte_out(&mut self) -> Result<(), FilterError> {
        if self.out.len() > self.limit {
            return Err(FilterError::LimitExceeded(self.limit));
        }
        let last = self.out.last().copied().unwrap_or(0);
        if last == 0xFF {
            self.push_high(20);
        } else if self.c < 0x800_0000 {
            self.push_high(19);
        } else {
            // A carry into the last byte.
            if let Some(b) = self.out.last_mut() {
                *b = b.wrapping_add(1);
            }
            if last.wrapping_add(1) == 0xFF {
                self.c &= 0x7FF_FFFF;
                self.push_high(20);
            } else {
                self.push_high(19);
            }
        }
        Ok(())
    }

    /// Output `C >> shift` as the next byte: 7 bits after a 0xFF (bit stuffing), else 8.
    fn push_high(&mut self, shift: u32) {
        self.out.push(((self.c >> shift) & 0xFF) as u8);
        if shift == 20 {
            self.c &= 0xF_FFFF;
            self.ct = 7;
        } else {
            self.c &= 0x7_FFFF;
            self.ct = 8;
        }
    }

    /// FLUSH (E.2.9), then the 0xFF 0xAC marker that ends arithmetically coded data.
    fn finish(mut self) -> Result<Vec<u8>, FilterError> {
        let temp = self.c + self.a;
        self.c |= 0xFFFF;
        if self.c >= temp {
            self.c -= 0x8000;
        }
        self.c <<= self.ct;
        self.byte_out()?;
        self.c <<= self.ct;
        self.byte_out()?;
        let mut out = self.out;
        out.remove(0);
        if out.last() == Some(&0xFF) {
            out.pop();
        }
        out.extend_from_slice(&[0xFF, 0xAC]);
        Ok(out)
    }
}

/// A segment header (T.88 §7.2) for a page-1 segment that refers to no other segment.
fn segment(out: &mut Vec<u8>, number: u32, kind: u8, data: &[u8]) -> Result<(), FilterError> {
    let len = u32::try_from(data.len()).map_err(|_| FilterError::LimitExceeded(u32::MAX as usize))?;
    out.extend_from_slice(&number.to_be_bytes());
    out.push(kind & 0x3F); // one-byte page association, no deferred retention
    out.push(0); // no referred-to segments
    out.push(1); // page 1
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(data);
    Ok(())
}

/// Encode a packed 1-bit image (`width` × `height`, rows padded to whole bytes, 0 = black as in
/// PDF samples) as a `JBIG2Decode` stream: a page information segment and an immediate generic
/// region segment. Returns [`FilterError::LimitExceeded`] once the code grows past `limit` bytes
/// (callers that only want a smaller stream stop early).
pub fn encode_jbig2_generic(bits: &[u8], width: usize, height: usize, limit: usize) -> Result<Vec<u8>, FilterError> {
    let bad = |detail: &str| FilterError::Corrupt { filter: "JBIG2Encode", detail: detail.into() };
    if width == 0 || height == 0 {
        return Err(bad("the image is empty"));
    }
    let (Ok(w32), Ok(h32)) = (u32::try_from(width), u32::try_from(height)) else {
        return Err(bad("the image is too large"));
    };
    // A height of 0xFFFFFFFF means "unknown" in the page information segment.
    if w32 == u32::MAX || h32 == u32::MAX {
        return Err(bad("the image is too large"));
    }
    let stride = width.div_ceil(8);
    let need = stride.checked_mul(height).ok_or_else(|| bad("the image is too large"))?;
    let bits = bits.get(..need).ok_or_else(|| bad("fewer samples than the image needs"))?;

    // Rows unpacked to one JBIG2 pixel (1 = black) per byte, with 4 blank pixels each side so
    // the template never reads outside a row.
    const PAD: usize = 4;
    let row_len = width.checked_add(2 * PAD).ok_or_else(|| bad("the image is too large"))?;
    let unpack = |y: usize, row: &mut [u8]| {
        let src = bits.get(y * stride..(y + 1) * stride).unwrap_or_default();
        for x in 0..width {
            let byte = src.get(x / 8).copied().unwrap_or(0xFF);
            if let Some(p) = row.get_mut(PAD + x) {
                *p = u8::from(byte & (0x80 >> (x % 8)) == 0);
            }
        }
    };
    let mut up2 = vec![0u8; row_len];
    let mut up1 = vec![0u8; row_len];
    let mut cur = vec![0u8; row_len];
    let mut mq = Mq::new(1 << 16, limit);
    let mut ltp = false;
    for y in 0..height {
        unpack(y, &mut cur);
        // Typical prediction: the row is the same as the one above (all white above row 0).
        let same = cur == up1;
        mq.encode(SLTP_CONTEXT, u8::from(same != ltp))?;
        ltp = same;
        if !same {
            for x in 0..width {
                let i = PAD + x;
                let px = |row: &[u8], dx: isize| -> usize { usize::from(row.get(i.wrapping_add_signed(dx)).copied().unwrap_or(0)) };
                // Template 0 with the nominal adaptive pixels (3,-1), (-3,-1), (2,-2), (-2,-2),
                // most significant first (T.88 Figure 3, §6.2.5.3).
                let cx = (px(&up2, -2) << 15)
                    | (px(&up2, -1) << 14)
                    | (px(&up2, 0) << 13)
                    | (px(&up2, 1) << 12)
                    | (px(&up2, 2) << 11)
                    | (px(&up1, -3) << 10)
                    | (px(&up1, -2) << 9)
                    | (px(&up1, -1) << 8)
                    | (px(&up1, 0) << 7)
                    | (px(&up1, 1) << 6)
                    | (px(&up1, 2) << 5)
                    | (px(&up1, 3) << 4)
                    | (px(&cur, -4) << 3)
                    | (px(&cur, -3) << 2)
                    | (px(&cur, -2) << 1)
                    | px(&cur, -1);
                mq.encode(cx, cur.get(i).copied().unwrap_or(0))?;
            }
        }
        std::mem::swap(&mut up2, &mut up1);
        std::mem::swap(&mut up1, &mut cur);
    }
    let code = mq.finish()?;

    let mut out = Vec::with_capacity(code.len() + 64);
    // Page information (type 48): size, unknown resolution, lossless, default pixel 0 (white),
    // OR combination, not striped.
    let mut page = Vec::with_capacity(19);
    page.extend_from_slice(&w32.to_be_bytes());
    page.extend_from_slice(&h32.to_be_bytes());
    page.extend_from_slice(&0u32.to_be_bytes());
    page.extend_from_slice(&0u32.to_be_bytes());
    page.push(0x01);
    page.extend_from_slice(&0u16.to_be_bytes());
    segment(&mut out, 0, 48, &page)?;
    // Immediate generic region (type 38) covering the page.
    let mut region = Vec::with_capacity(code.len() + 26);
    region.extend_from_slice(&w32.to_be_bytes());
    region.extend_from_slice(&h32.to_be_bytes());
    region.extend_from_slice(&0u32.to_be_bytes());
    region.extend_from_slice(&0u32.to_be_bytes());
    region.push(0); // external combination operator OR
    region.push(0b0000_1000); // arithmetic coding, template 0, TPGDON
    for (x, y) in [(3i8, -1i8), (-3, -1), (2, -2), (-2, -2)] {
        region.push(x as u8);
        region.push(y as u8);
    }
    region.extend_from_slice(&code);
    segment(&mut out, 1, 38, &region)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sink {
        width: usize,
        row: Vec<bool>,
        rows: Vec<Vec<bool>>,
    }

    impl hayro_jbig2::Decoder for Sink {
        fn push_pixel(&mut self, black: bool) {
            self.row.push(black);
        }
        fn push_pixel_chunk(&mut self, black: bool, chunk_count: u32) {
            for _ in 0..chunk_count * 8 {
                self.row.push(black);
            }
        }
        fn next_line(&mut self) {
            let mut r = std::mem::take(&mut self.row);
            r.truncate(self.width);
            self.rows.push(r);
        }
    }

    /// Decode with hayro-jbig2 (an independent decoder) back to packed PDF samples.
    fn decode(data: &[u8], w: usize, h: usize) -> Vec<u8> {
        let image = hayro_jbig2::Image::new_embedded(data, None).unwrap();
        assert_eq!((image.width() as usize, image.height() as usize), (w, h));
        let mut sink = Sink { width: w, row: Vec::new(), rows: Vec::new() };
        image.decode(&mut sink).unwrap();
        let stride = w.div_ceil(8);
        let mut out = vec![0u8; stride * h];
        for (y, row) in sink.rows.iter().enumerate().take(h) {
            for x in 0..w {
                // PDF samples: 1 = white.
                if !row.get(x).copied().unwrap_or(false) {
                    out[y * stride + x / 8] |= 0x80 >> (x % 8);
                }
            }
        }
        out
    }

    /// Packed bits with the padding bits of each row cleared, as `decode` leaves them.
    fn normalised(bits: &[u8], w: usize, h: usize) -> Vec<u8> {
        let stride = w.div_ceil(8);
        let mut v = bits[..stride * h].to_vec();
        if !w.is_multiple_of(8) {
            let mask = 0xFFu8 >> (w % 8);
            for y in 0..h {
                v[y * stride + stride - 1] &= !mask;
            }
        }
        v
    }

    fn round_trip(bits: &[u8], w: usize, h: usize) -> usize {
        let code = encode_jbig2_generic(bits, w, h, usize::MAX).unwrap();
        assert_eq!(decode(&code, w, h), normalised(bits, w, h), "{w}×{h}");
        code.len()
    }

    #[test]
    fn every_small_image_round_trips() {
        let mut seed = 0x2545_F491u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for (w, h) in [(1usize, 1usize), (7, 3), (8, 8), (9, 2), (33, 17), (64, 1), (1, 64), (100, 40)] {
            let stride = w.div_ceil(8);
            for density in [0u32, 1, 8, 50, 100] {
                let bits: Vec<u8> =
                    (0..stride * h).map(|_| (0..8).fold(0u8, |b, i| if next() % 100 < density { b } else { b | (0x80 >> i) })).collect();
                round_trip(&bits, w, h);
            }
        }
    }

    #[test]
    fn text_like_pages_compress_and_repeat_rows_cheaply() {
        let (w, h) = (1200usize, 400usize);
        let stride = w.div_ceil(8);
        let mut bits = vec![0xFFu8; stride * h];
        // Bars of text-like strokes with blank leading between.
        for y in 0..h {
            if (y / 20) % 2 == 0 {
                for x in 0..w {
                    if (x / 3 + y / 4) % 5 == 0 {
                        bits[y * stride + x / 8] &= !(0x80 >> (x % 8));
                    }
                }
            }
        }
        let n = round_trip(&bits, w, h);
        assert!(n < bits.len() / 8, "{n} bytes for {} bytes of samples", bits.len());
        let blank = vec![0xFFu8; stride * h];
        // Segment headers and fields take 67 bytes; the 400 rows take a few more.
        assert!(round_trip(&blank, w, h) < 80, "a blank page costs one decision per row");
    }

    #[test]
    fn bad_input_is_an_error_and_limits_stop_early() {
        assert!(encode_jbig2_generic(&[], 0, 1, 100).is_err());
        assert!(encode_jbig2_generic(&[0; 3], 8, 4, 100).is_err(), "too few samples");
        assert!(encode_jbig2_generic(&[0; 1], usize::MAX, 1, 100).is_err());
        let noise: Vec<u8> = (0..4096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        assert!(matches!(encode_jbig2_generic(&noise, 256, 128, 64), Err(FilterError::LimitExceeded(64))));
    }
}

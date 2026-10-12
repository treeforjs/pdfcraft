//! Results from decoding filtered data streams.

mod ascii_85;
pub(crate) mod ascii_hex;
#[cfg(feature = "images")]
mod ccitt;
#[cfg(feature = "images")]
mod dct;
#[cfg(feature = "images")]
mod jbig2;
#[cfg(feature = "images")]
mod jpx;
mod lzw_flate;
mod run_length;

use crate::object::Dict;
use crate::object::Name;
use crate::object::dict::keys::*;
use crate::object::stream::{DecodeFailure, FilterResult, ImageDecodeParams};
use core::ops::Deref;

/// A data filter.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Filter {
    /// ASCII hexadecimal encoding.
    AsciiHexDecode,
    /// ASCII base-85 encoding.
    Ascii85Decode,
    /// Lempel-Ziv-Welch (LZW) compression.
    LzwDecode,
    /// DEFLATE compression (zlib/gzip).
    FlateDecode,
    /// Run-length encoding compression.
    RunLengthDecode,
    /// CCITT Group 3 or Group 4 fax compression.
    CcittFaxDecode,
    /// JBIG2 compression for bi-level images.
    Jbig2Decode,
    /// JPEG (DCT) compression.
    DctDecode,
    /// JPEG 2000 compression.
    JpxDecode,
    /// Encryption filter.
    Crypt,
}

impl Filter {
    fn debug_name(&self) -> &'static str {
        match self {
            Self::AsciiHexDecode => "ascii_hex",
            Self::Ascii85Decode => "ascii_85",
            Self::LzwDecode => "lzw",
            Self::FlateDecode => "flate",
            Self::RunLengthDecode => "run-length",
            Self::CcittFaxDecode => "ccit_fax",
            Self::Jbig2Decode => "jbig2",
            Self::DctDecode => "dct",
            Self::JpxDecode => "jpx",
            Self::Crypt => "crypt",
        }
    }

    pub(crate) fn from_name(name: Name<'_>) -> Option<Self> {
        match name.deref() {
            ASCII_HEX_DECODE | ASCII_HEX_DECODE_ABBREVIATION => Some(Self::AsciiHexDecode),
            ASCII85_DECODE | ASCII85_DECODE_ABBREVIATION => Some(Self::Ascii85Decode),
            LZW_DECODE | LZW_DECODE_ABBREVIATION => Some(Self::LzwDecode),
            FLATE_DECODE | FLATE_DECODE_ABBREVIATION => Some(Self::FlateDecode),
            RUN_LENGTH_DECODE | RUN_LENGTH_DECODE_ABBREVIATION => Some(Self::RunLengthDecode),
            CCITTFAX_DECODE | CCITTFAX_DECODE_ABBREVIATION => Some(Self::CcittFaxDecode),
            JBIG2_DECODE => Some(Self::Jbig2Decode),
            DCT_DECODE | DCT_DECODE_ABBREVIATION => Some(Self::DctDecode),
            JPX_DECODE => Some(Self::JpxDecode),
            CRYPT => Some(Self::Crypt),
            _ => {
                warn!("unknown filter: {}", name.as_str());

                None
            }
        }
    }

    /// PdfCraft patch: `limit` is the most bytes the filter may produce (see `decode_limit`).
    pub(crate) fn apply(
        &self,
        data: &[u8],
        params: &Dict<'_>,
        #[cfg_attr(not(feature = "images"), allow(unused))] image_params: &ImageDecodeParams,
        limit: usize,
    ) -> Result<FilterResult<'static>, DecodeFailure> {
        let res = match self {
            Self::AsciiHexDecode => ascii_hex::decode(data)
                .map(FilterResult::from_data)
                .ok_or(DecodeFailure::StreamDecode),
            Self::Ascii85Decode => ascii_85::decode(data)
                .map(FilterResult::from_data)
                .ok_or(DecodeFailure::StreamDecode),
            Self::RunLengthDecode => run_length::decode(data, limit)
                .map(FilterResult::from_data)
                .ok_or(DecodeFailure::StreamDecode),
            Self::LzwDecode => lzw_flate::lzw::decode(data, params, limit)
                .map(FilterResult::from_data)
                .ok_or(DecodeFailure::StreamDecode),
            Self::FlateDecode => lzw_flate::flate::decode(data, params, limit)
                .map(FilterResult::from_data)
                .ok_or(DecodeFailure::StreamDecode),
            #[cfg(feature = "images")]
            Self::DctDecode => {
                dct::decode(data, params, image_params).ok_or(DecodeFailure::ImageDecode)
            }
            #[cfg(feature = "images")]
            Self::CcittFaxDecode => {
                ccitt::decode(data, params, image_params).ok_or(DecodeFailure::ImageDecode)
            }
            #[cfg(feature = "images")]
            Self::Jbig2Decode => {
                jbig2::decode(data, params, image_params).ok_or(DecodeFailure::ImageDecode)
            }
            #[cfg(feature = "images")]
            Self::JpxDecode => jpx::decode(data, image_params).ok_or(DecodeFailure::ImageDecode),
            #[cfg(not(feature = "images"))]
            Self::DctDecode | Self::CcittFaxDecode | Self::Jbig2Decode | Self::JpxDecode => {
                warn!("image decoding is not supported (enable the `images` feature)");
                Err(DecodeFailure::ImageDecode)
            }
            _ => Err(DecodeFailure::StreamDecode),
        };

        if res.is_err() {
            warn!("failed to apply filter {}", self.debug_name());
        }

        res
    }
}

/// PdfCraft patch: the most bytes Flate, LZW or RunLength may produce for a stream that isn't an
/// image of known size: content streams, fonts, forms, ICC profiles. These filters expand without
/// limit (Flate about 1000:1), so a 2 MB page that inflated to 2 GB took the renderer past 3 GB in
/// under a second. They never write past the limit (the buffer can still grow to under twice
/// it). Real streams are far smaller; the same figure bounds lopdf's load-time decoding.
pub const MAX_DECODED_STREAM: usize = 256 << 20;

/// PdfCraft patch: the most bytes they may produce for an image of known size, however large it
/// says it is.
const MAX_DECODED_IMAGE: u64 = 1 << 30;

/// PdfCraft patch: the output limit for one filter of a stream decoded with `params`; `last`
/// says whether it is the last filter. The last filter of an image whose size, bits per
/// component and components are known produces its pixels, plus at most one PNG predictor tag
/// byte per pixel byte (a predictor row can be as short as one byte): bytes past that are never
/// drawn. Earlier filters produce compressed data, which can be larger than the pixels, and
/// everything else gets [`MAX_DECODED_STREAM`]. Decoding stops at the limit and keeps what it
/// has.
pub(crate) fn decode_limit(params: &ImageDecodeParams, last: bool) -> usize {
    let (true, Some(bpc), Some(components)) = (last, params.bpc, params.num_components) else {
        return MAX_DECODED_STREAM;
    };
    if params.width == 0 || params.height == 0 {
        return MAX_DECODED_STREAM;
    }
    let bits = u64::from(params.width) * u64::from(components) * u64::from(bpc);
    let pixels = bits.div_ceil(8).saturating_mul(u64::from(params.height));
    let image = pixels.saturating_mul(2).min(MAX_DECODED_IMAGE);
    usize::try_from(image).unwrap_or(MAX_DECODED_STREAM)
}

/// PdfCraft patch: the most samples (pixels × components, 1 GiB of 32-bit floats) a JPEG 2000
/// tile, or a decoded JPEG 2000 image, may hold. See [`jpx_size_ok`].
pub const MAX_JPX_SAMPLES: u64 = 1 << 28;

/// PdfCraft patch: whether the JPEG 2000 codestream in `data` may be decoded. hayro-jpeg2000
/// keeps a 32-bit float for every sample of a whole tile at full resolution, whatever the target
/// resolution, so its memory follows the codestream's SIZ marker and not the image dictionary: a
/// 12608 × 16806 four-component scan in one tile asked for 3.4 GB at once, and a 1 × 1 image whose
/// codestream declared 60000 × 60000 × 4, 57.6 GB (an abort on any machine). A tile of more than
/// [`MAX_JPX_SAMPLES`] is refused. Data without a SIZ marker is left to the decoder to reject.
pub fn jpx_size_ok(data: &[u8]) -> bool {
    // SIZ directly follows SOC (ISO 15444-1 A.5.1), in a raw codestream and in a JP2 file's
    // contiguous codestream box alike.
    let Some(at) = data.windows(4).position(|w| w == [0xff, 0x4f, 0xff, 0x51]) else {
        return true;
    };
    let siz = data.get(at + 4..).unwrap_or_default();
    let u32_at = |o: usize| siz.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    // Lsiz, Rsiz, Xsiz, Ysiz, XOsiz, YOsiz, XTsiz, YTsiz, XTOsiz, YTOsiz, Csiz, then
    // (Ssiz, XRsiz, YRsiz) per component.
    let (Some(x), Some(y), Some(x0), Some(y0), Some(tile_w), Some(tile_h), Some(c)) = (
        u32_at(4),
        u32_at(8),
        u32_at(12),
        u32_at(16),
        u32_at(20),
        u32_at(24),
        siz.get(36..38).map(|b| u16::from_be_bytes([b[0], b[1]])),
    ) else {
        // A truncated SIZ marker: the decoder can't use it either.
        return false;
    };
    // The largest tile, clipped to the image area: an upper bound on every tile's size.
    let tile_w = u64::from(tile_w.min(x.saturating_sub(x0)));
    let tile_h = u64::from(tile_h.min(y.saturating_sub(y0)));
    let mut samples: u64 = 0;
    for i in 0..usize::from(c) {
        let (Some(&dx), Some(&dy)) = (siz.get(38 + 3 * i + 1), siz.get(38 + 3 * i + 2)) else {
            return false;
        };
        let (dx, dy) = (u64::from(dx.max(1)), u64::from(dy.max(1)));
        samples = samples.saturating_add(tile_w.div_ceil(dx).saturating_mul(tile_h.div_ceil(dy)));
    }
    samples <= MAX_JPX_SAMPLES
}

/// PdfCraft patch: whether a CCITT image of `columns` × `rows` may be decoded. The decoder sizes
/// its line buffers from `/Columns`, which a fuzzed file set to 4294967295 (a 4 GiB allocation);
/// real fax lines are a few thousand pixels wide.
pub fn ccitt_size_ok(columns: u32, rows: u32) -> bool {
    const MAX_COLUMNS: u32 = 1 << 20;
    const MAX_PIXELS: u64 = 1 << 28;
    columns > 0 && columns <= MAX_COLUMNS && u64::from(columns) * u64::from(rows.max(1)) <= MAX_PIXELS
}

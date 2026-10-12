//! Strict, bounded content decoding; the bootstrap viewer's tolerant partial buffers must
//! never become an apparently complete native vector page.

use super::{Result, refused};
use hayro::hayro_syntax::object::{Array, Dict, Name, Object, Stream};
use pdfcraft_filters::{Filter, Params};

fn array_values<'a>(array: &Array<'a>) -> Result<Vec<Object<'a>>> {
    let count = array.raw_iter().take(17).count();
    if count > 16 {
        return Err(refused("too many filters or parameters"));
    }
    let values: Vec<_> = array.iter::<Object<'_>>().take(17).collect();
    if values.len() != count {
        return Err(refused("unresolved filter or parameter"));
    }
    Ok(values)
}

fn parameters(dict: &Dict<'_>) -> Result<Params> {
    let get = |key: &[u8], default: i32| -> Result<i64> {
        if dict.contains_key(key) {
            let Some(Object::Number(number)) = dict.get::<Object<'_>>(key) else {
                return Err(refused("invalid integer filter parameter"));
            };
            let value = number.as_f64();
            if !value.is_finite() || value.fract() != 0.0 || value < f64::from(i32::MIN) || value > f64::from(i32::MAX) {
                return Err(refused("invalid integer filter parameter"));
            }
            Ok(number.as_i64())
        } else {
            Ok(i64::from(default))
        }
    };
    Ok(Params {
        predictor: get(b"Predictor", 1)?,
        colors: get(b"Colors", 1)?,
        bits_per_component: get(b"BitsPerComponent", 8)?,
        columns: get(b"Columns", 1)?,
        early_change: get(b"EarlyChange", 1)?,
    })
}

/// Encoded bytes one stream may hold for the native path.
const MAX_ENCODED_STREAM: usize = 4 * 1024 * 1024;
/// Encoded bytes all of a page's content streams may hold together. One small stream named
/// 256 times in `/Contents` would otherwise be inflated 256 times.
pub(super) const MAX_ENCODED_PAGE: usize = 16 * 1024 * 1024;

/// Decodes one content stream within `max` decoded bytes, charging its encoded bytes to
/// `encoded_left`.
pub(super) fn stream(stream: &Stream<'_>, max: usize, encoded_left: &mut usize) -> Result<Vec<u8>> {
    let dict = stream.dict();
    if [b"F".as_slice(), b"FFilter", b"FDecodeParms", b"DP"].iter().any(|key| dict.contains_key(key)) {
        return Err(refused("external or abbreviated stream entries need compatibility decoding"));
    }
    let filters = if let Some(name) = dict.get::<Name<'_>>(b"Filter") {
        vec![name]
    } else if let Some(array) = dict.get::<Array<'_>>(b"Filter") {
        array_values(&array)?
            .into_iter()
            .map(|value| if let Object::Name(name) = value { Ok(name) } else { Err(refused("filter is not a name")) })
            .collect::<Result<Vec<_>>>()?
    } else if dict.contains_key(b"Filter") {
        return Err(refused("invalid filter entry"));
    } else {
        Vec::new()
    };
    let params = if let Some(params) = dict.get::<Dict<'_>>(b"DecodeParms") {
        if filters.len() != 1 {
            return Err(refused("filter parameter count mismatch"));
        }
        vec![parameters(&params)?]
    } else if let Some(array) = dict.get::<Array<'_>>(b"DecodeParms") {
        let values = array_values(&array)?;
        if values.len() != filters.len() {
            return Err(refused("filter parameter count mismatch"));
        }
        values
            .into_iter()
            .map(|value| match value {
                Object::Null(_) => Ok(Params::default()),
                Object::Dict(dict) => parameters(&dict),
                _ => Err(refused("invalid filter parameter dictionary")),
            })
            .collect::<Result<Vec<_>>>()?
    } else if dict.contains_key(b"DecodeParms") && !matches!(dict.get::<Object<'_>>(b"DecodeParms"), Some(Object::Null(_))) {
        return Err(refused("invalid filter parameters"));
    } else {
        vec![Params::default(); filters.len()]
    };
    let chain = filters
        .iter()
        .zip(params)
        .map(|(name, params)| {
            if name.len() > 32 {
                return Err(refused("unsupported filter name"));
            }
            let filter = Filter::from_name(name);
            // These other compatibility decoders accept missing end markers. Until framing
            // is validated, they must stay on the bootstrap path instead of making a
            // truncated prefix look like a complete native page. Flate requires StreamEnd.
            if filter != Filter::Flate {
                return Err(refused("content uses an unsupported decoder"));
            }
            // A predictor reshapes rows after inflation, and a short last row is padded or
            // dropped by convention rather than proven complete. Content streams have no rows.
            if params.predictor != 1 {
                return Err(refused("content stream uses a predictor"));
            }
            Ok((filter, params))
        })
        .collect::<Result<Vec<_>>>()?;
    let data = stream
        .raw_data_within(MAX_ENCODED_STREAM.min(*encoded_left))
        .ok_or_else(|| refused("encoded content budget exceeded or decryption failed"))?;
    *encoded_left = encoded_left.saturating_sub(data.len());
    if chain.is_empty() && data.len() > max {
        return Err(refused("decoded content budget exceeded"));
    }
    pdfcraft_filters::decode(&chain, &data, max).map_err(|_| refused("content stream is damaged or exceeds its decode budget"))
}

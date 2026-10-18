//! pdfcraft-render — page rasterization and document inspection.
//!
//! **Bootstrap status (see ADR-0004):** rasterization goes straight through the `hayro` crate
//! (hayro-interpret + vello_cpu). Inspection uses the lazy `cos` reader with a read-only lopdf
//! object adapter and retains `lopdf` loading for compatibility repairs. The renderer and that
//! adapter are replaced by `model` and the DisplayList design in M1–M2. The public API is what the engine and UI rely on, so
//! the swap stays internal to this crate.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod display_list;
mod inspect;
mod pixels;
mod raster;
mod structure;
pub mod svg;
pub mod text;
mod vector_raster;

pub use inspect::{
    Annotation, Attachment, AttachmentSource, DestView, DocInfo, Field, FieldKind, FontInfo, Layer, LayerOp, Link, LinkTarget, OutlineItem, PageInfo,
    Xfa, attachment_data, inspect, pretty_date,
};
pub use pixels::Pixels;
pub use raster::{
    MAX_PIXELS, MAX_SIDE, PageRenderer, RenderConfig, RenderPool, RenderRequest, RenderStats, RenderWarning, RenderedPage, RequestKind, Tile,
    device_pixels, effective_scale, native_vector_default, native_vector_requested, set_native_vector_default,
};
pub use svg::{SvgOptions, SvgPage, SvgStats};
pub use text::{PageText, TextGlyph, find_in_chars};

/// The program of the substitute the renderer draws a standard-14 font with when a PDF doesn't
/// embed it (the BSD-3-Clause Foxit/PDFium faces vendored in hayro-interpret, `ATTRIBUTION.toml`),
/// as bare CFF data (`FontFile3 /Subtype /Type1C`). `base_font` is one of the fourteen exact
/// standard names (`Helvetica`, `Times-BoldItalic`, `ZapfDingbats`, …); anything else is `None`.
/// Convert to PDF/X embeds them so that unembedded standard fonts print as they display.
pub fn standard_font_program(base_font: &str) -> Option<Vec<u8>> {
    use hayro::hayro_interpret::font::StandardFont as S;
    let font = match base_font {
        "Helvetica" => S::Helvetica,
        "Helvetica-Bold" => S::HelveticaBold,
        "Helvetica-Oblique" => S::HelveticaOblique,
        "Helvetica-BoldOblique" => S::HelveticaBoldOblique,
        "Courier" => S::Courier,
        "Courier-Bold" => S::CourierBold,
        "Courier-Oblique" => S::CourierOblique,
        "Courier-BoldOblique" => S::CourierBoldOblique,
        "Times-Roman" => S::TimesRoman,
        "Times-Bold" => S::TimesBold,
        "Times-Italic" => S::TimesItalic,
        "Times-BoldItalic" => S::TimesBoldItalic,
        "Symbol" => S::Symbol,
        "ZapfDingbats" => S::ZapfDingBats,
        _ => return None,
    };
    let (data, _) = font.get_font_data();
    Some((*data).as_ref().to_vec())
}

/// DeviceCMYK colours (quadruples, 0–255 each) as RGB triples, converted exactly as pages
/// render them (the renderer's own CMYK profile). Overprint preview composites its ink plates
/// with it, so simulated areas match the colours of the rest of the page. `None` when `cmyk`
/// isn't a whole number of quadruples or the conversion fails.
pub fn device_cmyk_to_rgb(cmyk: &[u8]) -> Option<Vec<u8>> {
    if !cmyk.len().is_multiple_of(4) {
        return None;
    }
    let mut out = vec![0u8; cmyk.len() / 4 * 3];
    hayro::hayro_interpret::color::device_cmyk_to_rgb8(cmyk, &mut out)?;
    Some(out)
}

/// Errors surfaced to the user when a document cannot be opened.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("the file is not a readable PDF: {0}")]
    Invalid(String),
    #[error("the document is protected by a password")]
    NeedsPassword,
    #[error("the password is incorrect")]
    WrongPassword,
    #[error("{0}")]
    Unsupported(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_cmyk_converts_as_pages_render_it() {
        let colours = [[0.0, 1.0, 0.0, 0.0], [1.0, 0.0, 1.0, 0.0], [0.2, 0.4, 0.6, 0.1], [0.0, 0.0, 0.0, 1.0]];
        for c in colours {
            let content = format!("{} {} {} {} k 0 0 10 10 re f", c[0], c[1], c[2], c[3]);
            let pdf = format!(
                "%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
                 2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
                 3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >> endobj\n\
                 4 0 obj << /Length {} >> stream\n{content}\nendstream endobj\ntrailer << /Root 1 0 R >>\n%%EOF",
                content.len()
            );
            let mut renderer = PageRenderer::new(std::sync::Arc::new(pdf.into_bytes()), RenderConfig::default());
            let page = renderer.render(RenderRequest { page: 0, kind: RequestKind::Pixels, scale: 1.0, ..Default::default() });
            assert!(page.error.is_none(), "{:?}", page.error);
            let at = (5 * page.width as usize + 5) * 4;
            let rendered = &page.rgba[at..at + 3];
            let quad = c.map(|v: f64| (v * 255.0).round() as u8);
            let converted = device_cmyk_to_rgb(&quad).unwrap();
            for (a, b) in rendered.iter().zip(&converted) {
                assert!(a.abs_diff(*b) <= 2, "{c:?}: rendered {rendered:?}, converted {converted:?}");
            }
        }
        assert!(device_cmyk_to_rgb(&[0, 0, 0]).is_none());
        assert_eq!(device_cmyk_to_rgb(&[]).unwrap(), Vec::<u8>::new());
    }
}

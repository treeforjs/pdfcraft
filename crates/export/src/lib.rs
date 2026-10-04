//! printcraft-export — Export a PDF ▸ Word, HTML, RTF (L4).
//!
//! The engine reduces each page to [`Page`]: paragraphs (text, box, size, bold/italic) and
//! images (encoded bytes and box), in reading order. The writers turn that into a flowing
//! document: headings are the paragraphs set larger than the body text, and images sit where
//! they fall between paragraphs.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod zip;

pub use zip::Zip;

/// A paragraph of a page.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub text: String,
    /// [x0, y0, x1, y1] in user space (y up).
    pub rect: [f64; 4],
    /// Font size in points.
    pub size: f64,
    pub bold: bool,
    pub italic: bool,
}

/// An image of a page.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    /// "png" or "jpg".
    pub ext: &'static str,
    pub bytes: Vec<u8>,
    pub rect: [f64; 4],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    pub width: f64,
    pub height: f64,
    pub blocks: Vec<Block>,
    pub images: Vec<Image>,
}

/// What goes into the output, in order.
enum Item<'a> {
    Para(&'a Block, u8),
    Img(&'a Image),
    PageBreak,
}

/// The body text size: the size most of the document's characters are set in.
fn body_size(pages: &[Page]) -> f64 {
    let mut by: Vec<(i64, usize)> = Vec::new();
    for b in pages.iter().flat_map(|p| &p.blocks) {
        let k = (b.size * 2.0).round() as i64;
        match by.iter_mut().find(|e| e.0 == k) {
            Some(e) => e.1 += b.text.len(),
            None => by.push((k, b.text.len())),
        }
    }
    by.into_iter().max_by_key(|e| e.1).map_or(11.0, |e| e.0 as f64 / 2.0)
}

/// Heading level for a block: 1 for ≥ 1.6× the body size, 2 for ≥ 1.25×, 0 for body text.
fn level(b: &Block, body: f64) -> u8 {
    let short = b.text.len() < 200;
    if short && b.size >= body * 1.6 {
        1
    } else if short && (b.size >= body * 1.25 || (b.bold && b.size >= body && b.text.len() < 80 && !b.text.ends_with('.'))) {
        2
    } else {
        0
    }
}

fn items(pages: &[Page]) -> Vec<Item<'_>> {
    let body = body_size(pages);
    let mut out = Vec::new();
    for (i, p) in pages.iter().enumerate() {
        if i > 0 {
            out.push(Item::PageBreak);
        }
        // Blocks and images by their top edge, top to bottom.
        let mut parts: Vec<(f64, f64, Item)> = p.blocks.iter().map(|b| (b.rect[3], b.rect[0], Item::Para(b, level(b, body)))).collect();
        parts.extend(p.images.iter().map(|im| (im.rect[3], im.rect[0], Item::Img(im))));
        parts.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.total_cmp(&b.1)));
        out.extend(parts.into_iter().map(|x| x.2));
    }
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(A[(n >> 18) as usize & 63] as char);
        s.push(A[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { A[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { A[n as usize & 63] as char } else { '=' });
    }
    s
}

/// One HTML file: headings and paragraphs, images inline, a rule between pages.
pub fn html(pages: &[Page], title: &str) -> String {
    let mut s = format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>{}</title>\n<style>body{{max-width:46em;margin:2em auto;font-family:sans-serif;line-height:1.45}}img{{max-width:100%}}hr{{border:0;border-top:1px solid #ccc;margin:2em 0}}</style>\n</head>\n<body>\n",
        esc(title)
    );
    for it in items(pages) {
        match it {
            Item::Para(b, lvl) => {
                let mut t = esc(&b.text);
                if b.italic {
                    t = format!("<em>{t}</em>");
                }
                if b.bold && lvl == 0 {
                    t = format!("<strong>{t}</strong>");
                }
                match lvl {
                    0 => s.push_str(&format!("<p>{t}</p>\n")),
                    l => s.push_str(&format!("<h{l}>{t}</h{l}>\n")),
                }
            }
            Item::Img(im) => {
                let mime = if im.ext == "jpg" { "image/jpeg" } else { "image/png" };
                s.push_str(&format!("<p><img alt=\"\" src=\"data:{mime};base64,{}\"></p>\n", base64(&im.bytes)));
            }
            Item::PageBreak => s.push_str("<hr>\n"),
        }
    }
    s.push_str("</body>\n</html>\n");
    s
}

/// A Word document (.docx, Office Open XML): Heading 1/2 and Normal paragraphs, images inline at
/// their size on the page, page breaks between pages.
pub fn docx(pages: &[Page], title: &str) -> Vec<u8> {
    let mut body = String::new();
    let mut media: Vec<(String, &Image)> = Vec::new();
    let run = |b: &Block| {
        let mut rpr = String::new();
        if b.bold {
            rpr.push_str("<w:b/>");
        }
        if b.italic {
            rpr.push_str("<w:i/>");
        }
        rpr.push_str(&format!("<w:sz w:val=\"{}\"/>", (b.size * 2.0).round().clamp(2.0, 3276.0) as i64));
        format!("<w:r><w:rPr>{rpr}</w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r>", esc(&b.text))
    };
    for it in items(pages) {
        match it {
            Item::Para(b, lvl) => {
                let style = match lvl {
                    1 => "<w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr>",
                    2 => "<w:pPr><w:pStyle w:val=\"Heading2\"/></w:pPr>",
                    _ => "",
                };
                body.push_str(&format!("<w:p>{style}{}</w:p>", run(b)));
            }
            Item::Img(im) => {
                let n = media.len() + 1;
                let name = format!("image{n}.{}", im.ext);
                // Size on the page, in EMU (12700 per point), at most the text width (6.5 in).
                let (w, h) = ((im.rect[2] - im.rect[0]).max(1.0), (im.rect[3] - im.rect[1]).max(1.0));
                let k = (468.0 / w).min(1.0);
                let (cx, cy) = ((w * k * 12700.0) as i64, (h * k * 12700.0) as i64);
                body.push_str(&format!(
                    "<w:p><w:r><w:drawing><wp:inline><wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:docPr id=\"{n}\" name=\"Picture {n}\"/>\
<a:graphic xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
<pic:pic xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:nvPicPr><pic:cNvPr id=\"{n}\" name=\"{name}\"/><pic:cNvPicPr/></pic:nvPicPr>\
<pic:blipFill><a:blip r:embed=\"rIdImg{n}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
<pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic>\
</a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"
                ));
                media.push((name, im));
            }
            Item::PageBreak => body.push_str("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>"),
        }
    }
    // The first page's size and margins of 1 in.
    let (pw, ph) = pages.first().map_or((612.0, 792.0), |p| (p.width, p.height));
    let doc = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\"><w:body>{body}\
<w:sectPr><w:pgSz w:w=\"{}\" w:h=\"{}\"/><w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr></w:body></w:document>",
        (pw * 20.0) as i64,
        (ph * 20.0) as i64
    );
    let mut types = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Default Extension=\"png\" ContentType=\"image/png\"/><Default Extension=\"jpg\" ContentType=\"image/jpeg\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>",
    );
    types.push_str("</Types>");
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/></Relationships>";
    let mut doc_rels = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rIdStyles\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>",
    );
    for (i, (name, _)) in media.iter().enumerate() {
        doc_rels.push_str(&format!(
            "<Relationship Id=\"rIdImg{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/{name}\"/>",
            i + 1
        ));
    }
    doc_rels.push_str("</Relationships>");
    let styles = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:pPr><w:spacing w:after=\"120\"/></w:pPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading1\"><w:name w:val=\"heading 1\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:pPr><w:keepNext/><w:spacing w:before=\"240\"/><w:outlineLvl w:val=\"0\"/></w:pPr><w:rPr><w:b/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading2\"><w:name w:val=\"heading 2\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:pPr><w:keepNext/><w:spacing w:before=\"200\"/><w:outlineLvl w:val=\"1\"/></w:pPr><w:rPr><w:b/></w:rPr></w:style>\
</w:styles>";
    let core = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>{}</dc:title></cp:coreProperties>",
        esc(title)
    );
    let mut z = Zip::default();
    z.add("[Content_Types].xml", types.as_bytes(), true);
    z.add("_rels/.rels", rels.as_bytes(), true);
    z.add("docProps/core.xml", core.as_bytes(), true);
    z.add("word/document.xml", doc.as_bytes(), true);
    z.add("word/styles.xml", styles.as_bytes(), true);
    z.add("word/_rels/document.xml.rels", doc_rels.as_bytes(), true);
    for (name, im) in &media {
        z.add(&format!("word/media/{name}"), &im.bytes, false);
    }
    z.finish()
}

fn rtf_text(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '\\' | '{' | '}' => {
                o.push('\\');
                o.push(c);
            }
            c if (c as u32) < 128 => o.push(c),
            c => {
                // \uN with a ? fallback; UTF-16 units as signed 16-bit numbers.
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    o.push_str(&format!("\\u{}?", *u as i16));
                }
            }
        }
    }
    o
}

/// Rich Text Format: paragraphs with their sizes and bold/italic, page breaks between pages
/// (images are left out).
pub fn rtf(pages: &[Page]) -> String {
    let mut s = String::from("{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0 Helvetica;}}\n");
    for it in items(pages) {
        match it {
            Item::Para(b, _) => {
                let mut fmt = format!("\\fs{}", (b.size * 2.0).round() as i64);
                if b.bold {
                    fmt.push_str("\\b");
                }
                if b.italic {
                    fmt.push_str("\\i");
                }
                s.push_str(&format!("{{\\pard{fmt} {}\\par}}\n", rtf_text(&b.text)));
            }
            Item::Img(_) => {}
            Item::PageBreak => s.push_str("\\page\n"),
        }
    }
    s.push('}');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> Page {
        let b = |t: &str, y: f64, size: f64, bold: bool| Block { text: t.into(), rect: [72.0, y, 500.0, y + size], size, bold, italic: false };
        Page {
            width: 612.0,
            height: 792.0,
            blocks: vec![
                b("Body text after the picture, which is long enough to be the main text size of the page.", 400.0, 11.0, false),
                b("Annual Report", 700.0, 24.0, true),
                b("Overview", 650.0, 14.0, true),
                b("Some <body> text & more of it so that eleven points is the body size here.", 620.0, 11.0, false),
            ],
            images: vec![Image {
                ext: "png",
                bytes: b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x02\0\0\0\x03".to_vec(),
                rect: [72.0, 450.0, 272.0, 600.0],
            }],
        }
    }

    #[test]
    fn html_has_headings_paragraphs_and_images_in_order() {
        let h = html(&[page(), page()], "Report");
        let order: Vec<usize> = ["<h1>Annual Report</h1>", "<h2>Overview</h2>", "<p>Some &lt;body&gt; text &amp;", "<img", "<p>Body text after"]
            .iter()
            .map(|x| h.find(x).unwrap_or_else(|| panic!("{x} missing in {h}")))
            .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
        assert_eq!(h.matches("<hr>").count(), 1);
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
    }

    #[test]
    fn docx_is_a_word_package() {
        let d = docx(&[page()], "Report");
        assert!(d.starts_with(b"PK"));
        let names: Vec<&str> =
            ["[Content_Types].xml", "word/document.xml", "word/styles.xml", "word/_rels/document.xml.rels", "word/media/image1.png", "_rels/.rels"]
                .to_vec();
        for n in names {
            assert!(d.windows(n.len()).any(|w| w == n.as_bytes()), "{n}");
        }
    }

    #[test]
    fn rtf_escapes_and_sizes() {
        let mut p = page();
        p.blocks.push(Block { text: "Café {x}".into(), rect: [72.0, 100.0, 200.0, 110.0], size: 10.0, bold: false, italic: true });
        let r = rtf(&[p]);
        assert!(r.starts_with("{\\rtf1") && r.ends_with('}'));
        assert!(r.contains("\\fs48\\b Annual Report"));
        assert!(r.contains("\\fs20\\i Caf\\u233? \\{x\\}"), "{r}");
    }
}

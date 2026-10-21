//! Synthetic fixture PDFs for `cargo xtask fidelity`, written as hand-made COS.
//!
//! Every fixture is generated here from code: page content, images (procedural pixels) and fonts
//! (the standard 14 by name, or the repository's OFL fonts from `assets/fonts/`, already listed in
//! ATTRIBUTION.toml). Nothing comes from PDF, the pdf.js corpus or any third-party document,
//! so the files and every number measured on them are contributor-original (AGENTS.md §1). The
//! fixtures are written under the output directory at run time and never committed.
//!
//! Each page also carries its *ground truth* text: the words the content stream shows (inside
//! the crop box, invisible text included, annotation and field text excluded), in reading order.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::ttf::Ttf;

pub struct Fixture {
    pub name: &'static str,
    pub about: &'static str,
    pub bytes: Vec<u8>,
    /// Expected text per page, words in reading order.
    pub truth: Vec<String>,
}

impl Fixture {
    pub fn pages(&self) -> usize {
        self.truth.len()
    }
}

/// All fixtures, in report order. `fonts` is `assets/fonts/`.
pub fn all(fonts: &Path) -> Result<Vec<Fixture>> {
    Ok(vec![
        standard_fonts(),
        embedded_fonts(fonts)?,
        text_operators(),
        type3_fonts(),
        vector_art(),
        shadings_patterns(),
        color_spaces(),
        images(),
        transparency(),
        forms(),
        annotations(),
        rotated_pages(),
        page_boxes(),
    ])
}

// ---------------------------------------------------------------------------------------------
// COS writer

/// Objects numbered from 1, written with a classic xref table.
#[derive(Default)]
pub struct Pdf {
    objects: Vec<Vec<u8>>,
}

impl Pdf {
    pub fn reserve(&mut self) -> u32 {
        self.objects.push(Vec::new());
        u32::try_from(self.objects.len()).unwrap_or(u32::MAX)
    }

    pub fn set(&mut self, id: u32, body: impl AsRef<[u8]>) {
        if let Some(slot) = (id as usize).checked_sub(1).and_then(|i| self.objects.get_mut(i)) {
            *slot = body.as_ref().to_vec();
        }
    }

    pub fn add(&mut self, body: impl AsRef<[u8]>) -> u32 {
        let id = self.reserve();
        self.set(id, body);
        id
    }

    /// A stream object; `dict` holds the entries other than `/Length`.
    pub fn stream(&mut self, dict: &str, data: &[u8]) -> u32 {
        let mut b = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        b.extend_from_slice(data);
        b.extend_from_slice(b"\nendstream");
        self.add(b)
    }

    /// A FlateDecode stream (stored deflate blocks: valid zlib without a compressor dependency).
    pub fn flate(&mut self, dict: &str, data: &[u8]) -> u32 {
        self.stream(&format!("{dict} /Filter /FlateDecode"), &zlib_stored(data))
    }

    pub fn finish(self, root: u32) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut offsets = Vec::with_capacity(self.objects.len());
        for (i, body) in self.objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        let size = self.objects.len() + 1;
        out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
        out
    }
}

/// zlib framing around stored (uncompressed) deflate blocks.
pub fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    if data.is_empty() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    let mut chunks = data.chunks(0xFFFF).peekable();
    while let Some(chunk) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none()));
        let len = u16::try_from(chunk.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in data {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

/// A document: one flat page tree plus extra catalog entries.
struct Doc {
    pdf: Pdf,
    pages: u32,
    pages_extra: String,
    kids: Vec<u32>,
    catalog_extra: String,
    truth: Vec<String>,
}

const MB: &str = "/MediaBox [0 0 400 300]";

impl Doc {
    fn new() -> Self {
        let mut pdf = Pdf::default();
        let pages = pdf.reserve();
        Self { pdf, pages, pages_extra: String::new(), kids: Vec::new(), catalog_extra: String::new(), truth: Vec::new() }
    }

    fn page_at(&mut self, id: u32, attrs: &str, resources: &str, c: &Content, annots: &[u32]) {
        let contents = self.pdf.flate("", c.ops.as_bytes());
        let annots = if annots.is_empty() { String::new() } else { format!(" /Annots [{}]", refs(annots)) };
        let page = format!("<< /Type /Page /Parent {} 0 R {attrs} /Resources << {resources} >> /Contents {contents} 0 R{annots} >>", self.pages);
        self.pdf.set(id, page);
        self.kids.push(id);
        self.truth.push(c.truth.trim().to_string());
    }

    fn page(&mut self, attrs: &str, resources: &str, c: &Content) -> u32 {
        let id = self.pdf.reserve();
        self.page_at(id, attrs, resources, c, &[]);
        id
    }

    fn finish(mut self, name: &'static str, about: &'static str) -> Fixture {
        let kids = refs(&self.kids);
        let pages = format!("<< /Type /Pages /Kids [{kids}] /Count {} {} >>", self.kids.len(), self.pages_extra);
        self.pdf.set(self.pages, pages);
        let catalog = self.pdf.add(format!("<< /Type /Catalog /Pages {} 0 R {} >>", self.pages, self.catalog_extra));
        Fixture { name, about, bytes: self.pdf.finish(catalog), truth: self.truth }
    }
}

fn refs(ids: &[u32]) -> String {
    ids.iter().map(|i| format!("{i} 0 R")).collect::<Vec<_>>().join(" ")
}

/// Content-stream text with the ground truth kept beside it.
#[derive(Default)]
struct Content {
    ops: String,
    truth: String,
}

impl Content {
    fn op(&mut self, s: &str) -> &mut Self {
        self.ops.push_str(s);
        self.ops.push('\n');
        self
    }

    fn truth(&mut self, words: &str) -> &mut Self {
        self.truth.push_str(words);
        self.truth.push('\n');
        self
    }

    /// One line of simple-font text at (x, y).
    fn text(&mut self, font: &str, size: f64, x: f64, y: f64, s: &str) -> &mut Self {
        let _ = writeln!(self.ops, "BT /{font} {size} Tf {x} {y} Td ({}) Tj ET", lit(s));
        self.truth(s)
    }
}

/// A PDF literal string body: WinAnsi bytes, with delimiters escaped.
fn lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '(' | ')' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            ' '..='~' => out.push(ch),
            // Latin-1 is WinAnsi from 0xA0 up.
            '\u{a0}'..='\u{ff}' => {
                let _ = write!(out, "\\{:03o}", u32::from(ch));
            }
            _ => out.push('?'),
        }
    }
    out
}

fn std_font(res: &str, base: &str) -> String {
    format!("/{res} << /Type /Font /Subtype /Type1 /BaseFont /{base} /Encoding /WinAnsiEncoding >>")
}

const HELV: &str = "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >> >>";

/// A closed Bézier circle path (no paint operator).
fn circle(cx: f64, cy: f64, r: f64) -> String {
    let k = 0.552_284_75 * r;
    format!(
        "{} {cy} m {} {} {} {} {cx} {} c {} {} {} {} {} {cy} c {} {} {} {} {cx} {} c {} {} {} {} {} {cy} c h",
        cx + r,
        cx + r,
        cy + k,
        cx + k,
        cy + r,
        cy + r,
        cx - k,
        cy + r,
        cx - r,
        cy + k,
        cx - r,
        cx - r,
        cy - k,
        cx - k,
        cy - r,
        cy - r,
        cx + k,
        cy - r,
        cx + r,
        cy - k,
        cx + r
    )
}

/// A five-pointed star drawn in one stroke order (self-intersecting: even-odd leaves a hole).
fn star(cx: f64, cy: f64, r: f64) -> String {
    let mut s = String::new();
    for i in 0..5 {
        let a = std::f64::consts::FRAC_PI_2 + f64::from(i * 2) * std::f64::consts::TAU / 5.0;
        let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
        let _ = write!(s, "{x:.2} {y:.2} {} ", if i == 0 { "m" } else { "l" });
    }
    s.push('h');
    s
}

// ---------------------------------------------------------------------------------------------
// Fixtures

fn standard_fonts() -> Fixture {
    let mut d = Doc::new();
    let fonts = [
        ("F1", "Helvetica"),
        ("F2", "Times-Roman"),
        ("F3", "Courier"),
        ("F4", "Helvetica-Bold"),
        ("F5", "Times-Italic"),
        ("F6", "Helvetica-Oblique"),
        ("F7", "Courier-Bold"),
        ("F8", "Times-BoldItalic"),
    ];
    let res = format!("/Font << {} >>", fonts.map(|(r, b)| std_font(r, b)).join(" "));
    let mut c = Content::default();
    c.text("F1", 14.0, 24.0, 266.0, "Helvetica regular sets the quick brown fox")
        .text("F4", 14.0, 24.0, 244.0, "Helvetica bold jumps over lazy dogs")
        .text("F2", 14.0, 24.0, 222.0, "Times roman prints classic serif glyphs")
        .text("F5", 14.0, 24.0, 200.0, "Times italic leans gently forward")
        .text("F8", 14.0, 24.0, 178.0, "Times bold italic adds emphasis")
        .text("F3", 12.0, 24.0, 156.0, "Courier monospace column 0123456789")
        .text("F7", 12.0, 24.0, 138.0, "Courier bold terminal output")
        .text("F6", 10.0, 24.0, 118.0, "Small oblique caption at ten points")
        .text("F1", 7.0, 24.0, 104.0, "Tiny seven point footnote text for hinting")
        .text("F2", 28.0, 24.0, 60.0, "Large café heading")
        .text("F1", 9.0, 24.0, 30.0, "Accents déjà vu, naïve façade, Ångström");
    d.page(MB, &res, &c);

    // Two columns: reading order must finish the left one first.
    let mut c = Content::default();
    let left = ["Columns test the reading", "order of extracted text", "where the left column", "should come before the", "right column entirely"];
    let right = ["Right column begins here", "after the left side ends", "and finishes this page", "with a final sentence", "about reading order"];
    for (i, line) in left.iter().enumerate() {
        c.text("F2", 11.0, 24.0, 260.0 - 14.0 * i as f64, line);
    }
    for (i, line) in right.iter().enumerate() {
        c.text("F2", 11.0, 214.0, 260.0 - 14.0 * i as f64, line);
    }
    c.op("0.5 g 200 120 0.5 150 re f");
    d.page(MB, &res, &c);
    d.finish("text-standard-fonts", "Standard 14 fonts (WinAnsi, accents), sizes 7–28 pt, a two-column page")
}

/// An embedded simple TrueType font with WinAnsi encoding and computed widths.
fn simple_truetype(pdf: &mut Pdf, bytes: &[u8], name: &str) -> Result<u32> {
    let ttf = Ttf::parse(bytes).with_context(|| name.to_string())?;
    let file = pdf.stream(&format!("/Length1 {}", bytes.len()), bytes);
    let fd = pdf.add(descriptor(&ttf, name, 32, file));
    let widths: Vec<String> = (32u8..=255)
        .map(|code| {
            let ch = matches!(code, 32..=126 | 160..=255).then(|| char::from(code));
            ch.and_then(|c| ttf.glyph(c)).map_or(0, |g| ttf.advance(g)).to_string()
        })
        .collect();
    Ok(pdf.add(format!(
        "<< /Type /Font /Subtype /TrueType /BaseFont /{name} /FirstChar 32 /LastChar 255 /Widths [{}] /Encoding /WinAnsiEncoding /FontDescriptor {fd} 0 R >>",
        widths.join(" ")
    )))
}

fn descriptor(ttf: &Ttf, name: &str, flags: u32, file: u32) -> String {
    let [x0, y0, x1, y1] = ttf.bbox.map(|v| ttf.scale(i32::from(v)));
    let (asc, desc) = (ttf.scale(i32::from(ttf.ascent)), ttf.scale(i32::from(ttf.descent)));
    format!(
        "<< /Type /FontDescriptor /FontName /{name} /Flags {flags} /FontBBox [{x0} {y0} {x1} {y1}] /ItalicAngle 0 /Ascent {asc} /Descent {desc} /CapHeight {asc} /StemV 80 /FontFile2 {file} 0 R >>"
    )
}

/// An embedded TrueType font as a Type0/Identity-H CID font with `/W` and a `/ToUnicode` CMap
/// covering `chars`. Returns the font object and the parsed font (to encode text as glyph ids).
fn type0_truetype(pdf: &mut Pdf, bytes: &[u8], name: &str, chars: &str) -> Result<(u32, Ttf)> {
    let ttf = Ttf::parse(bytes).with_context(|| name.to_string())?;
    let file = pdf.stream(&format!("/Length1 {}", bytes.len()), bytes);
    let fd = pdf.add(descriptor(&ttf, name, 4, file));
    let mut used = std::collections::BTreeMap::new();
    for ch in chars.chars() {
        let gid = ttf.glyph(ch).with_context(|| format!("{name}: no glyph for {ch:?}"))?;
        used.insert(gid, ch);
    }
    let w: Vec<String> = used.keys().map(|g| format!("{g} [{}]", ttf.advance(*g))).collect();
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let pairs: Vec<(&u16, &char)> = used.iter().collect();
    for chunk in pairs.chunks(100) {
        let _ = writeln!(cmap, "{} beginbfchar", chunk.len());
        for (g, ch) in chunk {
            let mut utf16 = [0u16; 2];
            let hex: String = ch.encode_utf16(&mut utf16).iter().map(|u| format!("{u:04X}")).collect();
            let _ = writeln!(cmap, "<{g:04X}> <{hex}>");
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    let tounicode = pdf.flate("", cmap.as_bytes());
    let cid = pdf.add(format!(
        "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{name} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {fd} 0 R /W [{}] /CIDToGIDMap /Identity >>",
        w.join(" ")
    ));
    let font = pdf.add(format!(
        "<< /Type /Font /Subtype /Type0 /BaseFont /{name} /Encoding /Identity-H /DescendantFonts [{cid} 0 R] /ToUnicode {tounicode} 0 R >>"
    ));
    Ok((font, ttf))
}

fn gid_hex(ttf: &Ttf, s: &str) -> Result<String> {
    let mut out = String::from("<");
    for ch in s.chars() {
        let Some(g) = ttf.glyph(ch) else { bail!("no glyph for {ch:?}") };
        let _ = write!(out, "{g:04X}");
    }
    out.push('>');
    Ok(out)
}

fn embedded_fonts(fonts: &Path) -> Result<Fixture> {
    let read = |name: &str| std::fs::read(fonts.join(name)).with_context(|| format!("reading assets/fonts/{name}"));
    let mut d = Doc::new();
    let inter = simple_truetype(&mut d.pdf, &read("Inter-Regular.ttf")?, "Inter-Regular")?;
    let semi = simple_truetype(&mut d.pdf, &read("Inter-SemiBold.ttf")?, "Inter-SemiBold")?;
    let script = simple_truetype(&mut d.pdf, &read("DancingScript.ttf")?, "DancingScript")?;
    let mono_lines = ["fn main() { println!(\"JetBrains Mono\"); }", "Identity-H CID font with ToUnicode 0123456789", "let café = 42; // déjà"];
    let (mono, mono_ttf) = type0_truetype(&mut d.pdf, &read("JetBrainsMono-Regular.ttf")?, "JetBrainsMono-Regular", &mono_lines.concat())?;
    let res = format!("/Font << /F1 {inter} 0 R /F2 {semi} 0 R /F3 {script} 0 R /F4 {mono} 0 R >>");
    let mut c = Content::default();
    c.text("F2", 18.0, 24.0, 262.0, "Inter semibold heading weight")
        .text("F1", 14.0, 24.0, 238.0, "Inter regular embedded TrueType text")
        .text("F1", 10.0, 24.0, 220.0, "Embedded fonts keep their exact shapes, even accents like café")
        .text("F3", 24.0, 24.0, 186.0, "Dancing Script flourish")
        .text("F3", 15.0, 24.0, 162.0, "Handwriting style signature line");
    for (i, line) in mono_lines.iter().enumerate() {
        let _ = writeln!(c.ops, "BT /F4 10 Tf 24 {} Td {} Tj ET", 128 - 16 * i as i32, gid_hex(&mono_ttf, line)?);
        c.truth(line);
    }
    c.text("F1", 8.0, 24.0, 40.0, "Eight point Inter body copy checks small embedded glyph rendering quality");
    d.page(MB, &res, &c);
    Ok(d.finish(
        "text-embedded-fonts",
        "Embedded OFL TrueType: simple WinAnsi (Inter, Dancing Script) and Type0 Identity-H with ToUnicode (JetBrains Mono)",
    ))
}

fn text_operators() -> Fixture {
    let mut d = Doc::new();
    let res = format!("/Font << {} {} >>", std_font("F1", "Helvetica"), std_font("F2", "Helvetica-Bold"));
    let mut c = Content::default();
    let q = |c: &mut Content, ops: &str, truth: &str| {
        c.op(&format!("q {ops} Q"));
        c.truth(truth);
    };
    q(&mut c, "BT /F1 12 Tf 2 Tc 24 370 Td (Character spacing widens letters) Tj ET", "Character spacing widens letters");
    q(&mut c, "BT /F1 12 Tf 6 Tw 24 350 Td (Word spacing widens gaps between words) Tj ET", "Word spacing widens gaps between words");
    q(&mut c, "BT /F1 12 Tf 140 Tz 24 330 Td (Horizontal scaling stretches) Tj ET", "Horizontal scaling stretches");
    q(&mut c, "BT /F1 12 Tf 24 310 Td (Baseline ) Tj 5 Ts (rises ) Tj -4 Ts (falls ) Tj 0 Ts (back) Tj ET", "Baseline rises falls back");
    q(&mut c, "BT /F1 12 Tf 24 290 Td [(Ke) 30 (rn) 20 (ing) -1200 (adjusts) -1200 (spacing)] TJ ET", "Kerning adjusts spacing");
    q(&mut c, "1 0 0 RG 0.6 w BT /F2 18 Tf 1 Tr 24 262 Td (Outlined stroke text) Tj ET", "Outlined stroke text");
    q(&mut c, "1 0.85 0.2 rg 0.1 0.1 0.5 RG 0.8 w BT /F2 18 Tf 2 Tr 24 236 Td (Filled and stroked) Tj ET", "Filled and stroked");
    q(&mut c, "BT /F1 12 Tf 3 Tr 24 214 Td (Invisible searchable layer) Tj ET", "Invisible searchable layer");
    let stripes: String =
        (0..8).map(|i| format!("{} {} {} rg {} 160 18 40 re f ", f64::from(i % 3) / 2.0, f64::from(i % 2), 0.5, 24 + 18 * i)).collect();
    q(&mut c, &format!("BT /F2 32 Tf 7 Tr 24 168 Td (CLIPPED) Tj ET {stripes}"), "CLIPPED");
    q(&mut c, "BT /F1 12 Tf 0.866 0.5 -0.5 0.866 220 120 Tm (Rotated thirty degrees) Tj ET", "Rotated thirty degrees");
    q(
        &mut c,
        "BT /F1 10 Tf 12 TL 24 130 Td (Leading line one) Tj T* (line two by T star) Tj (line three by quote) ' 1 0.5 (line four by double quote) \" ET",
        "Leading line one\nline two by T star\nline three by quote\nline four by double quote",
    );
    q(&mut c, "BT /F1 12 Tf -1 0 0 1 380 40 Tm (Mirrored) Tj ET", "Mirrored");
    q(&mut c, "BT /F1 12 Tf 0 1 -1 0 380 140 Tm (Vertical) Tj ET", "Vertical");
    d.page("/MediaBox [0 0 400 400]", &res, &c);
    d.finish("text-operators", "Tc, Tw, Tz, Ts, TJ kerning, render modes 1/2/3/7, rotated, mirrored, T* ' \" operators")
}

fn type3_fonts() -> Fixture {
    let mut d = Doc::new();
    // Glyph shapes in a 1000-unit grid: A = triangle, B = two bars, C = open box. Uncolored (d1).
    let glyphs = [
        ("A", "700 0 0 0 700 800 d1 0 0 m 350 800 l 700 0 l h 175 0 m 350 400 l 525 0 l h f"),
        ("B", "700 0 0 0 700 800 d1 0 0 600 350 re f 0 450 600 350 re f"),
        ("C", "700 0 0 0 700 800 d1 0 0 m 650 0 l 650 150 l 150 150 l 150 650 l 650 650 l 650 800 l 0 800 l h f"),
    ];
    let mut procs = String::new();
    for (name, ops) in glyphs {
        let id = d.pdf.stream("", ops.as_bytes());
        let _ = write!(procs, "/{name} {id} 0 R ");
    }
    let tounicode = d.pdf.flate(
        "",
        b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /T3 def /CMapType 2 def 1 begincodespacerange <00> <FF> endcodespacerange 3 beginbfchar <41> <0041> <42> <0042> <43> <0043> endbfchar endcmap CMapName currentdict /CMap defineresource pop end end",
    );
    let base = format!(
        "/Type /Font /Subtype /Type3 /FontBBox [0 0 700 800] /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << {procs}>> /Encoding << /Type /Encoding /Differences [65 /A /B /C] >> /FirstChar 65 /LastChar 67 /Widths [760 760 760] /Resources << >>"
    );
    let with_map = d.pdf.add(format!("<< {base} /ToUnicode {tounicode} 0 R >>"));
    let by_names = d.pdf.add(format!("<< {base} >>"));
    // A colored (d0) glyph font: glyph colors itself red regardless of the text fill.
    let red = d.pdf.stream("", b"760 0 d0 1 0 0 rg 0 0 700 800 re f 1 1 1 rg 200 200 300 400 re f");
    let colored = d.pdf.add(format!(
        "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 700 800] /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /A {red} 0 R >> /Encoding << /Type /Encoding /Differences [65 /A] >> /FirstChar 65 /LastChar 65 /Widths [760] /Resources << >> >>"
    ));
    let res = format!("/Font << /T1 {with_map} 0 R /T2 {by_names} 0 R /T3 {colored} 0 R {} >>", std_font("F1", "Helvetica"));
    let mut c = Content::default();
    c.text("F1", 11.0, 24.0, 270.0, "Type 3 fonts drawn from content streams");
    c.op("0 0 0.7 rg BT /T1 36 Tf 24 200 Td (ABC CAB) Tj ET").truth("ABC CAB");
    c.op("0 0.5 0 rg BT /T2 24 Tf 24 140 Td (BAC ACB) Tj ET").truth("BAC ACB");
    c.op("0 0 1 rg BT /T3 30 Tf 24 80 Td (AAA) Tj ET").truth("AAA");
    c.op("0.4 0 0.4 rg BT /T1 48 Tf 0.9 0.3 -0.3 0.9 240 120 Tm (CAB) Tj ET").truth("CAB");
    d.page(MB, &res, &c);
    d.finish("type3-fonts", "Type 3 fonts: uncolored d1 glyphs with and without ToUnicode, a colored d0 glyph, a rotated run")
}

fn vector_art() -> Fixture {
    let mut d = Doc::new();
    let mut c = Content::default();
    c.op("0 0 0 RG");
    for (i, w) in [0.0, 0.25, 0.5, 1.0, 2.0, 4.0].iter().enumerate() {
        let y = 284.0 - 9.0 * i as f64;
        c.op(&format!("q {w} w 20 {y} m 180 {y} l S Q"));
    }
    c.op("q 8 w 0.2 0.2 0.6 RG 0 J 220 284 m 300 284 l S 1 J 220 266 m 300 266 l S 2 J 220 248 m 300 248 l S Q");
    c.op("q 0 0 0 RG 0.5 w 220 290 m 220 240 l S 300 290 m 300 240 l S Q");
    c.op("q 2 w [6 3] 0 d 20 222 m 180 222 l S [2 2 8 2] 4 d 20 212 m 180 212 l S [12] 0 d 1 J 20 202 m 180 202 l S Q");
    c.op("0.9 0.2 0.2 rg 20 120 60 50 re f");
    c.op("q 0.2 0.6 0.9 rg 0.1 0.1 0.4 RG 2 w 90 120 60 50 re B Q");
    c.op(&format!("0.1 0.6 0.2 rg {} f", circle(190.0, 145.0, 25.0)));
    c.op(&format!("0.5 0.2 0.7 rg {} f*", star(260.0, 145.0, 32.0)));
    c.op(&format!("0.5 0.2 0.7 rg {} f", star(340.0, 145.0, 32.0)));
    let stripes: String = (0..10).map(|i| format!("{} g {} 20 8 80 re f ", if i % 2 == 0 { 0.2 } else { 0.7 }, 20 + 8 * i)).collect();
    c.op(&format!("q {} W n {stripes} Q", circle(60.0, 60.0, 38.0)));
    c.op("q 1.5 w 0.5 0 0.5 RG 120 30 m 160 100 220 0 260 70 c S Q");
    for (j, x) in [(0, 280.0), (1, 320.0), (2, 360.0)] {
        c.op(&format!("q 7 w {j} j 0.6 0.3 0 RG {x} 205 m {} 240 l {} 205 l S Q", x + 12.0, x + 24.0));
    }
    c.op("q 6 w 0 j 1.5 M 0.3 0.3 0.3 RG 290 30 m 320 90 l 350 30 l S Q");
    d.page(MB, "", &c);

    // Form XObjects: transforms, nesting and BBox clipping.
    let x1 = d.pdf.stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 50 50]",
        format!("0.2 0.5 0.9 rg 0 0 50 50 re f 1 0.8 0 rg {} f 0 G 2 w 0 0 m 50 50 l S", circle(25.0, 25.0, 15.0)).as_bytes(),
    );
    // Nested: draws X1 through a BBox that keeps only its left half.
    let x2 = d.pdf.stream(&format!("/Type /XObject /Subtype /Form /BBox [0 0 25 50] /Resources << /XObject << /X1 {x1} 0 R >> >>"), b"/X1 Do");
    let res = format!("/XObject << /X1 {x1} 0 R /X2 {x2} 0 R >> {HELV}");
    let mut c = Content::default();
    c.text("F1", 11.0, 20.0, 280.0, "Form XObject transforms");
    c.op("q 0 w 0.6 G");
    for i in 0..9 {
        let y = 20.0 + 25.0 * f64::from(i);
        c.op(&format!("200 {y} m 390 {y} l S"));
    }
    c.op("Q");
    c.op("q 1 0 0 1 20 200 cm /X1 Do Q");
    c.op("q 1.5 0 0 1.5 90 190 cm /X1 Do Q");
    c.op("q 0.7071 0.7071 -0.7071 0.7071 230 195 cm /X1 Do Q");
    c.op("q 1 0 0.5 1 280 200 cm /X1 Do Q");
    c.op("q 1 0 0 1 20 100 cm /X2 Do Q");
    c.op("q 1 0 0 -1 100 150 cm /X1 Do Q");
    c.op("q 3 0 0 3 200 20 cm /X2 Do Q");
    d.page(MB, &res, &c);
    d.finish(
        "vector-art",
        "Line widths (incl. hairline), caps, joins, miter limit, dashes, fills, even-odd, clipping, curves; nested Form XObjects with BBox clips",
    )
}

fn shadings_patterns() -> Fixture {
    let mut d = Doc::new();
    let lin = |c0: &str, c1: &str| format!("<< /FunctionType 2 /Domain [0 1] /C0 [{c0}] /C1 [{c1}] /N 1 >>");
    let axial = format!("<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [20 0 180 0] /Function {} /Extend [true true] >>", lin("1 0 0", "0 0 1"));
    let radial = format!(
        "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [290 340 5 290 340 45] /Function {} /Extend [false true] >>",
        lin("1 1 0", "0 0.5 0")
    );
    let stitched = format!(
        "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [20 0 180 0] /Function << /FunctionType 3 /Domain [0 1] /Functions [{} {}] /Bounds [0.5] /Encode [0 1 0 1] >> >>",
        lin("1 1 1", "1 0.5 0"),
        lin("1 0.5 0", "0.2 0 0.4")
    );
    let vertical =
        format!("<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 200 0 280] /Function {} /Extend [true true] >>", lin("0 0.6 0.6", "1 1 0.6"));
    let pat_sh = d.pdf.add(format!("<< /PatternType 2 /Shading {vertical} >>"));
    // Gouraud triangle mesh (type 4): flag, x, y (16-bit over 0..400 × 0..400), r, g, b.
    let mut mesh = Vec::new();
    for (x, y, rgb) in [(220.0, 110.0, [255u8, 0, 0]), (380.0, 110.0, [0, 255, 0]), (300.0, 190.0, [0, 0, 255])] {
        mesh.push(0u8);
        for v in [x, y] {
            mesh.extend_from_slice(&((v / 400.0 * 65535.0) as u16).to_be_bytes());
        }
        mesh.extend_from_slice(&rgb);
    }
    let gouraud = d.pdf.stream(
        "/ShadingType 4 /ColorSpace /DeviceRGB /BitsPerCoordinate 16 /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 400 0 400 0 1 0 1 0 1]",
        &mesh,
    );
    let tile = d.pdf.stream(
        "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 20 20] /XStep 20 /YStep 20 /Resources << >>",
        b"1 0 0 rg 0 0 10 10 re f 0 0 1 rg 10 10 10 10 re f",
    );
    let hatch = d.pdf.stream(
        "/Type /Pattern /PatternType 1 /PaintType 2 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 /Resources << >>",
        b"1.5 w 0 0 m 10 10 l S 0 10 m 10 0 l S",
    );
    let res = format!(
        "/Shading << /Sh1 {axial} /Sh2 {radial} /Sh3 {stitched} /Sh4 {gouraud} 0 R >> /Pattern << /P1 {pat_sh} 0 R /P2 {tile} 0 R /P3 {hatch} 0 R >> /ColorSpace << /CSp [/Pattern /DeviceRGB] >> {HELV}"
    );
    let mut c = Content::default();
    c.text("F1", 10.0, 20.0, 386.0, "Shadings and patterns");
    c.op("q 20 300 160 70 re W n /Sh1 sh Q");
    c.op("q 220 300 160 80 re W n /Sh2 sh Q");
    c.op("q 20 210 160 70 re W n /Sh3 sh Q");
    c.op(&format!("q /Pattern cs /P1 scn {} f Q", star(300.0, 242.0, 40.0)));
    c.op("q /Sh4 sh Q");
    c.op("q 20 110 160 80 re W n /Pattern cs /P2 scn 20 110 160 80 re f Q");
    c.op("q /CSp cs 0.1 0.6 0.2 /P3 scn 20 20 160 70 re f Q");
    c.op(&format!("q /CSp cs 0.8 0.1 0.3 /P3 scn {} f Q", circle(300.0, 55.0, 35.0)));
    d.page("/MediaBox [0 0 400 400]", &res, &c);
    d.finish(
        "shadings-patterns",
        "Axial, radial, stitched (type 3 function) and Gouraud mesh shadings; shading pattern; colored and uncolored tiling patterns",
    )
}

fn color_spaces() -> Fixture {
    let mut d = Doc::new();
    let tint = d.pdf.stream("/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1]", b"{ 0 0 }");
    let res = format!(
        "/ColorSpace << /Sep [/Separation /Spot /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.6 1 0] /N 1 >>] \
         /Lab [/Lab << /WhitePoint [0.9505 1 1.089] /Range [-100 100 -100 100] >>] \
         /Cal [/CalRGB << /WhitePoint [0.9505 1 1.089] /Gamma [2.2 2.2 2.2] /Matrix [0.4124 0.2126 0.0193 0.3576 0.7152 0.1192 0.1805 0.0722 0.9505] >>] \
         /CalG [/CalGray << /WhitePoint [0.9505 1 1.089] /Gamma 2.2 >>] \
         /Idx [/Indexed /DeviceRGB 3 <FF000000FF000000FFFFFF00>] \
         /DN [/DeviceN [/Cyan /Magenta] /DeviceCMYK {tint} 0 R] >> {HELV}"
    );
    let rows: [[(&str, &str); 5]; 5] = [
        [("0 g", "Gray 0"), ("0.25 g", "Gray 25"), ("0.5 g", "Gray 50"), ("0.75 g", "Gray 75"), ("0.9 g", "Gray 90")],
        [("1 0 0 rg", "RGB red"), ("0 1 0 rg", "RGB green"), ("0 0 1 rg", "RGB blue"), ("0.2 0.4 0.6 rg", "RGB slate"), ("1 0.5 0 rg", "RGB orange")],
        [
            ("1 0 0 0 k", "CMYK cyan"),
            ("0 1 0 0 k", "CMYK magenta"),
            ("0 0 1 0 k", "CMYK yellow"),
            ("0 0 0 1 k", "CMYK black"),
            ("0.2 0.4 0.6 0.1 k", "CMYK mix"),
        ],
        [
            ("/Sep cs 0.25 sc", "Spot 25"),
            ("/Sep cs 0.5 sc", "Spot 50"),
            ("/Sep cs 1 sc", "Spot 100"),
            ("/Lab cs 50 60 40 sc", "Lab red"),
            ("/Lab cs 70 -40 50 sc", "Lab green"),
        ],
        [
            ("/Cal cs 0.8 0.3 0.1 sc", "CalRGB rust"),
            ("/CalG cs 0.5 sc", "CalGray mid"),
            ("/Idx cs 1 sc", "Indexed green"),
            ("/Idx cs 3 sc", "Indexed yellow"),
            ("/DN cs 0.7 0.3 sc", "DeviceN two"),
        ],
    ];
    let mut c = Content::default();
    c.text("F1", 10.0, 20.0, 286.0, "Colour spaces");
    for (r, row) in rows.iter().enumerate() {
        let y = 244.0 - 55.0 * r as f64;
        let mut labels = Vec::new();
        for (i, (op, label)) in row.iter().enumerate() {
            let x = 20.0 + 75.0 * i as f64;
            c.op(&format!("q {op} {x} {y} 60 34 re f Q"));
            let _ = writeln!(c.ops, "BT /F1 7 Tf {x} {} Td ({label}) Tj ET", y - 9.0);
            labels.push(*label);
        }
        c.truth(&labels.join(" "));
    }
    c.op("q 0 0 0 1 K 3 w 1 0 0 0 k 330 12 50 20 re B Q");
    d.page(MB, &res, &c);
    d.finish("color-spaces", "Gray, RGB, CMYK, Separation, Lab, CalRGB, CalGray, Indexed and DeviceN (type 4 function) fills")
}

fn images() -> Fixture {
    let mut d = Doc::new();
    let mut grad = Vec::with_capacity(64 * 64 * 3);
    for y in 0..64u32 {
        for x in 0..64u32 {
            grad.extend_from_slice(&[(x * 4) as u8, (y * 4) as u8, 128]);
        }
    }
    let im1 = d.pdf.flate("/Type /XObject /Subtype /Image /Width 64 /Height 64 /ColorSpace /DeviceRGB /BitsPerComponent 8", &grad);
    let checker: Vec<u8> = (0..64u32).map(|i| if (i % 8 + i / 8) % 2 == 0 { 30 } else { 220 }).collect();
    let im2 =
        d.pdf.stream("/Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray /BitsPerComponent 8 /Interpolate false", &checker);
    // 16×16 stencil: a ring.
    let mut mask = Vec::with_capacity(32);
    for y in 0..16i32 {
        let mut row = 0u16;
        for x in 0..16i32 {
            let r2 = (x * 2 - 15).pow(2) + (y * 2 - 15).pow(2);
            let paint = (60..=200).contains(&r2);
            if !paint {
                row |= 1 << (15 - x);
            }
        }
        mask.extend_from_slice(&row.to_be_bytes());
    }
    let im3 = d.pdf.stream("/Type /XObject /Subtype /Image /Width 16 /Height 16 /ImageMask true /BitsPerComponent 1", &mask);
    let alpha: Vec<u8> = (0..32 * 32)
        .map(|i| {
            let (x, y) = (f64::from(i % 32) - 15.5, f64::from(i / 32) - 15.5);
            (255.0 * (1.0 - (x * x + y * y).sqrt() / 16.0).clamp(0.0, 1.0)) as u8
        })
        .collect();
    let smask = d.pdf.flate("/Type /XObject /Subtype /Image /Width 32 /Height 32 /ColorSpace /DeviceGray /BitsPerComponent 8", &alpha);
    let orange: Vec<u8> = [230u8, 120, 20].repeat(32 * 32);
    let im4 = d.pdf.flate(
        &format!("/Type /XObject /Subtype /Image /Width 32 /Height 32 /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask {smask} 0 R"),
        &orange,
    );
    // Indexed, 2 bits per sample: four quadrants.
    let mut idx = Vec::new();
    for y in 0..16u32 {
        for xb in 0..4u32 {
            let mut b = 0u8;
            for k in 0..4u32 {
                let x = xb * 4 + k;
                let v = (u8::from(x >= 8)) | (u8::from(y >= 8) << 1);
                b |= v << (6 - 2 * k);
            }
            idx.push(b);
        }
    }
    let im5 = d.pdf.stream(
        "/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace [/Indexed /DeviceRGB 3 <2040C0E0E0E0C02020208020>] /BitsPerComponent 2",
        &idx,
    );
    let ramp: Vec<u8> = (0..32 * 32).map(|i| ((i % 32) * 8) as u8).collect();
    let im6 = d.pdf.stream("/Type /XObject /Subtype /Image /Width 32 /Height 32 /ColorSpace /DeviceGray /BitsPerComponent 8 /Decode [1 0]", &ramp);
    let mut cmyk = Vec::new();
    for y in 0..16u32 {
        for x in 0..16u32 {
            cmyk.extend_from_slice(&[(x * 16) as u8, (y * 16) as u8, 0, if (x + y) % 5 == 0 { 200 } else { 0 }]);
        }
    }
    let im7 = d.pdf.stream("/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceCMYK /BitsPerComponent 8", &cmyk);
    let bits: Vec<u8> = (0..16u32).flat_map(|y| if y % 4 < 2 { [0xF0, 0x0F] } else { [0x0F, 0xF0] }).collect();
    let im8 = d.pdf.stream("/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceGray /BitsPerComponent 1", &bits);
    let res = format!(
        "/XObject << /Im1 {im1} 0 R /Im2 {im2} 0 R /Im3 {im3} 0 R /Im4 {im4} 0 R /Im5 {im5} 0 R /Im6 {im6} 0 R /Im7 {im7} 0 R /Im8 {im8} 0 R >> {HELV}"
    );
    let mut c = Content::default();
    c.text("F1", 10.0, 20.0, 314.0, "Images and resampling");
    c.op("q 96 0 0 96 20 190 cm /Im1 Do Q");
    c.op("q 72 0 0 72 130 200 cm /Im2 Do Q");
    c.op("q 0 0 0.8 rg 64 0 0 64 220 205 cm /Im3 Do Q");
    let stripes: String = (0..8).map(|i| format!("{} g {} 190 10 90 re f ", if i % 2 == 0 { 0.3 } else { 0.9 }, 300 + 10 * i)).collect();
    c.op(&format!("q {stripes} Q"));
    c.op("q 80 0 0 80 300 195 cm /Im4 Do Q");
    c.op("q 64 0 0 64 20 95 cm /Im5 Do Q");
    c.op("q 64 0 0 64 100 95 cm /Im6 Do Q");
    c.op("q 64 0 0 64 180 95 cm /Im7 Do Q");
    c.op("q 64 0 0 64 260 95 cm /Im8 Do Q");
    let hex: String =
        (0..16u32).map(|i| format!("{:02X}{:02X}{:02X}", (i * 16) as u8, 255 - (i * 16) as u8, if i % 2 == 0 { 255 } else { 0 })).collect();
    c.op(&format!("q 48 0 0 48 340 100 cm BI /W 4 /H 4 /CS /RGB /BPC 8 /F /AHx ID\n{hex}>\nEI Q"));
    c.op("q 0.866 0.5 -0.5 0.866 0 0 cm 1 0 0 1 60 -30 cm 60 0 0 60 0 0 cm /Im1 Do Q");
    c.op("q 40 0 0 40 200 15 cm /Im2 Do Q q 1 0 0 1 250 15 cm 120 0 0 40 0 0 cm /Im6 Do Q");
    d.page("/MediaBox [0 0 400 330]", &res, &c);
    d.finish("images", "RGB Flate, gray non-interpolated upscale, stencil mask, soft mask, 2-bit Indexed, Decode inversion, CMYK, 1-bit gray, inline AHx image, rotated image")
}

fn transparency() -> Fixture {
    let mut d = Doc::new();
    let gs = "/GSa << /Type /ExtGState /ca 0.5 >> /GSs << /Type /ExtGState /CA 0.5 >> /GSk << /Type /ExtGState /ca 0.6 >>";
    let two = format!("1 0 0 rg {} f 0 0 1 rg {} f", circle(185.0, 270.0, 30.0), circle(215.0, 270.0, 30.0));
    let knock = d.pdf.stream(
        &format!("/Type /XObject /Subtype /Form /BBox [140 220 260 320] /Group << /S /Transparency /K true /I true >> /Resources << /ExtGState << {gs} >> >>"),
        format!("/GSk gs {two}").as_bytes(),
    );
    let two_r = format!("1 0 0 rg {} f 0 0 1 rg {} f", circle(295.0, 270.0, 30.0), circle(325.0, 270.0, 30.0));
    let plain = d.pdf.stream(
        &format!("/Type /XObject /Subtype /Form /BBox [250 220 370 320] /Group << /S /Transparency /K false /I true >> /Resources << /ExtGState << {gs} >> >>"),
        format!("/GSk gs {two_r}").as_bytes(),
    );
    let group = d.pdf.stream(
        "/Type /XObject /Subtype /Form /BBox [20 10 130 100] /Group << /S /Transparency >>",
        b"1 0 0 rg 20 10 70 70 re f 0 0 1 rg 55 30 70 70 re f",
    );
    let mask_form = d.pdf.stream(
        "/Type /XObject /Subtype /Form /BBox [140 10 260 100] /Group << /S /Transparency /CS /DeviceRGB >> /Resources << /Shading << /S << /ShadingType 2 /ColorSpace /DeviceRGB /Coords [140 0 260 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [1 1 1] /C1 [0 0 0] /N 1 >> >> >> >>",
        b"/S sh",
    );
    let modes = ["Multiply", "Screen", "Overlay", "Darken", "Lighten", "ColorDodge", "Difference", "Luminosity"];
    let bm: String = modes.iter().map(|m| format!("/BM{m} << /Type /ExtGState /BM /{m} >> ")).collect();
    let res = format!(
        "/ExtGState << {gs} {bm} /GSm << /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G {mask_form} 0 R >> >> >> /XObject << /K {knock} 0 R /P {plain} 0 R /G {group} 0 R >> {HELV}"
    );
    let mut c = Content::default();
    let stripes: String = (0..40).map(|i| format!("{} 0 10 330 re ", 20 * i)).collect();
    c.op(&format!("q 0.85 g {stripes} f Q"));
    c.text("F1", 9.0, 20.0, 318.0, "Transparency and blending");
    c.op(&format!(
        "q /GSa gs 1 0 0 rg {} f 0 0.7 0 rg {} f 0 0 1 rg {} f Q",
        circle(60.0, 270.0, 30.0),
        circle(95.0, 270.0, 30.0),
        circle(77.0, 240.0, 30.0)
    ));
    c.op("/K Do /P Do");
    let mut labels = Vec::new();
    for (i, m) in modes.iter().enumerate() {
        let x = 20.0 + 46.0 * i as f64;
        c.op(&format!("q 1 0.6 0.1 rg {x} 140 32 40 re f /BM{m} gs 0.2 0.4 1 rg {} 150 32 40 re f Q", x + 10.0));
        let _ = writeln!(c.ops, "BT /F1 6 Tf {x} 130 Td ({m}) Tj ET");
        labels.push(*m);
    }
    c.truth(&labels.join(" "));
    c.op("q /GSa gs /G Do Q");
    c.op("q /GSm gs 0 0.6 0.3 rg 140 10 120 90 re f Q");
    c.op("q /GSs gs 12 w 0 0.4 0.8 RG 1 J 280 20 m 385 105 l S Q");
    d.page("/MediaBox [0 0 400 330]", &res, &c);
    d.finish("transparency", "Fill/stroke alpha, eight blend modes, knockout vs non-knockout groups, group opacity, luminosity soft mask")
}

fn forms() -> Fixture {
    let mut d = Doc::new();
    let page = d.pdf.reserve();
    let helv = d.pdf.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    let zadb = d.pdf.add("<< /Type /Font /Subtype /Type1 /BaseFont /ZapfDingbats >>");
    let ap = |pdf: &mut Pdf, w: f64, h: f64, ops: &str| {
        pdf.stream(
            &format!("/Type /XObject /Subtype /Form /BBox [0 0 {w} {h}] /Resources << /Font << /Helv {helv} 0 R /ZaDb {zadb} 0 R >> >>"),
            ops.as_bytes(),
        )
    };
    let mut fields = Vec::new();
    let mut widgets = Vec::new();
    // A field merged with its one widget.
    let widget = |pdf: &mut Pdf, fields: &mut Vec<u32>, widgets: &mut Vec<u32>, body: String| {
        let id = pdf.add(format!("<< /Type /Annot /Subtype /Widget /F 4 /P {page} 0 R {body} >>"));
        fields.push(id);
        widgets.push(id);
    };
    let name_ap = ap(
        &mut d.pdf,
        180.0,
        22.0,
        "0.9 0.95 1 rg 0 0 180 22 re f 0 0 0.6 RG 1 w 0.5 0.5 179 21 re S /Tx BMC q 1 1 178 20 re W n BT /Helv 12 Tf 0 g 3 6 Td (Ada Lovelace) Tj ET Q EMC",
    );
    widget(
        &mut d.pdf,
        &mut fields,
        &mut widgets,
        format!(
            "/FT /Tx /T (name) /V (Ada Lovelace) /Rect [110 250 290 272] /DA (/Helv 12 Tf 0 g) /MK << /BG [0.9 0.95 1] /BC [0 0 0.6] >> /AP << /N {name_ap} 0 R >>"
        ),
    );
    widget(
        &mut d.pdf,
        &mut fields,
        &mut widgets,
        "/FT /Tx /T (city) /V (Lisbon) /Rect [110 220 290 242] /DA (/Helv 11 Tf 0 0 0.5 rg) /MK << /BG [1 1 0.85] /BC [0.5 0.5 0] >>".into(),
    );
    let on = ap(&mut d.pdf, 16.0, 16.0, "1 g 0 0 16 16 re f 0 G 1 w 0.5 0.5 15 15 re S 0 g BT /ZaDb 12 Tf 2.5 3.5 Td (4) Tj ET");
    let off = ap(&mut d.pdf, 16.0, 16.0, "1 g 0 0 16 16 re f 0 G 1 w 0.5 0.5 15 15 re S");
    widget(
        &mut d.pdf,
        &mut fields,
        &mut widgets,
        format!("/FT /Btn /T (agree) /V /Yes /AS /Yes /Rect [110 190 126 206] /MK << /CA (4) >> /AP << /N << /Yes {on} 0 R /Off {off} 0 R >> >>"),
    );
    widget(
        &mut d.pdf,
        &mut fields,
        &mut widgets,
        format!("/FT /Btn /T (news) /V /Off /AS /Off /Rect [200 190 216 206] /MK << /CA (4) >> /AP << /N << /Yes {on} 0 R /Off {off} 0 R >> >>"),
    );
    let radio = d.pdf.reserve();
    let mut kids = Vec::new();
    for (i, opt) in ["Small", "Medium", "Large"].iter().enumerate() {
        let dot =
            ap(&mut d.pdf, 14.0, 14.0, &format!("1 g {} f 0 G {} S 0 g {} f", circle(7.0, 7.0, 6.5), circle(7.0, 7.0, 6.5), circle(7.0, 7.0, 3.0)));
        let empty = ap(&mut d.pdf, 14.0, 14.0, &format!("1 g {} f 0 G {} S", circle(7.0, 7.0, 6.5), circle(7.0, 7.0, 6.5)));
        let x = 110.0 + 70.0 * i as f64;
        let state = if *opt == "Medium" { "/Medium" } else { "/Off" };
        let kid = d.pdf.add(format!(
            "<< /Type /Annot /Subtype /Widget /F 4 /P {page} 0 R /Parent {radio} 0 R /AS {state} /Rect [{x} 160 {} 174] /AP << /N << /{opt} {dot} 0 R /Off {empty} 0 R >> >> >>",
            x + 14.0
        ));
        kids.push(kid);
        widgets.push(kid);
    }
    d.pdf.set(radio, format!("<< /FT /Btn /Ff 49152 /T (size) /V /Medium /Kids [{}] >>", refs(&kids)));
    fields.push(radio);
    let combo_ap = ap(&mut d.pdf, 120.0, 20.0, "1 g 0 0 120 20 re f 0 G 0.5 0.5 119 19 re S /Tx BMC BT /Helv 11 Tf 0 g 3 5 Td (Spain) Tj ET EMC");
    widget(
        &mut d.pdf,
        &mut fields,
        &mut widgets,
        format!(
            "/FT /Ch /Ff 131072 /T (country) /Opt [(Portugal) (Spain) (France)] /V (Spain) /Rect [110 128 230 148] /DA (/Helv 11 Tf 0 g) /AP << /N {combo_ap} 0 R >>"
        ),
    );
    widget(
        &mut d.pdf,
        &mut fields,
        &mut widgets,
        "/FT /Ch /T (letter) /Opt [(Alpha) (Beta) (Gamma)] /V (Beta) /I [1] /Rect [250 100 370 148] /DA (/Helv 10 Tf 0 g) /MK << /BC [0 0 0] >>"
            .into(),
    );
    let button_ap = ap(&mut d.pdf, 80.0, 24.0, "0.8 g 0 0 80 24 re f 0.4 G 1 w 0.5 0.5 79 23 re S BT /Helv 12 Tf 0 g 20 7 Td (Submit) Tj ET");
    widget(
        &mut d.pdf,
        &mut fields,
        &mut widgets,
        format!("/FT /Btn /Ff 65536 /T (submit) /Rect [110 20 190 44] /MK << /CA (Submit) /BG [0.8 0.8 0.8] >> /AP << /N {button_ap} 0 R >>"),
    );
    widget(
        &mut d.pdf,
        &mut fields,
        &mut widgets,
        "/FT /Tx /Ff 4096 /T (notes) /V (Line one\\rLine two) /Rect [110 56 290 96] /DA (/Helv 10 Tf 0 g) /MK << /BC [0 0 0] >>".into(),
    );
    widget(
        &mut d.pdf,
        &mut fields,
        &mut widgets,
        "/FT /Tx /Ff 16777216 /MaxLen 6 /T (code) /V (ABC123) /Rect [300 20 390 44] /DA (/Helv 12 Tf 0 g) /MK << /BC [0 0 0] >>".into(),
    );
    d.catalog_extra = format!(
        "/AcroForm << /Fields [{}] /DR << /Font << /Helv {helv} 0 R /ZaDb {zadb} 0 R >> >> /DA (/Helv 0 Tf 0 g) /NeedAppearances false >>",
        refs(&fields)
    );
    let mut c = Content::default();
    c.text("F1", 11.0, 20.0, 284.0, "Form fields");
    for (label, y) in [("Name", 256.0), ("City", 226.0), ("Agree", 194.0), ("Size", 163.0), ("Country", 134.0), ("Notes", 80.0), ("Action", 28.0)] {
        c.text("F1", 10.0, 20.0, y, label);
    }
    d.page_at(page, MB, HELV, &c, &widgets);
    d.finish(
        "forms",
        "AcroForm widgets with appearance streams (text, check box, radio, combo, button) and without (text, multiline, comb, list box)",
    )
}

fn annotations() -> Fixture {
    let mut d = Doc::new();
    let page = d.pdf.reserve();
    let helv_b = d.pdf.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>");
    let square_ap = d.pdf.stream("/Type /XObject /Subtype /Form /BBox [0 0 60 40]", b"1 1 0 rg 1 0 0 RG 2 w 1 1 58 38 re B");
    let stamp_ap = d.pdf.stream(
        &format!("/Type /XObject /Subtype /Form /BBox [0 0 110 34] /Resources << /Font << /HB {helv_b} 0 R >> >>"),
        b"0 0.5 0 RG 3 w 2 2 106 30 re S 0 0.5 0 rg BT /HB 16 Tf 12 11 Td (APPROVED) Tj ET",
    );
    let hidden_ap = d.pdf.stream("/Type /XObject /Subtype /Form /BBox [0 0 40 40]", b"1 0 0 rg 0 0 40 40 re f");
    let mut annots = Vec::new();
    let mut add = |pdf: &mut Pdf, body: &str| annots.push(pdf.add(format!("<< /Type /Annot /P {page} 0 R {body} >>")));
    let pdf = &mut d.pdf;
    add(pdf, &format!("/Subtype /Square /F 4 /Rect [20 20 80 60] /C [1 0 0] /IC [1 1 0] /AP << /N {square_ap} 0 R >>"));
    add(pdf, "/Subtype /Square /F 4 /Rect [100 20 170 60] /C [0 0 1] /IC [0.8 0.9 1] /BS << /W 3 >>");
    add(pdf, "/Subtype /Circle /F 4 /Rect [190 20 250 60] /C [0 0.6 0] /BS << /W 2 >>");
    add(pdf, "/Subtype /Line /F 4 /Rect [260 15 390 65] /L [270 25 380 55] /LE [/None /OpenArrow] /C [0.6 0 0.6] /BS << /W 2 >>");
    add(pdf, "/Subtype /Highlight /F 4 /Rect [24 236 160 252] /QuadPoints [24 252 160 252 24 236 160 236] /C [1 1 0]");
    add(pdf, "/Subtype /Underline /F 4 /Rect [24 212 140 228] /QuadPoints [24 228 140 228 24 212 140 212] /C [0 0 1]");
    add(pdf, "/Subtype /StrikeOut /F 4 /Rect [24 188 150 204] /QuadPoints [24 204 150 204 24 188 150 188] /C [1 0 0]");
    add(pdf, "/Subtype /Text /F 28 /Rect [200 240 220 260] /Name /Comment /C [1 0.9 0.3] /Contents (A sticky note)");
    add(pdf, "/Subtype /FreeText /F 4 /Rect [200 180 380 220] /DA (/Helv 10 Tf 1 0 0 rg) /Contents (Free text note) /C [1 1 0.9]");
    add(pdf, "/Subtype /Ink /F 4 /Rect [20 80 140 160] /InkList [[30 90 50 150 70 100 90 150 110 90 130 140]] /C [0.5 0 0.8] /BS << /W 2 >>");
    add(pdf, "/Subtype /Polygon /F 4 /Rect [150 80 250 160] /Vertices [160 90 240 90 230 150 170 140] /C [0 0.5 0.5] /IC [0.8 1 1] /BS << /W 1.5 >>");
    add(pdf, &format!("/Subtype /Stamp /F 4 /Rect [270 110 380 144] /Name /Approved /AP << /N {stamp_ap} 0 R >>"));
    add(pdf, "/Subtype /Link /F 4 /Rect [270 80 380 100] /Border [0 0 0] /A << /S /URI /URI (https://example.invalid/) >>");
    add(pdf, &format!("/Subtype /Square /F 2 /Rect [340 240 380 280] /C [1 0 0] /AP << /N {hidden_ap} 0 R >>"));
    add(pdf, &format!("/Subtype /Square /F 36 /Rect [290 240 330 280] /C [1 0 0] /AP << /N {hidden_ap} 0 R >>"));
    let mut c = Content::default();
    c.text("F1", 11.0, 24.0, 280.0, "Annotations over page text");
    c.text("F1", 12.0, 26.0, 240.0, "Highlighted words here");
    c.text("F1", 12.0, 26.0, 216.0, "Underlined phrase");
    c.text("F1", 12.0, 26.0, 192.0, "Struck out sentence");
    c.text("F1", 9.0, 272.0, 86.0, "Link without border");
    d.page_at(page, MB, HELV, &c, &annots);
    d.finish("annotations", "Markup annotations with appearance streams (Square, Stamp) and without (Square, Circle, Line, text markup, note, FreeText, Ink, Polygon); hidden/NoView flags")
}

fn rotated_pages() -> Fixture {
    let mut d = Doc::new();
    for (rot, media) in [(0, MB), (90, MB), (180, MB), (270, MB), (-90, "/MediaBox [0 0 300 400]"), (450, "/MediaBox [0 0 300 400]")] {
        let (w, h) = if media == MB { (400.0, 300.0) } else { (300.0, 400.0) };
        let mut c = Content::default();
        c.op(&format!("1 0 0 rg 0 0 30 30 re f 0 0 1 rg {} {} 30 30 re f", w - 30.0, h - 30.0));
        c.op(&format!(
            "q 3 w 0 0.5 0 RG {} 60 m {} {} l S {} {} m {} {} l {} {} l S Q",
            w / 2.0,
            w / 2.0,
            h - 60.0,
            w / 2.0 - 20.0,
            h - 80.0,
            w / 2.0,
            h - 60.0,
            w / 2.0 + 20.0,
            h - 80.0
        ));
        let label = if rot < 0 { "minus ninety".to_string() } else { rot.to_string() };
        c.text("F1", 16.0, 40.0, h / 2.0, &format!("Page rotated {label} degrees"));
        c.text("F1", 10.0, 40.0, h / 2.0 - 20.0, "Arrow points to the top edge");
        d.page(&format!("{media} /Rotate {rot}"), HELV, &c);
    }
    d.finish("rotated-pages", "/Rotate 0, 90, 180, 270, -90 and 450 on landscape and portrait media")
}

fn page_boxes() -> Fixture {
    let mut d = Doc::new();
    d.pages_extra = "/MediaBox [0 0 250 350]".into();
    let grid = |x0: f64, y0: f64, x1: f64, y1: f64| {
        let mut s = String::from("q 0.5 w 0.7 G ");
        let mut x = x0;
        while x <= x1 {
            let _ = write!(s, "{x} {y0} m {x} {y1} l ");
            x += 50.0;
        }
        let mut y = y0;
        while y <= y1 {
            let _ = write!(s, "{x0} {y} m {x1} {y} l ");
            y += 50.0;
        }
        s.push_str("S Q");
        s
    };
    let mut c = Content::default();
    c.op(&grid(0.0, 0.0, 400.0, 300.0)).op("0.9 0.3 0.1 rg 50 40 300 220 re 54 44 292 212 re f*");
    c.text("F1", 14.0, 70.0, 150.0, "Crop box inside media box");
    c.op("BT /F1 12 Tf 5 5 Td (OUTSIDE) Tj ET");
    d.page("/MediaBox [0 0 400 300] /CropBox [50 40 350 260]", HELV, &c);
    let mut c = Content::default();
    c.op(&grid(100.0, 200.0, 500.0, 500.0)).op("0 0.5 0.8 rg 100 200 30 30 re f");
    c.text("F1", 14.0, 140.0, 350.0, "Media box with offset origin");
    d.page("/MediaBox [100 200 500 500]", HELV, &c);
    let mut c = Content::default();
    c.op(&grid(-50.0, -50.0, 350.0, 350.0)).op("0.2 0.6 0.2 rg -40 -40 380 20 re f");
    c.text("F1", 14.0, 30.0, 150.0, "Crop box larger than media");
    d.page("/MediaBox [0 0 300 300] /CropBox [-50 -50 350 350]", HELV, &c);
    let mut c = Content::default();
    c.op(&grid(0.0, 0.0, 400.0, 300.0)).op("0.9 0.3 0.1 rg 60 50 20 20 re f");
    c.text("F1", 14.0, 70.0, 150.0, "Cropped and rotated page");
    d.page("/MediaBox [0 0 400 300] /CropBox [50 40 350 260] /Rotate 90", HELV, &c);
    let mut c = Content::default();
    c.op(&grid(0.0, 0.0, 250.0, 350.0)).op("0.5 0 0.5 rg 0 0 25 25 re f");
    c.text("F1", 14.0, 20.0, 175.0, "Inherited media box");
    d.page("", HELV, &c);
    d.finish("page-boxes", "CropBox inside, larger than and offset from MediaBox, crop plus rotation, inherited MediaBox; text outside the crop box")
}

#[cfg(test)]
/// Words a page shows, for tests: every fixture's truth is non-empty where text is drawn.
pub fn distinct_words(s: &str) -> std::collections::BTreeSet<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn fixtures() -> Vec<Fixture> {
        all(&crate::gates::root().join("assets/fonts")).expect("fixtures build")
    }

    #[test]
    fn every_fixture_is_a_well_formed_pdf_with_the_declared_pages() {
        for f in fixtures() {
            let doc = lopdf::Document::load_mem(&f.bytes).unwrap_or_else(|e| panic!("{}: {e}", f.name));
            assert_eq!(doc.get_pages().len(), f.pages(), "{}", f.name);
            assert!(f.pages() > 0, "{}", f.name);
            assert!(f.truth.iter().any(|t| !distinct_words(t).is_empty()), "{}: no ground-truth text", f.name);
        }
    }

    #[test]
    fn fixture_names_are_unique_and_generation_is_deterministic() {
        let (a, b) = (fixtures(), fixtures());
        let names: BTreeSet<&str> = a.iter().map(|f| f.name).collect();
        assert_eq!(names.len(), a.len());
        for (x, y) in a.iter().zip(&b) {
            assert!(x.bytes == y.bytes, "{} is not deterministic", x.name);
        }
    }

    #[test]
    fn stored_zlib_round_trips_through_a_real_inflater() {
        // Stored blocks keep the plaintext visible, so decode explicitly with lopdf's inflater
        // (and its adler32 check) over more than one 64 KiB block.
        let data: Vec<u8> = (0..150_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut pdf = Pdf::default();
        let id = pdf.flate("", &data);
        let root = pdf.add("<< /Type /Catalog >>");
        let doc = lopdf::Document::load_mem(&pdf.finish(root)).expect("parse");
        let stream = doc.get_object((id, 0)).and_then(lopdf::Object::as_stream).expect("stream");
        assert_eq!(stream.decompressed_content().expect("inflate"), data);
        assert_eq!(zlib_stored(b""), [0x78, 0x01, 1, 0, 0, 0xFF, 0xFF, 0, 0, 0, 1]);
    }

    #[test]
    fn literal_strings_escape_delimiters_and_encode_latin1() {
        assert_eq!(lit("a(b)c\\"), "a\\(b\\)c\\\\");
        assert_eq!(lit("café"), "caf\\351");
        assert_eq!(lit("€"), "?");
    }
}

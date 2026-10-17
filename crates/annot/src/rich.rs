//! Rich text of text boxes (ISO 32000-2 §12.7.4.3): the `/RC` XHTML body and `/DS` default
//! style read into runs of formatted text, edited by character range, written back, and laid
//! out for the appearance stream with the standard fonts' exact metrics.
//!
//! Only what a text box can show is kept: the font family (Helvetica, Times or Courier, the
//! standard fonts every viewer has), size, colour, bold, italic, underline and strikethrough,
//! and the box's alignment. Anything else in the XHTML is read as plain text in the
//! surrounding style.

use crate::{Rgb, n};

/// At most this many characters of rich text are read or laid out.
pub const MAX_RICH_CHARS: usize = 100_000;
/// XHTML elements nested deeper than this are read as plain text of their parent.
const MAX_DEPTH: usize = 64;
/// At most this many runs are kept (adjacent runs with the same format are merged first).
const MAX_SPANS: usize = 10_000;

/// A standard font family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Family {
    #[default]
    Helvetica,
    Times,
    Courier,
}

impl Family {
    pub const ALL: [Family; 3] = [Family::Helvetica, Family::Times, Family::Courier];

    pub fn name(self) -> &'static str {
        match self {
            Family::Helvetica => "Helvetica",
            Family::Times => "Times",
            Family::Courier => "Courier",
        }
    }

    /// A CSS `font-family` list: the first family that names (or is like) a standard one.
    pub fn from_css(list: &str) -> Option<Family> {
        list.split(',').find_map(|f| {
            let f = f.trim().trim_matches(['\'', '"']).to_ascii_lowercase();
            if f.starts_with("times") || f == "serif" || f.contains("roman") || f.contains("minion") || f.contains("georgia") {
                Some(Family::Times)
            } else if f.starts_with("courier") || f == "monospace" || f.contains("mono") {
                Some(Family::Courier)
            } else if f.starts_with("helvetica") || f.starts_with("arial") || f == "sans-serif" || f.contains("myriad") || f.contains("sans") {
                Some(Family::Helvetica)
            } else {
                None
            }
        })
    }

    /// The standard font's `/BaseFont` with this weight and slant.
    pub fn base_font(self, bold: bool, italic: bool) -> &'static str {
        match (self, bold, italic) {
            (Family::Helvetica, false, false) => "Helvetica",
            (Family::Helvetica, true, false) => "Helvetica-Bold",
            (Family::Helvetica, false, true) => "Helvetica-Oblique",
            (Family::Helvetica, true, true) => "Helvetica-BoldOblique",
            (Family::Times, false, false) => "Times-Roman",
            (Family::Times, true, false) => "Times-Bold",
            (Family::Times, false, true) => "Times-Italic",
            (Family::Times, true, true) => "Times-BoldItalic",
            (Family::Courier, false, false) => "Courier",
            (Family::Courier, true, false) => "Courier-Bold",
            (Family::Courier, false, true) => "Courier-Oblique",
            (Family::Courier, true, true) => "Courier-BoldOblique",
        }
    }

    /// The appearance's font resource name for this weight and slant (`Helv` for plain
    /// Helvetica, as PdfCraft's plain text boxes have always used).
    pub(crate) fn resource(self, bold: bool, italic: bool) -> &'static str {
        match (self, bold, italic) {
            (Family::Helvetica, false, false) => "Helv",
            (Family::Helvetica, true, false) => "HeBo",
            (Family::Helvetica, false, true) => "HeOb",
            (Family::Helvetica, true, true) => "HeBO",
            (Family::Times, false, false) => "TiRo",
            (Family::Times, true, false) => "TiBo",
            (Family::Times, false, true) => "TiIt",
            (Family::Times, true, true) => "TiBI",
            (Family::Courier, false, false) => "Cour",
            (Family::Courier, true, false) => "CoBo",
            (Family::Courier, false, true) => "CoOb",
            (Family::Courier, true, true) => "CoBO",
        }
    }
}

/// How a run of text looks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextFormat {
    pub family: Family,
    /// Points, 1–400.
    pub size: f64,
    pub color: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
}

impl Default for TextFormat {
    fn default() -> Self {
        Self { family: Family::Helvetica, size: 12.0, color: [0.0; 3], bold: false, italic: false, underline: false, strike: false }
    }
}

impl TextFormat {
    pub fn base_font(&self) -> &'static str {
        self.family.base_font(self.bold, self.italic)
    }

    /// The advance of `c` in this format, in points.
    pub fn char_width(&self, c: char) -> f64 {
        let c = if c == '\t' { ' ' } else { c };
        let em = pdfcraft_fonts::standard_char_width(self.base_font(), c)
            .or_else(|| pdfcraft_fonts::standard_char_width(self.base_font(), '?'))
            .unwrap_or(0.556);
        em * self.size
    }

    /// The CSS declarations for this format (`font-size:…;color:…;…`), with `align` first when
    /// given.
    fn css(&self, align: Option<Align>) -> String {
        let mut s = String::new();
        s.push_str(&format!("font-size:{}pt;", n(self.size)));
        if let Some(a) = align {
            s.push_str(&format!("text-align:{};", a.css()));
        }
        s.push_str(&format!(
            "color:{};font-weight:{};font-style:{};font-family:{};font-stretch:normal",
            hex(self.color),
            if self.bold { "bold" } else { "normal" },
            if self.italic { "italic" } else { "normal" },
            self.family.name()
        ));
        let deco = match (self.underline, self.strike) {
            (true, true) => Some("underline line-through"),
            (true, false) => Some("underline"),
            (false, true) => Some("line-through"),
            (false, false) => None,
        };
        if let Some(d) = deco {
            s.push_str(&format!(";text-decoration:{d}"));
        }
        s
    }

    /// Apply one CSS declaration.
    fn apply(&mut self, prop: &str, value: &str, align: &mut Option<Align>) {
        let value = value.trim();
        match prop.trim().to_ascii_lowercase().as_str() {
            "font-size" => {
                if let Some(s) = css_size(value, self.size) {
                    self.size = s;
                }
            }
            "font-weight" => {
                let v = value.to_ascii_lowercase();
                self.bold = match v.as_str() {
                    "bold" | "bolder" => true,
                    "normal" | "lighter" => false,
                    _ => v.parse::<u32>().map(|w| w >= 600).unwrap_or(self.bold),
                };
            }
            "font-style" => self.italic = matches!(value.to_ascii_lowercase().as_str(), "italic" | "oblique"),
            "font-family" => {
                if let Some(f) = Family::from_css(value) {
                    self.family = f;
                }
            }
            "color" => {
                if let Some(c) = css_color(value) {
                    self.color = c;
                }
            }
            "text-decoration" | "text-decoration-line" => {
                let v = value.to_ascii_lowercase();
                if v.contains("none") {
                    self.underline = false;
                    self.strike = false;
                }
                if v.contains("underline") {
                    self.underline = true;
                }
                if v.contains("line-through") {
                    self.strike = true;
                }
            }
            "text-align" => *align = Align::from_css(value).or(*align),
            // `font: [style] [weight] size[/line-height] family` (and `/DS`'s `font: Helvetica 12pt`).
            "font" => {
                for tok in value.split_whitespace() {
                    let t = tok.split('/').next().unwrap_or(tok);
                    match t.to_ascii_lowercase().as_str() {
                        "bold" => self.bold = true,
                        "italic" | "oblique" => self.italic = true,
                        "normal" => {}
                        _ => {
                            if let Some(s) = css_size(t, self.size) {
                                self.size = s;
                            } else if let Some(f) = Family::from_css(t) {
                                self.family = f;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Apply a `style` attribute (or `/DS`): `;`-separated declarations.
    pub(crate) fn apply_style(&mut self, style: &str, align: &mut Option<Align>) {
        for decl in style.split(';') {
            if let Some((p, v)) = decl.split_once(':') {
                self.apply(p, v, align);
            }
        }
    }
}

/// Alignment of a text box's lines (`/Q`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

impl Align {
    pub fn from_q(q: i64) -> Align {
        match q {
            1 => Align::Center,
            2 => Align::Right,
            _ => Align::Left,
        }
    }

    pub fn q(self) -> i64 {
        match self {
            Align::Left => 0,
            Align::Center => 1,
            Align::Right => 2,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
        }
    }

    pub fn parse(s: &str) -> Option<Align> {
        Align::from_css(s)
    }

    fn css(self) -> &'static str {
        self.name()
    }

    fn from_css(s: &str) -> Option<Align> {
        match s.trim().to_ascii_lowercase().as_str() {
            "left" | "start" | "justify" => Some(Align::Left),
            "center" | "centre" => Some(Align::Center),
            "right" | "end" => Some(Align::Right),
            _ => None,
        }
    }
}

/// A run of text in one format. Line breaks are `\n` inside the text.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub format: TextFormat,
}

/// A text box's text: runs of formatted text and the box's alignment.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct RichText {
    pub spans: Vec<Span>,
    pub align: Align,
}

/// A change to the format of a range: each `Some` field is set, `None` ones are kept.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct FormatChange {
    pub family: Option<Family>,
    pub size: Option<f64>,
    pub color: Option<Rgb>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
}

impl FormatChange {
    pub fn is_empty(&self) -> bool {
        *self == FormatChange::default()
    }

    pub fn apply(&self, f: &mut TextFormat) {
        if let Some(v) = self.family {
            f.family = v;
        }
        if let Some(v) = self.size.filter(|s| s.is_finite() && *s > 0.0) {
            f.size = v.clamp(1.0, 400.0);
        }
        if let Some(v) = self.color.filter(|c| c.iter().all(|x| x.is_finite())) {
            f.color = v.map(|x| x.clamp(0.0, 1.0));
        }
        if let Some(v) = self.bold {
            f.bold = v;
        }
        if let Some(v) = self.italic {
            f.italic = v;
        }
        if let Some(v) = self.underline {
            f.underline = v;
        }
        if let Some(v) = self.strike {
            f.strike = v;
        }
    }
}

impl RichText {
    /// `text` in one format.
    pub fn plain(text: &str, format: TextFormat, align: Align) -> RichText {
        let mut r = RichText { spans: vec![Span { text: text.to_string(), format }], align };
        r.normalize();
        r
    }

    /// The text without formatting (`/Contents`).
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }

    pub fn char_count(&self) -> usize {
        self.spans.iter().map(|s| s.text.chars().count()).sum()
    }

    /// The format of the first character (what `/DA` and `/DS` describe), or `fallback`.
    pub fn base(&self, fallback: TextFormat) -> TextFormat {
        self.spans.first().map(|s| s.format).unwrap_or(fallback)
    }

    /// The format at character `at` (the character before it, so typing continues a run; the
    /// first character's format at the start).
    pub fn format_at(&self, at: usize) -> Option<TextFormat> {
        let chars = self.chars();
        chars.get(at.saturating_sub(1)).or_else(|| chars.first()).map(|(_, f)| *f)
    }

    /// Every run has one format: true when all characters share it.
    pub fn is_uniform(&self) -> bool {
        self.spans.windows(2).all(|w| w.first().map(|a| a.format) == w.get(1).map(|b| b.format))
    }

    fn chars(&self) -> Vec<(char, TextFormat)> {
        self.spans.iter().flat_map(|s| s.text.chars().map(move |c| (c, s.format))).take(MAX_RICH_CHARS).collect()
    }

    fn from_chars(chars: &[(char, TextFormat)], align: Align, fallback: TextFormat) -> RichText {
        let mut spans: Vec<Span> = Vec::new();
        for (c, f) in chars {
            match spans.last_mut() {
                Some(last) if last.format == *f => last.text.push(*c),
                _ => spans.push(Span { text: c.to_string(), format: *f }),
            }
        }
        if spans.is_empty() {
            spans.push(Span { text: String::new(), format: fallback });
        }
        RichText { spans, align }
    }

    /// Merge neighbouring runs that look the same and drop empty ones (keeping one run, so an
    /// empty box still has a format).
    pub fn normalize(&mut self) {
        let fallback = self.base(TextFormat::default());
        let chars = self.chars();
        *self = RichText::from_chars(&chars, self.align, fallback);
        self.spans.truncate(MAX_SPANS);
    }

    /// The same formatting over new text: the unchanged start and end keep their formats, and
    /// what was typed in between takes the format of the character before it.
    pub fn retext(&self, text: &str) -> RichText {
        let old = self.chars();
        let new: Vec<char> = text.chars().take(MAX_RICH_CHARS).collect();
        let prefix = old.iter().zip(&new).take_while(|((a, _), b)| a == *b).count();
        let max_suffix = old.len().min(new.len()) - prefix;
        let suffix = old.iter().rev().zip(new.iter().rev()).take(max_suffix).take_while(|((a, _), b)| a == *b).count();
        let fallback = self.base(TextFormat::default());
        let typed = self.format_at(prefix).unwrap_or(fallback);
        let mut chars: Vec<(char, TextFormat)> = Vec::with_capacity(new.len());
        chars.extend(old.iter().take(prefix).copied());
        chars.extend(new.iter().skip(prefix).take(new.len() - prefix - suffix).map(|c| (*c, typed)));
        chars.extend(old.iter().skip(old.len() - suffix).copied());
        RichText::from_chars(&chars, self.align, fallback)
    }

    /// Change the format of characters `start..end` (clamped; the whole text when the range is
    /// empty).
    pub fn format_range(&self, start: usize, end: usize, change: &FormatChange) -> RichText {
        let mut chars = self.chars();
        let fallback = self.base(TextFormat::default());
        let (start, end) = (start.min(end), start.max(end));
        let (start, end) = if start == end || chars.is_empty() { (0, chars.len()) } else { (start.min(chars.len()), end.min(chars.len())) };
        for (_, f) in chars.iter_mut().take(end).skip(start) {
            change.apply(f);
        }
        let mut out = RichText::from_chars(&chars, self.align, fallback);
        if chars.is_empty()
            && let Some(s) = out.spans.first_mut()
        {
            change.apply(&mut s.format);
        }
        out
    }

    /// The `/RC` XHTML for this text.
    pub fn to_rc(&self) -> String {
        let base = self.base(TextFormat::default());
        let mut body = String::new();
        let mut para = String::new();
        let mut paras: Vec<String> = Vec::new();
        for s in &self.spans {
            let mut parts = s.text.split('\n').peekable();
            while let Some(part) = parts.next() {
                let part = part.trim_end_matches('\r');
                if !part.is_empty() {
                    if s.format == base {
                        para.push_str(&esc(part));
                    } else {
                        para.push_str(&format!("<span style=\"{}\">{}</span>", s.format.css(None), esc(part)));
                    }
                }
                if parts.peek().is_some() {
                    paras.push(std::mem::take(&mut para));
                }
            }
        }
        paras.push(para);
        for p in paras {
            body.push_str(&format!("<p dir=\"ltr\">{p}</p>"));
        }
        format!(
            "<?xml version=\"1.0\"?><body xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:xfa=\"http://www.xfa.org/schema/xfa-data/1.0/\" xfa:spec=\"2.0.2\" style=\"{}\">{body}</body>",
            base.css(Some(self.align))
        )
    }

    /// The `/DS` default style string.
    pub fn to_ds(&self) -> String {
        let base = self.base(TextFormat::default());
        let mut s = format!("font: {} {}pt; text-align:{}; color:{}", base.family.name(), n(base.size), self.align.css(), hex(base.color));
        if base.bold {
            s.push_str("; font-weight:bold");
        }
        if base.italic {
            s.push_str("; font-style:italic");
        }
        s
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn hex(c: Rgb) -> String {
    let b = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", b(c[0]), b(c[1]), b(c[2]))
}

/// A CSS length as points (`pt`, `px` as points, as Acrobat writes them, `em` and `%` of
/// `current`).
fn css_size(v: &str, current: f64) -> Option<f64> {
    let v = v.trim().to_ascii_lowercase();
    let (num, scale) = if let Some(x) = v.strip_suffix("pt").or_else(|| v.strip_suffix("px")) {
        (x, 1.0)
    } else if let Some(x) = v.strip_suffix("em") {
        (x, current)
    } else if let Some(x) = v.strip_suffix('%') {
        (x, current / 100.0)
    } else {
        (v.as_str(), 1.0)
    };
    let s = num.trim().parse::<f64>().ok()? * scale;
    (s.is_finite() && s > 0.0).then(|| s.clamp(1.0, 400.0))
}

/// A CSS colour: `#rgb`, `#rrggbb`, `rgb(r, g, b)` or a few names.
fn css_color(v: &str) -> Option<Rgb> {
    let v = v.trim().to_ascii_lowercase();
    if let Some(h) = v.strip_prefix('#') {
        let digits: Vec<u32> = h.chars().map(|c| c.to_digit(16)).collect::<Option<_>>()?;
        let c = match digits.as_slice() {
            [r, g, b] => [r * 17, g * 17, b * 17],
            [r1, r2, g1, g2, b1, b2] => [r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2],
            _ => return None,
        };
        return Some(c.map(|x| f64::from(x) / 255.0));
    }
    if let Some(inner) = v.strip_prefix("rgb(").and_then(|x| x.strip_suffix(')')) {
        let parts: Vec<f64> = inner
            .split(',')
            .map(|p| {
                let p = p.trim();
                match p.strip_suffix('%') {
                    Some(x) => x.trim().parse::<f64>().ok().map(|x| x / 100.0),
                    None => p.parse::<f64>().ok().map(|x| x / 255.0),
                }
            })
            .collect::<Option<_>>()?;
        let [r, g, b] = parts.as_slice() else { return None };
        return Some([*r, *g, *b].map(|x| if x.is_finite() { x.clamp(0.0, 1.0) } else { 0.0 }));
    }
    Some(match v.as_str() {
        "black" => [0.0; 3],
        "white" => [1.0; 3],
        "red" => [1.0, 0.0, 0.0],
        "green" => [0.0, 0.5, 0.0],
        "blue" => [0.0, 0.0, 1.0],
        "gray" | "grey" => [0.5; 3],
        _ => return None,
    })
}

/// Read `/RC` with `base` (from `/DS` and `/DA`) as the default format. `None` when it isn't
/// well-formed XHTML.
pub fn parse_rc(rc: &str, base: TextFormat, align: Align) -> Option<RichText> {
    // XHTML from other tools may use HTML entities XML doesn't define.
    let rc = rc.replace("&nbsp;", "&#160;");
    let start = rc.find("<body").or_else(|| rc.find('<'))?;
    let xml = rc.get(start..)?;
    let doc = roxmltree::Document::parse(xml).ok()?;
    let mut st = Reader { chars: Vec::new(), align: None, started: false };
    st.element(doc.root_element(), base, 0);
    Some(RichText::from_chars(&st.chars, st.align.unwrap_or(align), base))
}

struct Reader {
    chars: Vec<(char, TextFormat)>,
    align: Option<Align>,
    /// A paragraph has been read, so the next one starts a new line.
    started: bool,
}

impl Reader {
    fn push(&mut self, c: char, f: TextFormat) {
        if self.chars.len() < MAX_RICH_CHARS {
            self.chars.push((c, f));
        }
    }

    fn element(&mut self, node: roxmltree::Node, inherited: TextFormat, depth: usize) {
        let mut f = inherited;
        let tag = node.tag_name().name().to_ascii_lowercase();
        match tag.as_str() {
            "b" | "strong" => f.bold = true,
            "i" | "em" => f.italic = true,
            "u" => f.underline = true,
            "s" | "strike" | "del" => f.strike = true,
            _ => {}
        }
        if let Some(style) = node.attribute("style") {
            // Paragraph alignment: the first paragraph's (or the body's) is the box's.
            let mut align = None;
            f.apply_style(style, &mut align);
            if self.align.is_none() {
                self.align = align;
            }
        }
        let block = matches!(tag.as_str(), "p" | "div");
        if block {
            if self.started {
                // The break ends the paragraph before, in its format.
                let end = self.chars.last().map(|(_, f)| *f).unwrap_or(f);
                self.push('\n', end);
            }
            self.started = true;
        }
        if tag == "br" {
            self.push('\n', f);
            return;
        }
        for child in node.children() {
            if child.is_element() {
                if depth < MAX_DEPTH {
                    self.element(child, f, depth + 1);
                }
            } else if let Some(t) = child.text() {
                // Line breaks in the markup are layout, not text; paragraphs and `<br/>` break lines.
                if !block
                    && !matches!(tag.as_str(), "span" | "b" | "strong" | "i" | "em" | "u" | "s" | "strike" | "del" | "font" | "a" | "sup" | "sub")
                    && t.trim().is_empty()
                {
                    continue;
                }
                for c in t.chars() {
                    let c = if c == '\r' || c == '\n' { ' ' } else { c };
                    self.push(c, f);
                }
            }
        }
    }
}

/// A line of laid-out text: its characters, where its width ends, and how tall it is.
pub(crate) struct Line {
    pub chars: Vec<(char, TextFormat)>,
    pub width: f64,
    /// The largest font size on the line (the base size on an empty line).
    pub size: f64,
}

fn width(chars: &[(char, TextFormat)]) -> f64 {
    chars.iter().map(|(c, f)| f.char_width(*c)).sum()
}

/// Greedy line breaking within `max` points, as [`pdfcraft_fonts::wrap`] does for plain text
/// (paragraphs split on newlines; words longer than a line are broken by character).
pub(crate) fn layout(text: &RichText, max: f64) -> Vec<Line> {
    let base = text.base(TextFormat::default());
    let chars = text.chars();
    let mut lines = Vec::new();
    let finish = |chars: Vec<(char, TextFormat)>, para_size: f64, lines: &mut Vec<Line>| {
        let size = chars.iter().map(|(_, f)| f.size).fold(0.0, f64::max);
        let size = if size > 0.0 { size } else { para_size };
        lines.push(Line { width: width(&chars), chars, size });
    };
    for para in chars.split(|(c, _)| *c == '\n' || *c == '\r') {
        let para_size = para.first().map(|(_, f)| f.size).unwrap_or(base.size);
        let mut line: Vec<(char, TextFormat)> = Vec::new();
        let words = para.split(|(c, _)| *c == ' ');
        // The space before each word (its format), found by position.
        let mut pos = 0usize;
        for word in words {
            let space = if pos == 0 { None } else { para.get(pos - 1).copied() };
            pos += word.len() + 1;
            let mut candidate = line.clone();
            if !line.is_empty()
                && let Some(s) = space
            {
                candidate.push(s);
            }
            candidate.extend_from_slice(word);
            if width(&candidate) <= max || line.is_empty() && width(word) <= max {
                line = candidate;
                continue;
            }
            if !line.is_empty() {
                finish(std::mem::take(&mut line), para_size, &mut lines);
            }
            for ch in word {
                if !line.is_empty() && width(&line) + ch.1.char_width(ch.0) > max {
                    finish(std::mem::take(&mut line), para_size, &mut lines);
                }
                line.push(*ch);
            }
        }
        finish(line, para_size, &mut lines);
    }
    lines
}

/// The natural width of the widest paragraph, in points.
pub(crate) fn widest_paragraph(text: &RichText) -> f64 {
    text.chars().split(|(c, _)| *c == '\n').map(width).fold(0.0, f64::max)
}

/// `/Contents` as rich text compares: line breaks as `\n`, no-break spaces as spaces, no
/// trailing white space.
fn comparable(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n").replace('\u{a0}', " ").trim_end().to_string()
}

/// A text box's rich text: `/RC` when it agrees with `/Contents` (else `/Contents`, which every
/// edit keeps current), in the default style of `/DA`, `/DS` and `/Q`.
pub fn of_dict(d: &pdfcraft_cos::Dict) -> RichText {
    use pdfcraft_cos::{Object, PdfString};
    let text = |key: &[u8]| d.get(key).and_then(|o| o.as_string()).map(PdfString::to_text);
    let (color, size) = crate::appearance::parse_da(d);
    let mut base = TextFormat { size: size.clamp(1.0, 400.0), color, ..TextFormat::default() };
    let mut align = Some(Align::from_q(d.get(b"Q").and_then(Object::as_int).unwrap_or(0)));
    let q = align;
    if let Some(ds) = text(b"DS") {
        base.apply_style(&ds, &mut align);
        // `/Q` is the box's alignment; `/DS` only describes it.
        align = q;
    }
    let align = align.unwrap_or_default();
    let contents: String = text(b"Contents").unwrap_or_default().chars().take(MAX_RICH_CHARS).collect();
    if let Some(rc) = text(b"RC")
        && let Some(mut rich) = parse_rc(&rc, base, align)
        && comparable(&rich.text()) == comparable(&contents)
    {
        rich.align = align;
        // Keep `/Contents`' own characters (line breaks, spaces), with the runs' formats.
        let mut rich = rich.retext(&contents);
        rich.normalize();
        return rich;
    }
    RichText::plain(&contents, base, align)
}

/// Write `text` into a text box's `/Contents`, `/DA`, `/Q`, `/DS` and `/RC`.
pub(crate) fn write(d: &mut pdfcraft_cos::Dict, text: &RichText) {
    use pdfcraft_cos::{Object, PdfString};
    let base = text.base(TextFormat::default());
    let [r, g, b] = base.color.map(|x| x.clamp(0.0, 1.0));
    d.set(b"Contents".to_vec(), PdfString::text(&text.text()));
    d.set(b"DA".to_vec(), PdfString::literal(format!("{} {} {} rg /Helv {} Tf", n(r), n(g), n(b), n(base.size)).into_bytes()));
    d.set(b"Q".to_vec(), Object::Int(text.align.q()));
    d.set(b"DS".to_vec(), PdfString::text(&text.to_ds()));
    d.set(b"RC".to_vec(), PdfString::text(&text.to_rc()));
}

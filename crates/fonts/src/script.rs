//! Typed signatures: a name in the bundled script font (Dancing Script, OFL-1.1) as glyph
//! outlines, flattened to polygons. Drawn as filled paths, so no font is embedded in the PDF.

use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, MetadataProvider};

static FONT: &[u8] = include_bytes!("../../../assets/fonts/DancingScript.ttf");

/// Outlines of a line of text at a font size of 1 (em units): y up, the baseline at 0, starting
/// at x = 0. Fill them with the even-odd (or nonzero) rule.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScriptOutline {
    pub contours: Vec<Vec<[f64; 2]>>,
    pub width: f64,
    pub ascent: f64,
    pub descent: f64,
}

struct Flatten {
    contours: Vec<Vec<[f64; 2]>>,
    cur: Vec<[f64; 2]>,
    scale: f64,
    dx: f64,
}

impl Flatten {
    fn pt(&self, x: f32, y: f32) -> [f64; 2] {
        [self.dx + x as f64 * self.scale, y as f64 * self.scale]
    }
    fn last(&self) -> [f64; 2] {
        self.cur.last().copied().unwrap_or([self.dx, 0.0])
    }
}

const STEPS: usize = 8;

impl OutlinePen for Flatten {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.cur.len() > 2 {
            self.contours.push(std::mem::take(&mut self.cur));
        }
        self.cur.clear();
        let p = self.pt(x, y);
        self.cur.push(p);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.pt(x, y);
        self.cur.push(p);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (p0, c, p) = (self.last(), self.pt(cx, cy), self.pt(x, y));
        for i in 1..=STEPS {
            let t = i as f64 / STEPS as f64;
            let u = 1.0 - t;
            self.cur.push([u * u * p0[0] + 2.0 * u * t * c[0] + t * t * p[0], u * u * p0[1] + 2.0 * u * t * c[1] + t * t * p[1]]);
        }
    }
    fn curve_to(&mut self, c0x: f32, c0y: f32, c1x: f32, c1y: f32, x: f32, y: f32) {
        let (p0, c0, c1, p) = (self.last(), self.pt(c0x, c0y), self.pt(c1x, c1y), self.pt(x, y));
        for i in 1..=STEPS {
            let t = i as f64 / STEPS as f64;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            self.cur.push([a * p0[0] + b * c0[0] + c * c1[0] + d * p[0], a * p0[1] + b * c0[1] + c * c1[1] + d * p[1]]);
        }
    }
    fn close(&mut self) {
        if self.cur.len() > 2 {
            self.contours.push(std::mem::take(&mut self.cur));
        }
        self.cur.clear();
    }
}

/// The outlines of `text` in the script font (characters it lacks are skipped).
pub fn script_outline(text: &str) -> ScriptOutline {
    let Ok(font) = FontRef::new(FONT) else { return ScriptOutline::default() };
    let loc = LocationRef::default();
    let metrics = font.metrics(Size::unscaled(), loc);
    let scale = 1.0 / metrics.units_per_em.max(1) as f64;
    let charmap = font.charmap();
    let glyphs = font.outline_glyphs();
    let advances = font.glyph_metrics(Size::unscaled(), loc);
    let mut pen = Flatten { contours: Vec::new(), cur: Vec::new(), scale, dx: 0.0 };
    for ch in text.chars() {
        let Some(gid) = charmap.map(ch) else { continue };
        if let Some(g) = glyphs.get(gid) {
            let _ = g.draw(DrawSettings::unhinted(Size::unscaled(), loc), &mut pen);
            pen.close();
        }
        pen.dx += advances.advance_width(gid).unwrap_or(0.0) as f64 * scale;
    }
    ScriptOutline { contours: pen.contours, width: pen.dx, ascent: metrics.ascent as f64 * scale, descent: metrics.descent as f64 * scale }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_name_becomes_outlines() {
        let o = super::script_outline("Ada L.");
        assert!(o.contours.len() >= 5, "{}", o.contours.len());
        assert!(o.width > 1.0 && o.width < 6.0, "{}", o.width);
        assert!(o.ascent > 0.5 && o.descent < 0.0);
        let max_x = o.contours.iter().flatten().map(|p| p[0]).fold(0.0, f64::max);
        assert!(max_x <= o.width + 0.2);
        assert!(super::script_outline("").contours.is_empty());
    }
}

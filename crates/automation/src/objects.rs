//! Edit a PDF ▸ selected page objects: list them (`page_objects`), arrange their paint order
//! (`objects_arrange`), align (`objects_align`) and distribute (`objects_distribute`) them, and
//! rotate, flip or move them (`objects_transform`), resize (`objects_resize`) and delete them
//! (`objects_delete`), and change vector paths' fill, stroke, line style and opacity
//! (`objects_style`). Boxes are in points from the top-left of the displayed page (y down), like
//! every other tool; directions are as displayed.

use pdfcraft_engine::{AlignTo, Anchor, Arrange, Axis, Dash, Edit, LineCap, LineJoin, ObjectSize, ObjectsEdit, Paint, PathStyle};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, withheld};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

impl Automation {
    pub(crate) fn page_objects(&self, a: &Args) -> Result<Value> {
        let page = self.page(a)?;
        let doc = self.doc(a)?;
        let copy = doc.allows_copying();
        let info = doc.info.pages.get(page).ok_or_else(|| bad(format!("no page {}", page + 1)))?;
        let r = |x: f32| (x as f64 * 100.0).round() / 100.0;
        let list: Vec<Value> = doc
            .page_objects(page)
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let rect = o.rect.map(|b| {
                    let (u, v) = (info.user_to_view(b[0] as f32, b[1] as f32), info.user_to_view(b[2] as f32, b[3] as f32));
                    json!([r(u[0].min(v[0])), r(u[1].min(v[1])), r(u[0].max(v[0])), r(u[1].max(v[1]))])
                });
                let mut v = json!({ "object": i + 1, "kind": o.kind.label(), "rect": rect, "mcid": o.mcid });
                if !o.text.is_empty() {
                    v["text"] = if copy { json!(o.text) } else { Value::Null };
                }
                v
            })
            .collect();
        Ok(withheld(json!({ "page": page + 1, "count": list.len(), "objects": list }), copy))
    }

    /// `objects_arrange`, `objects_align`, `objects_distribute`, `objects_transform`,
    /// `objects_resize`, `objects_style` and `objects_delete`.
    pub(crate) fn objects_edit(&mut self, tool: &str, a: &Args) -> Result<Value> {
        let page = self.page(a)?;
        let n = self.doc(a)?.page_objects(page).len();
        let numbers = a.ints("objects")?;
        if numbers.is_empty() {
            return Err(bad("objects must list at least one object number (from page_objects)"));
        }
        let mut objects = Vec::with_capacity(numbers.len().min(n));
        for k in numbers {
            if k < 1 || k as u64 > n as u64 {
                return Err(bad(format!("object {k} is out of range: page {} has {n} objects", page + 1)));
            }
            objects.push(k as usize - 1);
        }
        let change = match tool {
            "objects_arrange" => ObjectsEdit::Arrange(match a.str("order")? {
                "front" => Arrange::Front,
                "forward" => Arrange::Forward,
                "backward" => Arrange::Backward,
                "back" => Arrange::Back,
                other => return Err(bad(format!("unknown order {other:?}: use front, forward, backward or back"))),
            }),
            "objects_align" => ObjectsEdit::Align(match a.str("align")? {
                "left" => AlignTo::Left,
                "horizontal_center" => AlignTo::HorizontalCenter,
                "right" => AlignTo::Right,
                "top" => AlignTo::Top,
                "vertical_center" => AlignTo::VerticalCenter,
                "bottom" => AlignTo::Bottom,
                other => return Err(bad(format!("unknown align {other:?}: use left, horizontal_center, right, top, vertical_center or bottom"))),
            }),
            "objects_distribute" => ObjectsEdit::Distribute(match a.str("axis")? {
                "horizontal" => Axis::Horizontal,
                "vertical" => Axis::Vertical,
                other => return Err(bad(format!("unknown axis {other:?}: use horizontal or vertical"))),
            }),
            "objects_resize" => resize(a)?,
            "objects_style" => ObjectsEdit::Style(style(a)?),
            "objects_delete" => ObjectsEdit::Delete,
            _ => match a.str("action")? {
                "rotate" => {
                    let q = a.opt_int("quarters")?.unwrap_or(1);
                    ObjectsEdit::Rotate(q.rem_euclid(4) as i32)
                }
                "flip_horizontal" => ObjectsEdit::Flip { horizontal: true },
                "flip_vertical" => ObjectsEdit::Flip { horizontal: false },
                "move" => {
                    let (dx, dy) = (a.opt_num("dx")?.unwrap_or(0.0), a.opt_num("dy")?.unwrap_or(0.0));
                    if dx.abs() > 1e6 || dy.abs() > 1e6 {
                        return Err(bad("dx and dy must be at most 1,000,000 points"));
                    }
                    // View offsets run down the page; display space runs up.
                    ObjectsEdit::Move { dx, dy: -dy }
                }
                other => return Err(bad(format!("unknown action {other:?}: use rotate, flip_horizontal, flip_vertical or move"))),
            },
        };
        self.apply(a, Edit::EditPageObjects { page, objects, change })
    }
}

/// `objects_resize`'s arguments: a target width/height, or scale factors, and the anchor.
fn resize(a: &Args) -> Result<ObjectsEdit> {
    let (width, height) = (a.opt_num("width")?, a.opt_num("height")?);
    let scale = a.opt_num("scale")?;
    let (sx, sy) = (a.opt_num("scale_x")?, a.opt_num("scale_y")?);
    let size = match (width.is_some() || height.is_some(), scale, sx, sy) {
        (true, None, None, None) => ObjectSize::To { width, height },
        (false, Some(k), None, None) => ObjectSize::Scale { sx: k, sy: k },
        (false, None, x, y) if x.is_some() || y.is_some() => ObjectSize::Scale { sx: x.unwrap_or(1.0), sy: y.unwrap_or(1.0) },
        (false, None, None, None) => return Err(bad("give width and/or height, scale, or scale_x and/or scale_y")),
        _ => return Err(bad("give either width/height, scale, or scale_x/scale_y, not several")),
    };
    let anchor = match a.opt_str("anchor")?.unwrap_or("center") {
        "center" => Anchor::Center,
        "top_left" => Anchor::TopLeft,
        "top" => Anchor::Top,
        "top_right" => Anchor::TopRight,
        "left" => Anchor::Left,
        "right" => Anchor::Right,
        "bottom_left" => Anchor::BottomLeft,
        "bottom" => Anchor::Bottom,
        "bottom_right" => Anchor::BottomRight,
        other => {
            return Err(bad(format!(
                "unknown anchor {other:?}: use center, top_left, top, top_right, left, right, bottom_left, bottom or bottom_right"
            )));
        }
    };
    Ok(ObjectsEdit::Resize { size, anchor })
}

/// Numbers of a JSON array (all finite), at most `max`.
fn numbers(v: &Value, key: &str, max: usize) -> Result<Vec<f64>> {
    let arr = v.as_array().ok_or_else(|| bad(format!("{key} must be an array of numbers")))?;
    if arr.len() > max {
        return Err(bad(format!("{key} has at most {max} numbers")));
    }
    arr.iter().map(|x| x.as_f64().filter(|f| f.is_finite()).ok_or_else(|| bad(format!("{key} must be an array of finite numbers")))).collect()
}

/// A colour argument: "none", "#RRGGBB", [gray], [r, g, b], [c, m, y, k] (0–1), or
/// { space: gray | rgb | cmyk | current, components }.
fn paint(v: &Value, key: &str) -> Result<Paint> {
    let device = |c: Vec<f64>| -> Result<Paint> {
        Ok(match c.as_slice() {
            [g] => Paint::Gray(*g),
            [r, g, b] => Paint::Rgb([*r, *g, *b]),
            [c, m, y, k] => Paint::Cmyk([*c, *m, *y, *k]),
            _ => return Err(bad(format!("{key} needs 1 (gray), 3 (RGB) or 4 (CMYK) components"))),
        })
    };
    match v {
        Value::String(t) if t == "none" => Ok(Paint::None),
        Value::String(t) => {
            let hex = t.strip_prefix('#').filter(|h| h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()));
            let hex = hex.ok_or_else(|| bad(format!("{key} must be \"none\", \"#RRGGBB\", an array of components or {{space, components}}")))?;
            let byte = |i: usize| hex.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()).map_or(0.0, |b| f64::from(b) / 255.0);
            Ok(Paint::Rgb([byte(0), byte(2), byte(4)]))
        }
        Value::Array(_) => device(numbers(v, key, 4)?),
        Value::Object(o) => {
            let c = numbers(o.get("components").ok_or_else(|| bad(format!("{key} needs components")))?, key, 32)?;
            match o.get("space").and_then(Value::as_str).unwrap_or("current") {
                "gray" if c.len() == 1 => device(c),
                "rgb" if c.len() == 3 => device(c),
                "cmyk" if c.len() == 4 => device(c),
                "current" => Ok(Paint::Current(c)),
                "gray" | "rgb" | "cmyk" => Err(bad(format!("{key}: gray needs 1 component, rgb 3 and cmyk 4"))),
                other => Err(bad(format!("unknown {key} space {other:?}: use gray, rgb, cmyk or current"))),
            }
        }
        _ => Err(bad(format!("{key} must be \"none\", \"#RRGGBB\", an array of components or {{space, components}}"))),
    }
}

/// `objects_style`'s arguments.
fn style(a: &Args) -> Result<PathStyle> {
    let fill = a.get("fill").map(|v| paint(v, "fill")).transpose()?;
    let stroke = a.get("stroke").map(|v| paint(v, "stroke")).transpose()?;
    let dash = match a.get("dash") {
        Some(v) => Some(Dash { pattern: numbers(v, "dash", 16)?, phase: a.opt_num("dash_phase")?.unwrap_or(0.0) }),
        None if a.get("dash_phase").is_some() => return Err(bad("dash_phase needs dash")),
        None => None,
    };
    let cap = a
        .opt_str("cap")?
        .map(|c| match c {
            "butt" => Ok(LineCap::Butt),
            "round" => Ok(LineCap::Round),
            "square" => Ok(LineCap::Square),
            other => Err(bad(format!("unknown cap {other:?}: use butt, round or square"))),
        })
        .transpose()?;
    let join = a
        .opt_str("join")?
        .map(|j| match j {
            "miter" => Ok(LineJoin::Miter),
            "round" => Ok(LineJoin::Round),
            "bevel" => Ok(LineJoin::Bevel),
            other => Err(bad(format!("unknown join {other:?}: use miter, round or bevel"))),
        })
        .transpose()?;
    Ok(PathStyle { fill, stroke, line_width: a.opt_num("line_width")?, dash, cap, join, opacity: a.opt_num("opacity")? })
}

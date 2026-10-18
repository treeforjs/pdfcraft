//! Edit a PDF ▸ selected page objects: list them (`page_objects`), arrange their paint order
//! (`objects_arrange`), align (`objects_align`) and distribute (`objects_distribute`) them, and
//! rotate, flip or move them (`objects_transform`). Boxes are in points from the top-left of the
//! displayed page (y down), like every other tool; directions are as displayed.

use pdfcraft_engine::{AlignTo, Arrange, Axis, Edit, ObjectsEdit};
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

    /// `objects_arrange`, `objects_align`, `objects_distribute` and `objects_transform`.
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

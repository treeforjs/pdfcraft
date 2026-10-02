//! Fill & Sign (Acrobat's Fill & Sign tool, execution plan M5.7): type text onto the page, place
//! ✓ ✕ ● ─ marks and today's date, and sign with a drawn signature. Everything is an annotation
//! (typewriter text, PrintCraft-drawn stamps, ink), so it can be moved, deleted and undone like
//! any comment.

use egui::{Color32, CornerRadius, Pos2, Sense, Stroke, pos2, vec2};
use printcraft_engine::{Edit, FillMark, NewAnnotation, Shape, Style};
use printcraft_render::DocInfo;

use crate::canvas::{DocView, PageXform};
use crate::theme::Tokens;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FillTool {
    Text,
    Check,
    Cross,
    Dot,
    Line,
    Date,
    Signature,
}

pub const FILL_TOOLS: [FillTool; 7] =
    [FillTool::Text, FillTool::Cross, FillTool::Check, FillTool::Dot, FillTool::Line, FillTool::Date, FillTool::Signature];

impl FillTool {
    pub fn command(self) -> &'static str {
        match self {
            FillTool::Text => "sign.fill.text",
            FillTool::Check => "sign.fill.check",
            FillTool::Cross => "sign.fill.cross",
            FillTool::Dot => "sign.fill.dot",
            FillTool::Line => "sign.fill.line",
            FillTool::Date => "sign.fill.date",
            FillTool::Signature => "sign.fill.signature",
        }
    }

    pub fn from_command(id: &str) -> Option<Self> {
        FILL_TOOLS.into_iter().find(|t| t.command() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            FillTool::Text => "Add text",
            FillTool::Check => "Checkmark",
            FillTool::Cross => "Cross",
            FillTool::Dot => "Dot",
            FillTool::Line => "Line",
            FillTool::Date => "Date",
            FillTool::Signature => "Sign",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            FillTool::Text => "type",
            FillTool::Check => "check",
            FillTool::Cross => "x",
            FillTool::Dot => "circle-dot",
            FillTool::Line => "minus",
            FillTool::Date => "clock-3",
            FillTool::Signature => "signature",
        }
    }

    fn mark(self) -> Option<FillMark> {
        match self {
            FillTool::Check => Some(FillMark::Check),
            FillTool::Cross => Some(FillMark::Cross),
            FillTool::Dot => Some(FillMark::Dot),
            FillTool::Line => Some(FillMark::Line),
            _ => None,
        }
    }
}

/// Text being typed onto a page: (page, top-left in user space, text, focus requested).
#[derive(Clone, Debug, PartialEq)]
pub struct TypeBox {
    pub page: usize,
    pub at: [f64; 2],
    pub text: String,
    pub focus: bool,
}

/// The size Fill & Sign uses for typed text (Acrobat's default is 10 pt).
pub const TEXT_SIZE: f64 = 10.0;

fn to_user(xf: &PageXform, info: &DocInfo, page: usize, p: Pos2) -> [f64; 2] {
    let (vx, vy) = xf.screen_to_view(p);
    let u = info.pages[page].view_to_user(vx, vy);
    [u[0] as f64, u[1] as f64]
}

fn new(page: usize, shape: Shape, contents: String, author: &str) -> Edit {
    let style = Style::default_for(&shape);
    Edit::AddAnnotation(NewAnnotation { page, shape, style, contents, author: author.to_string() })
}

/// A typewriter annotation sized to its text.
pub fn typed(page: usize, at: [f64; 2], text: &str, author: &str) -> Edit {
    let rect = crate::comments::text_box_rect(at, text, TEXT_SIZE);
    new(page, Shape::Typewriter { rect, font_size: TEXT_SIZE }, text.to_string(), author)
}

/// Place a saved signature (strokes normalised to a 0–1 box, y up) with its left edge at `at`,
/// 150 pt wide.
pub fn signature_at(page: usize, at: [f64; 2], strokes: &[Vec<[f32; 2]>], author: &str) -> Option<Edit> {
    let w = 150.0;
    let (min_y, max_y) = strokes.iter().flatten().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
    if !min_y.is_finite() {
        return None;
    }
    let h = f64::from(max_y - min_y).max(0.05) * w;
    let strokes: Vec<Vec<[f64; 2]>> = strokes
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| s.iter().map(|p| [at[0] + f64::from(p[0]) * w, at[1] - h / 2.0 + f64::from(p[1] - min_y) * w]).collect())
        .collect();
    (!strokes.is_empty()).then(|| new(page, Shape::Signature { strokes }, String::new(), author))
}

/// Clicks with a Fill & Sign tool on one page. Returns `true` when the click was used.
#[allow(clippy::too_many_arguments)]
pub(crate) fn page_input(
    ui: &egui::Ui,
    resp: &egui::Response,
    xf: &PageXform,
    page: usize,
    info: &DocInfo,
    tool: FillTool,
    view: &mut DocView,
    signature: Option<&Vec<Vec<[f32; 2]>>>,
    author: &str,
    today: (i64, u32, u32),
) -> Option<FillAction> {
    let pointer = ui.input(|i| i.pointer.hover_pos())?;
    if !xf.rect.contains(pointer) {
        return None;
    }
    ui.ctx().set_cursor_icon(if tool == FillTool::Text { egui::CursorIcon::Text } else { egui::CursorIcon::Crosshair });
    if !resp.clicked() {
        return None;
    }
    let at = to_user(xf, info, page, pointer);
    match tool {
        FillTool::Text => {
            view.fill_text = Some(TypeBox { page, at: [at[0], at[1] + TEXT_SIZE * 0.6], text: String::new(), focus: true });
            None
        }
        FillTool::Date => {
            let (y, m, d) = today;
            Some(FillAction::Edit(Box::new(typed(page, [at[0], at[1] + TEXT_SIZE * 0.6], &format!("{m}/{d}/{y}"), author))))
        }
        FillTool::Signature => match signature {
            Some(s) => signature_at(page, at, s, author).map(|e| FillAction::Edit(Box::new(e))),
            None => Some(FillAction::CreateSignature),
        },
        mark => {
            let mark = mark.mark().expect("marks");
            let (w, h) = if mark == FillMark::Line { (36.0, 4.0) } else { (12.0, 12.0) };
            let rect = [at[0] - w / 2.0, at[1] - h / 2.0, at[0] + w / 2.0, at[1] + h / 2.0];
            Some(FillAction::Edit(Box::new(new(page, Shape::Mark { rect, mark }, String::new(), author))))
        }
    }
}

/// What a Fill & Sign click asks the app to do.
#[derive(Clone, Debug, PartialEq)]
pub enum FillAction {
    Edit(Box<Edit>),
    /// No signature yet: open the signature pad.
    CreateSignature,
}

/// The in-place editor for typed text. Returns the edit once committed.
pub(crate) fn type_box(ctx: &egui::Context, view: &mut DocView, info: &DocInfo, author: &str) -> Option<Edit> {
    let tb = view.fill_text.clone()?;
    let xf = view.page_xform(tb.page)?;
    let v = info.pages.get(tb.page)?.user_to_view(tb.at[0] as f32, tb.at[1] as f32);
    let pos = xf.norm_to_screen(v[0] / xf.pw, v[1] / xf.ph);
    let zoom = xf.rect.width() / xf.pw.max(1.0);
    let mut commit = false;
    let mut cancel = false;
    egui::Area::new(egui::Id::new(("fill-text", view.id.0))).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        let t = view.fill_text.as_mut().expect("checked");
        let width = ((t.text.len().max(8) as f32) * TEXT_SIZE as f32 * 0.6 * zoom).clamp(60.0, 600.0);
        let r = ui.add(
            egui::TextEdit::singleline(&mut t.text)
                .font(egui::FontId::proportional((TEXT_SIZE as f32 * zoom).max(8.0)))
                .desired_width(width)
                .background_color(Color32::from_rgba_unmultiplied(255, 255, 255, 230))
                .text_color(Color32::BLACK)
                .hint_text("Type text")
                .id_salt("fill-text-edit"),
        );
        if t.focus {
            r.request_focus();
            t.focus = false;
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        } else if r.lost_focus() {
            commit = true;
        }
    });
    if cancel {
        view.fill_text = None;
        return None;
    }
    if commit {
        let tb = view.fill_text.take()?;
        if !tb.text.trim().is_empty() {
            return Some(typed(tb.page, tb.at, tb.text.trim(), author));
        }
    }
    None
}

/// The signature pad: draw with the pointer; returns the strokes (normalised) on Apply.
pub(crate) fn signature_pad(ui: &mut egui::Ui, t: &Tokens, strokes: &mut Vec<Vec<[f32; 2]>>) -> (bool, bool) {
    ui.label(egui::RichText::new("Create signature").font(crate::theme::semibold(18.0)));
    ui.label(egui::RichText::new("Draw your signature below.").color(t.text_muted));
    ui.add_space(6.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(460.0, 150.0), Sense::drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(6), Color32::WHITE);
    painter.rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    painter.hline(rect.x_range().shrink(24.0), rect.bottom() - 34.0, Stroke::new(1.0, t.divider));
    // Normalised: x 0–1 across the pad, y up, in units of the pad's width.
    let norm = |p: Pos2| [(p.x - rect.left()) / rect.width(), (rect.bottom() - p.y) / rect.width()];
    if resp.drag_started() {
        strokes.push(Vec::new());
    }
    if resp.dragged()
        && let (Some(p), Some(s)) = (resp.interact_pointer_pos(), strokes.last_mut())
    {
        let n = norm(p.clamp(rect.min, rect.max));
        if s.last().is_none_or(|l| (l[0] - n[0]).abs() + (l[1] - n[1]).abs() > 0.002) {
            s.push(n);
        }
    }
    for s in strokes.iter() {
        let pts: Vec<Pos2> = s.iter().map(|p| pos2(rect.left() + p[0] * rect.width(), rect.bottom() - p[1] * rect.width())).collect();
        painter.add(egui::Shape::line(pts, Stroke::new(2.0, Color32::BLACK)));
    }
    ui.add_space(10.0);
    let (mut apply, mut cancel) = (false, false);
    ui.horizontal(|ui| {
        if ui.button("Clear").clicked() {
            strokes.clear();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let ready = strokes.iter().any(|s| s.len() > 1);
            if ui.add_enabled_ui(ready, |ui| crate::widgets::pill_button(ui, "Apply", true)).inner.clicked() {
                apply = true;
            }
            if crate::widgets::pill_button(ui, "Cancel", false).clicked() {
                cancel = true;
            }
        });
    });
    (apply, cancel)
}

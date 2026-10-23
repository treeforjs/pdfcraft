//! The Reading Order tool: numbered regions drawn over the visible pages in reading order; drag a
//! box around content and choose what it is (text, a heading, a figure, a table, an artifact …);
//! click a region to select it in the Order and Tags panels.
//!
//! Only the pages on screen are drawn, and each page's reading order is read once per edit
//! ([`crate::tags_ui::OrderCache`]), at most one new page per frame.

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use pdfcraft_engine::Edit;
use pdfcraft_engine::a11y::{ArtifactSelection, ArtifactSubtype, ArtifactType, RegionRole, RegionTag};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, QuickTool, RightPanel, widgets};

/// A box drawn with the tool, waiting for its role.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub doc: pdfcraft_engine::DocId,
    pub page: usize,
    /// In view space (points from the top-left of the displayed page).
    pub rect: [f32; 4],
}

#[derive(Clone, Debug, Default)]
pub struct ReadingState {
    pub pending: Option<Pending>,
    /// Alternate text for a figure or formula drawn next.
    pub alt: String,
    /// A box being dragged: (page, start on screen).
    drag: Option<(usize, Pos2)>,
}

/// What a drawn box becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Role(RegionRole),
    Artifact(ArtifactType, Option<ArtifactSubtype>),
}

/// The roles offered, with their labels.
pub const ROLES: [(&str, Choice); 10] = [
    ("Text", Choice::Role(RegionRole::Text)),
    ("Figure", Choice::Role(RegionRole::Figure)),
    ("Figure with caption", Choice::Role(RegionRole::FigureCaption)),
    ("Table", Choice::Role(RegionRole::Table)),
    ("Table cell", Choice::Role(RegionRole::Cell)),
    ("Formula", Choice::Role(RegionRole::Formula)),
    ("Form field", Choice::Role(RegionRole::FormField)),
    ("Artifact: header", Choice::Artifact(ArtifactType::Pagination, Some(ArtifactSubtype::Header))),
    ("Artifact: footer", Choice::Artifact(ArtifactType::Pagination, Some(ArtifactSubtype::Footer))),
    ("Artifact: background", Choice::Artifact(ArtifactType::Background, None)),
];

/// A region's colour, by what it is.
fn role_color(t: &Tokens, standard: &str) -> Color32 {
    match standard {
        s if s.starts_with('H') && s.len() <= 2 => Color32::from_rgb(0x8E, 0x4E, 0xE6),
        "Figure" | "Formula" => Color32::from_rgb(0x2D, 0x9D, 0x5B),
        "Table" | "TR" | "TH" | "TD" => Color32::from_rgb(0xE8, 0x8A, 0x1A),
        "L" | "LI" => Color32::from_rgb(0x14, 0x9C, 0xA8),
        _ => t.accent,
    }
}

enum Deferred {
    Edit(Box<Edit>),
    Select(usize, usize),
    Clear(usize),
    Done,
}

/// The tag request for a drawn box.
pub fn request(p: &Pending, choice: Choice, alt: &str, info: &pdfcraft_render::DocInfo) -> Option<Edit> {
    let r = p.rect.map(f64::from);
    match choice {
        Choice::Role(role) => {
            let alt = alt.trim();
            let takes_alt = matches!(role, RegionRole::Figure | RegionRole::FigureCaption | RegionRole::Formula);
            Some(Edit::TagRegion {
                request: RegionTag {
                    page: p.page,
                    rect: r,
                    role,
                    alt: (takes_alt && !alt.is_empty()).then(|| alt.to_owned()),
                    artifact_type: None,
                    artifact_subtype: None,
                },
            })
        }
        Choice::Artifact(ty, subtype) => {
            // Change to Artifact takes the box in PDF user space.
            let pi = info.pages.get(p.page)?;
            let a = pi.view_to_user(p.rect[0], p.rect[1]);
            let b = pi.view_to_user(p.rect[2], p.rect[3]);
            let rect = [a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])].map(f64::from);
            Some(Edit::MarkArtifact {
                selection: ArtifactSelection { page: Some(p.page), rect: Some(rect), ty: Some(ty), subtype, ..Default::default() },
            })
        }
    }
}

/// Draw the reading order over the visible pages (and the Tags panel's highlight), and run the
/// tool's gestures. Called after the document area each frame.
pub(crate) fn overlay(app: &mut PdfCraftApp, index: usize, ui: &mut egui::Ui) {
    let tool_on = app.quick_tool == QuickTool::ReadingOrder;
    let order_open = app.right == Some(RightPanel::Order);
    let tags_open = app.right == Some(RightPanel::Tags);
    if !tool_on && !order_open && !tags_open {
        return;
    }
    let unobstructed = app.dialog.is_none() && app.close_request.is_none() && !app.palette_open;
    let Some(view) = app.views.get(index) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    if view.organize {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let clip = view.viewport_rect();
    let pages = view.screen_pages();
    let painter = ui.painter().with_clip_rect(clip);
    let tools = &mut app.a11y_tools;
    let mut deferred: Option<Deferred> = None;

    if tags_open
        && let Some((p, rects)) = &tools.tags.highlight
        && let Some((_, xf)) = pages.iter().find(|(q, _)| q == p)
    {
        for r in rects {
            let sr = xf.view_rect(*r).expand(2.0);
            painter.rect(sr, CornerRadius::same(3), t.accent.gamma_multiply(0.18), Stroke::new(2.0, t.accent), StrokeKind::Outside);
        }
    }

    if tool_on || order_open {
        let mut read_one = false;
        for (p, xf) in &pages {
            let order = match tools.orders.peek(doc, *p) {
                Some(o) => o,
                None if !read_one => {
                    read_one = true;
                    tools.orders.get(doc, *p)
                }
                None => {
                    ui.ctx().request_repaint();
                    continue;
                }
            };
            let Ok(o) = &*order else { continue };
            for (k, r) in o.regions.iter().enumerate() {
                let Some(b) = r.rect else { continue };
                let sr = xf.view_rect(b.map(|x| x as f32));
                if !sr.intersects(clip) {
                    continue;
                }
                let c = role_color(&t, r.standard.as_deref().unwrap_or(&r.ty));
                let sel = tools.region == Some((*p, k));
                painter.rect(
                    sr,
                    CornerRadius::same(2),
                    c.gamma_multiply(if sel { 0.22 } else { 0.08 }),
                    Stroke::new(if sel { 2.0 } else { 1.0 }, c),
                    StrokeKind::Outside,
                );
                let badge =
                    Rect::from_min_size(sr.left_top() - vec2(1.0, 18.0).max(vec2(0.0, 0.0)), vec2(22.0 + 7.0 * r.ty.len().min(8) as f32, 16.0));
                painter.rect_filled(badge, CornerRadius::same(4), c);
                painter.text(
                    badge.left_center() + vec2(5.0, 0.0),
                    Align2::LEFT_CENTER,
                    format!("{} {}", k + 1, r.ty),
                    theme::semibold(10.5),
                    Color32::WHITE,
                );
            }
        }
    }

    if tool_on && unobstructed && doc.allows_modification() {
        let resp = ui.interact(clip, ui.id().with("reading-order-tool"), Sense::click_and_drag());
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let on_page = |p: Pos2| pages.iter().find(|(_, xf)| xf.rect.contains(p)).map(|(i, xf)| (*i, *xf));
        if pointer.and_then(on_page).is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if resp.drag_started()
            && let Some(o) = ui.input(|i| i.pointer.press_origin())
            && let Some((page, _)) = on_page(o)
        {
            tools.reading.drag = Some((page, o));
            tools.reading.pending = None;
        }
        if let Some((page, start)) = tools.reading.drag
            && let Some((_, xf)) = pages.iter().find(|(p, _)| *p == page)
            && let Some(end) = pointer
        {
            let end = pos2(end.x.clamp(xf.rect.left(), xf.rect.right()), end.y.clamp(xf.rect.top(), xf.rect.bottom()));
            let r = Rect::from_two_pos(start, end);
            painter.rect(r, CornerRadius::ZERO, t.accent.gamma_multiply(0.12), Stroke::new(1.5, t.accent), StrokeKind::Middle);
            if resp.drag_stopped() {
                tools.reading.drag = None;
                if r.width() >= 4.0 && r.height() >= 4.0 {
                    let (a, b) = (xf.screen_to_view(r.left_top()), xf.screen_to_view(r.right_bottom()));
                    tools.reading.pending = Some(Pending { doc: doc.id, page, rect: [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)] });
                }
            }
        }
        if resp.clicked()
            && let Some(p) = pointer
            && let Some((page, xf)) = on_page(p)
        {
            tools.reading.pending = None;
            let (vx, vy) = xf.screen_to_view(p);
            if let Some(Ok(o)) = tools.orders.peek(doc, page).as_deref() {
                // The innermost (smallest) region under the pointer.
                let hit = o
                    .regions
                    .iter()
                    .enumerate()
                    .filter_map(|(k, r)| r.rect.map(|b| (k, b)))
                    .filter(|(_, b)| f64::from(vx) >= b[0] && f64::from(vx) <= b[2] && f64::from(vy) >= b[1] && f64::from(vy) <= b[3])
                    .min_by(|a, b| {
                        let area = |r: &[f64; 4]| (r[2] - r[0]) * (r[3] - r[1]);
                        area(&a.1).total_cmp(&area(&b.1))
                    })
                    .map(|(k, _)| k);
                if let Some(k) = hit {
                    deferred = Some(Deferred::Select(page, k));
                } else {
                    tools.region = None;
                }
            }
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            tools.reading.pending = None;
            tools.reading.drag = None;
        }
    }

    if tool_on {
        // The tool's bar, over the top of the document.
        egui::Area::new(ui.id().with("reading-order-bar")).fixed_pos(clip.center_top() + vec2(-230.0, 10.0)).order(egui::Order::Foreground).show(
            ui.ctx(),
            |ui| {
                egui::Frame::new()
                    .fill(t.card)
                    .stroke(Stroke::new(1.0, t.border))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(egui::Margin::symmetric(12, 6))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(tl!("Draw a box around content, then choose what it is.")).font(theme::regular(12.5)));
                            let page = app.views.get(index).map_or(0, |v| v.current);
                            if widgets::pill_button(ui, tl!("Clear page structure"), false).clicked() {
                                deferred = Some(Deferred::Clear(page));
                            }
                            if widgets::pill_button(ui, tl!("Done"), true).clicked() {
                                deferred = Some(Deferred::Done);
                            }
                        });
                    });
            },
        );
    }

    if tool_on
        && let Some(p) = tools.reading.pending.clone()
        && p.doc == doc.id
        && let Some((_, xf)) = pages.iter().find(|(q, _)| *q == p.page)
    {
        let sr = xf.view_rect(p.rect);
        painter.rect(sr, CornerRadius::ZERO, t.accent.gamma_multiply(0.15), Stroke::new(2.0, t.accent), StrokeKind::Middle);
        let at = pos2((sr.right() + 8.0).min(clip.right() - 250.0), sr.top().max(clip.top() + 48.0));
        let reading = &mut tools.reading;
        let info = &doc.info;
        egui::Area::new(ui.id().with("reading-order-roles")).fixed_pos(at).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(t.card)
                .stroke(Stroke::new(1.0, t.border))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(230.0);
                    ui.label(egui::RichText::new(tl!("Tag this area as")).font(theme::semibold(13.0)));
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        for (lvl, label) in
                            [(1u8, "Heading 1"), (2, "Heading 2"), (3, "Heading 3"), (4, "Heading 4"), (5, "Heading 5"), (6, "Heading 6")]
                        {
                            let b = ui.add(egui::Button::new(format!("H{lvl}")).min_size(vec2(32.0, 24.0)));
                            b.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tl!(label)));
                            if b.on_hover_text(tl!(label)).clicked() {
                                deferred =
                                    request(&p, Choice::Role(RegionRole::Heading(lvl)), &reading.alt, info).map(|e| Deferred::Edit(Box::new(e)));
                            }
                        }
                    });
                    ui.add_space(2.0);
                    for (label, choice) in ROLES {
                        if ui.add(egui::Button::new(tl!(label)).min_size(vec2(210.0, 24.0))).clicked() {
                            deferred = request(&p, choice, &reading.alt, info).map(|e| Deferred::Edit(Box::new(e)));
                        }
                    }
                    ui.add_space(4.0);
                    let l = ui.label(egui::RichText::new(tl!("Alternate text (figures and formulas)")).small());
                    ui.add(egui::TextEdit::multiline(&mut reading.alt).desired_rows(2).desired_width(210.0)).labelled_by(l.id);
                    if ui.button(tl!("Cancel")).clicked() {
                        reading.pending = None;
                    }
                });
        });
    }

    match deferred {
        Some(Deferred::Edit(e)) => {
            app.a11y_tools.reading.pending = None;
            if app.apply_edit(*e) {
                app.a11y_tools.reading.alt.clear();
            }
        }
        Some(Deferred::Select(page, k)) => app.order_action(index, page, crate::tags_ui::OrderAction::Select(k)),
        Some(Deferred::Clear(page)) => app.order_action(index, page, crate::tags_ui::OrderAction::Clear(page)),
        Some(Deferred::Done) => {
            app.a11y_tools.reading = ReadingState::default();
            app.quick_tool = QuickTool::Select;
        }
        None => {}
    }
}

impl PdfCraftApp {
    /// Accessibility ▸ Reading order tool: the tool, with the Order panel beside it.
    pub(crate) fn execute_reading_order_tool(&mut self) {
        self.quick_tool = QuickTool::ReadingOrder;
        self.a11y_tools.reading = ReadingState::default();
        self.choose_right_panel(Some(RightPanel::Order));
        self.notify_tr("Draw a box around content, then choose what it is.");
    }
}

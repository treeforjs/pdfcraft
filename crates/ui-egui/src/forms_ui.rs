//! Filling in forms on the page (execution plan M6; Acrobat fills fields in every viewing mode).
//!
//! Text fields open an in-place editor over the widget (Enter or clicking away commits, Escape
//! cancels, Tab moves to the next field). Check boxes and radio buttons toggle on click. Combo
//! and list boxes open a list of their options. Every change is one undoable engine edit.

use egui::{Color32, CornerRadius, Rect, Stroke, vec2};
use printcraft_engine::{Edit, FieldValue, FormField, FormFieldKind, field_flags};
use printcraft_render::DocInfo;

use crate::canvas::{DocView, PageXform};
use crate::theme::Tokens;

const FOCUS_BLUE: Color32 = Color32::from_rgb(0x14, 0x73, 0xE6);

/// The field being edited.
#[derive(Clone, Debug, PartialEq)]
pub struct Focus {
    pub name: String,
    /// Which of the field's widgets (a field can appear on several pages).
    pub widget: usize,
    /// Text being typed (text fields).
    pub text: String,
    /// Choices picked so far (list boxes with multiple selection).
    pub picked: Vec<String>,
    pub request_focus: bool,
    /// Select the whole value when the editor opens (entered with Tab), so typing replaces it.
    pub select_all: bool,
}

#[derive(Clone, Debug, Default)]
pub struct FormView {
    pub focus: Option<Focus>,
    /// A message for the app to show (e.g. "buttons run JavaScript").
    pub notice: Option<String>,
    /// A push button was clicked: (its field name, what it does).
    pub button: Option<(String, printcraft_engine::form_scripts::ButtonAction)>,
}

fn widget_rect(xf: &PageXform, info: &DocInfo, page: usize, r: [f64; 4]) -> Rect {
    xf.user_rect(info, page, [r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32])
}

fn fillable(f: &FormField) -> bool {
    !f.read_only() && !matches!(f.kind, FormFieldKind::PushButton | FormFieldKind::Signature)
}

fn open_focus(f: &FormField, widget: usize) -> Focus {
    Focus {
        name: f.name.clone(),
        widget,
        text: f.value.first().cloned().unwrap_or_default(),
        picked: f.value.clone(),
        request_focus: true,
        select_all: false,
    }
}

/// Clicks on form widgets of one page. Returns `true` when the click belongs to a field.
#[allow(clippy::too_many_arguments)]
pub(crate) fn page_input(
    ui: &egui::Ui,
    resp: &egui::Response,
    xf: &PageXform,
    page: usize,
    info: &DocInfo,
    form: &[FormField],
    allowed: bool,
    view: &mut DocView,
) -> bool {
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let Some(p) = pointer.filter(|p| xf.rect.contains(*p)) else { return false };
    let hit = form.iter().find_map(|f| {
        f.widgets.iter().enumerate().find(|(_, w)| w.page == Some(page) && widget_rect(xf, info, page, w.rect).contains(p)).map(|(i, w)| (f, i, w))
    });
    let Some((f, wi, w)) = hit else { return false };
    let usable = allowed && fillable(f);
    ui.ctx().set_cursor_icon(match (usable, f.kind) {
        (false, _) => egui::CursorIcon::NotAllowed,
        (true, FormFieldKind::Text) => egui::CursorIcon::Text,
        _ => egui::CursorIcon::PointingHand,
    });
    if !resp.clicked() {
        return resp.is_pointer_button_down_on();
    }
    if !allowed {
        view.forms.notice = Some("The document's security settings don't allow filling in form fields".into());
        return true;
    }
    match f.kind {
        _ if f.read_only() => view.forms.notice = Some(format!("{} is read-only", f.name)),
        FormFieldKind::PushButton => match &f.button {
            Some(a) => view.forms.button = Some((f.name.clone(), a.clone())),
            None => view.forms.notice = Some(format!("{} has no action", f.name)),
        },
        FormFieldKind::Signature => view.forms.notice = Some("Signing arrives with digital signatures (M9)".into()),
        FormFieldKind::CheckBox => {
            view.forms.focus = None;
            view.pending_edit = Some(Edit::SetFieldValue { name: f.name.clone(), value: FieldValue::Check(f.value.is_empty()) });
        }
        FormFieldKind::Radio => {
            view.forms.focus = None;
            let on = w.on_state.clone();
            // Clicking the selected button again turns it off unless the group forbids it.
            let value = if f.value.first() == on.as_ref() && !f.has(field_flags::NO_TOGGLE_TO_OFF) { None } else { on };
            if value.as_ref() != f.value.first() {
                view.pending_edit = Some(Edit::SetFieldValue { name: f.name.clone(), value: FieldValue::Radio(value) });
            }
        }
        FormFieldKind::Text | FormFieldKind::Combo | FormFieldKind::List => {
            if view.forms.focus.as_ref().is_none_or(|x| x.name != f.name || x.widget != wi) {
                commit(view, form);
                view.forms.focus = Some(open_focus(f, wi));
            }
        }
    }
    true
}

/// The edit for the focused field, if its draft differs from the field's value.
fn commit(view: &mut DocView, form: &[FormField]) {
    let Some(focus) = view.forms.focus.take() else { return };
    let Some(f) = form.iter().find(|f| f.name == focus.name) else { return };
    let value = match f.kind {
        FormFieldKind::Text if f.value.first().map(String::as_str).unwrap_or("") != focus.text => FieldValue::Text(focus.text),
        FormFieldKind::List if f.has(field_flags::MULTI_SELECT) && focus.picked != f.value => FieldValue::Choice(focus.picked),
        _ => return,
    };
    view.pending_edit = Some(Edit::SetFieldValue { name: f.name.clone(), value });
}

/// Paint focus and hover frames on a page.
pub(crate) fn paint_page(ui: &egui::Ui, painter: &egui::Painter, xf: &PageXform, page: usize, info: &DocInfo, form: &[FormField], view: &DocView) {
    let pointer = ui.input(|i| i.pointer.hover_pos());
    for f in form {
        for (wi, w) in f.widgets.iter().enumerate().filter(|(_, w)| w.page == Some(page)) {
            let r = widget_rect(xf, info, page, w.rect);
            let focused = view.forms.focus.as_ref().is_some_and(|x| x.name == f.name && x.widget == wi);
            if focused {
                painter.rect_stroke(r.expand(1.0), CornerRadius::same(2), Stroke::new(2.0, FOCUS_BLUE), egui::StrokeKind::Outside);
            } else if fillable(f) && pointer.is_some_and(|p| r.contains(p)) {
                painter.rect_stroke(r, CornerRadius::same(1), Stroke::new(1.0, FOCUS_BLUE.gamma_multiply(0.8)), egui::StrokeKind::Outside);
            }
        }
    }
}

/// The in-place editor or option list for the focused field. Returns an edit to apply.
pub(crate) fn overlay(ctx: &egui::Context, view: &mut DocView, info: &DocInfo, form: &[FormField]) -> Option<Edit> {
    let focus = view.forms.focus.clone()?;
    let Some(f) = form.iter().find(|f| f.name == focus.name) else {
        view.forms.focus = None;
        return None;
    };
    let w = f.widgets.get(focus.widget)?;
    let page = w.page?;
    let xf = view.page_xform(page)?;
    let rect = widget_rect(&xf, info, page, w.rect);
    let zoom = xf.rect.width() / xf.pw.max(1.0);
    let t = Tokens::get(ctx);
    let mut edit = None;
    let mut close = false;
    let mut next: Option<bool> = None; // Tab / Shift+Tab
    match f.kind {
        FormFieldKind::Text => {
            let da = f.da.split_whitespace().collect::<Vec<_>>();
            let size =
                da.iter().position(|x| *x == "Tf").and_then(|i| da.get(i.wrapping_sub(1))).and_then(|s| s.parse::<f32>().ok()).filter(|s| *s > 0.0);
            let multiline = f.has(field_flags::MULTILINE);
            let font = (size.unwrap_or(((w.rect[3] - w.rect[1]) as f32 * 0.6).clamp(6.0, 12.0)) * zoom).clamp(6.0, 64.0);
            egui::Area::new(egui::Id::new(("form-editor", view.id.0))).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
                ui.set_min_size(rect.size());
                let fx = view.forms.focus.as_mut().expect("checked");
                let mut te = if multiline { egui::TextEdit::multiline(&mut fx.text) } else { egui::TextEdit::singleline(&mut fx.text) };
                te = te
                    .desired_width(rect.width() - 6.0)
                    .font(egui::FontId::proportional(font))
                    .background_color(Color32::from_rgb(0xFF, 0xFF, 0xF4))
                    .text_color(Color32::BLACK)
                    .margin(egui::Margin::symmetric(3, 1))
                    .password(f.has(field_flags::PASSWORD))
                    .id(egui::Id::new(("form-field", view.id.0, &f.name, focus.widget)));
                if let Some(max) = f.max_len {
                    te = te.char_limit(max);
                }
                if multiline {
                    te = te.desired_rows(((rect.height() / (font * 1.3)).floor() as usize).max(1));
                }
                let r = ui.add_sized(rect.size(), te);
                if fx.request_focus {
                    r.request_focus();
                    fx.request_focus = false;
                    if std::mem::take(&mut fx.select_all)
                        && let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), r.id)
                    {
                        let n = fx.text.chars().count();
                        st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(n))));
                        st.store(ui.ctx(), r.id);
                    }
                }
                let (enter, esc, tab, shift) = ui
                    .input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape), i.key_pressed(egui::Key::Tab), i.modifiers.shift));
                if esc {
                    close = true;
                } else if tab {
                    next = Some(!shift);
                } else if r.lost_focus() && (!multiline || !enter) {
                    // Enter in a single-line field, or clicking elsewhere, commits.
                    commit(view, form);
                }
            });
        }
        FormFieldKind::Combo | FormFieldKind::List => {
            let multi = f.kind == FormFieldKind::List && f.has(field_flags::MULTI_SELECT);
            let resp = egui::Area::new(egui::Id::new(("form-choices", view.id.0)))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.left_bottom() + vec2(0.0, 2.0))
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(rect.width().max(160.0));
                        ui.label(egui::RichText::new(&f.name).small().color(t.text_faint));
                        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                            let fx = view.forms.focus.as_mut().expect("checked");
                            for (export, display) in &f.options {
                                let on = if multi { fx.picked.contains(export) } else { f.value.first() == Some(export) };
                                if multi {
                                    let mut c = on;
                                    if ui.checkbox(&mut c, display).changed() {
                                        if c {
                                            fx.picked.push(export.clone());
                                        } else {
                                            fx.picked.retain(|x| x != export);
                                        }
                                    }
                                } else if ui.selectable_label(on, display).clicked() {
                                    edit = Some(Edit::SetFieldValue { name: f.name.clone(), value: FieldValue::Choice(vec![export.clone()]) });
                                    close = true;
                                }
                            }
                        });
                        if multi && ui.button("Done").clicked() {
                            commit(view, form);
                        }
                    });
                });
            let clicked_away = ctx.input(|i| i.pointer.any_click())
                && !resp.response.contains_pointer()
                && !ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| rect.contains(p));
            let (tab, shift) = ctx.input(|i| (i.key_pressed(egui::Key::Tab), i.modifiers.shift));
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            } else if tab {
                next = Some(!shift);
            } else if clicked_away {
                commit(view, form);
            }
        }
        _ => close = true,
    }
    if close {
        view.forms.focus = None;
    }
    if let Some(forward) = next {
        commit(view, form);
        let pending = view.pending_edit.take();
        // The next widget that takes typing or a choice, in the document's tab order (each
        // page's /Tabs: rows, columns or structure), wrapping.
        let mut order: Vec<(usize, &FormField, usize)> = form
            .iter()
            .filter(|x| fillable(x) && matches!(x.kind, FormFieldKind::Text | FormFieldKind::Combo | FormFieldKind::List))
            .flat_map(|x| x.widgets.iter().enumerate().filter(|(_, w)| w.page.is_some()).map(move |(wi, w)| (w.tab, x, wi)))
            .collect();
        order.sort_by_key(|o| o.0);
        if let Some(pos) = order
            .iter()
            .position(|(_, x, wi)| x.name == f.name && *wi == focus.widget)
            .or_else(|| order.iter().position(|(_, x, _)| x.name == f.name))
        {
            let k = if forward { (pos + 1) % order.len() } else { (pos + order.len() - 1) % order.len() };
            let (_, target, wi) = order[k];
            view.forms.focus = Some(Focus { select_all: true, ..open_focus(target, wi) });
            if let Some(p) = target.widgets[wi].page
                && view.page_screen_rect(p).is_none()
            {
                view.go_to_page(p);
            }
        }
        edit = edit.or(pending);
    }
    edit.or_else(|| view.pending_edit.take())
}

/// Where a field's widget is on screen (tests and automation).
pub fn field_screen_rect(view: &DocView, info: &DocInfo, f: &FormField, widget: usize) -> Option<Rect> {
    let w = f.widgets.get(widget)?;
    let page = w.page?;
    let xf = view.page_xform(page)?;
    Some(widget_rect(&xf, info, page, w.rect))
}

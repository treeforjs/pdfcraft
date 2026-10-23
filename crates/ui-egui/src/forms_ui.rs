//! Filling in forms on the page (execution plan M6; Acrobat fills fields in every viewing mode).
//!
//! Text fields open an in-place editor over the widget (Enter or clicking away commits, Escape
//! cancels, Tab moves to the next field). Check boxes and radio buttons toggle on click. Combo
//! and list boxes open a list of their options. Every change is one undoable engine edit.
//!
//! Field triggers (Field Properties ▸ Actions) fire as in Acrobat: Mouse Enter and Exit as the
//! pointer crosses a widget, Mouse Down and Focus on a press (Blur on the field that had the
//! focus), Mouse Up on the click, and the Keystroke event (`willCommit` false) as text is typed
//! into a field with a Keystroke script or an AF format. Nothing is read or run for a widget
//! whose `/A` and `/AA` have no entry for the trigger ([`pdfcraft_engine::FormField`] widgets
//! carry those bits), so a form without actions costs nothing extra per frame.

use egui::{Color32, CornerRadius, Rect, Stroke, vec2};
use pdfcraft_engine::{Edit, FieldTrigger as Trigger, FieldValue, FormField, FormFieldKind, field_flags};
use pdfcraft_render::DocInfo;

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
    /// Date fields: the month the calendar shows (year, month 1–12), and where it was drawn.
    pub calendar: Option<(i32, u32)>,
    pub calendar_rect: Option<egui::Rect>,
    /// Typing is checked keystroke by keystroke ([`checks_keystrokes`]).
    pub keystrokes: bool,
}

/// At most this many field triggers fire per frame (more are dropped).
pub const MAX_FIRES_PER_FRAME: usize = 16;

/// A field trigger to fire: the field, the trigger, and how many of its chain's first actions
/// the viewer already ran (a push button's first Mouse Up action).
#[derive(Clone, Debug, PartialEq)]
pub struct Fire {
    pub field: String,
    pub trigger: Trigger,
    pub skip: usize,
}

/// A keystroke to check against the field's Keystroke script or format: the text before it,
/// what it inserted over which characters, and the text after it.
#[derive(Clone, Debug, PartialEq)]
pub struct KeystrokeCheck {
    pub field: String,
    pub before: String,
    pub change: String,
    pub sel: std::ops::Range<usize>,
    pub after: String,
}

/// What a keystroke replaced: the inserted text and the replaced char range of `before`.
pub fn keystroke_diff(before: &str, after: &str) -> (String, std::ops::Range<usize>) {
    let b: Vec<char> = before.chars().collect();
    let a: Vec<char> = after.chars().collect();
    let prefix = b.iter().zip(&a).take_while(|(x, y)| x == y).count();
    let suffix = b.iter().rev().zip(a.iter().rev()).take_while(|(x, y)| x == y).count().min(b.len() - prefix).min(a.len() - prefix);
    let change: String = a.get(prefix..a.len() - suffix).map(|c| c.iter().collect()).unwrap_or_default();
    (change, prefix..b.len() - suffix)
}

#[derive(Clone, Debug, Default)]
pub struct FormView {
    pub focus: Option<Focus>,
    /// The draft whose edit is queued (`DocView::pending_edit`): if the field refuses the value,
    /// the editor reopens with it rather than losing the typing.
    pub(crate) committed: Option<Focus>,
    /// A message for the app to show (e.g. "buttons run JavaScript").
    pub notice: Option<FormNotice>,
    /// A push button was clicked: (its field name, what it does).
    pub button: Option<(String, pdfcraft_engine::form_scripts::ButtonAction)>,
    /// The widget under the pointer (field, widget, its trigger bits), for Mouse Enter / Exit.
    pub hover: Option<(String, usize, u8)>,
    /// This frame: the pointer is still over `hover` (`hover_seen`) or over another widget
    /// (`hover_new`); neither means it left.
    pub(crate) hover_seen: bool,
    pub(crate) hover_new: Option<(String, usize, u8)>,
    /// The field that has the focus (for Focus and Blur) and its widget's trigger bits.
    pub active: Option<(String, u8)>,
    /// A press landed on the page off every field (the focused one loses the focus).
    pub(crate) pressed_off: bool,
    /// Triggers to fire, in order (at most [`MAX_FIRES_PER_FRAME`]).
    pub fires: Vec<Fire>,
    /// Text typed into a field with keystroke checks, waiting to be checked.
    pub keystroke: Option<KeystrokeCheck>,
    /// The field the editor was open on last frame (Tab moving it is a focus change).
    pub(crate) editor_was: Option<String>,
}

impl FormView {
    fn fire(&mut self, field: &str, trigger: Trigger, bits: u8, skip: usize) {
        if bits & trigger.bit() != 0 && self.fires.len() < MAX_FIRES_PER_FRAME {
            self.fires.push(Fire { field: field.to_string(), trigger, skip });
        }
    }

    /// Give `field` (whose widget has trigger `bits`) the focus: Blur on the field that had it,
    /// then Focus.
    fn focus_field(&mut self, field: &str, bits: u8) {
        if self.active.as_ref().is_some_and(|(n, _)| n == field) {
            return;
        }
        if let Some((old, old_bits)) = self.active.take() {
            self.fire(&old, Trigger::OnBlur, old_bits, 0);
        }
        self.fire(field, Trigger::OnFocus, bits, 0);
        self.active = Some((field.to_string(), bits));
    }
}

/// Whether typing in `f` is checked keystroke by keystroke (a Keystroke script, or an AF format
/// that refuses characters).
fn checks_keystrokes(f: &FormField) -> bool {
    use pdfcraft_engine::form_scripts::Format;
    f.actions.scripts.keystroke.is_some() || !matches!(f.actions.format, Format::None | Format::Date(_) | Format::Time(_))
}

/// A form message for the app to show, with document data kept separate so the visible
/// text follows the UI language.
#[derive(Clone, Debug)]
pub enum FormNotice {
    /// Filling is blocked by the document's security settings.
    Security,
    /// A read-only field was clicked (its name).
    ReadOnly(String),
    /// A push button with no action was clicked (its name).
    NoAction(String),
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
        calendar: None,
        calendar_rect: None,
        keystrokes: f.kind == FormFieldKind::Text && checks_keystrokes(f),
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
        f.widgets
            .iter()
            .enumerate()
            .find(|(_, w)| w.page == Some(page) && !w.hidden && widget_rect(xf, info, page, w.rect).contains(p))
            .map(|(i, w)| (f, i, w))
    });
    let pressed = ui.input(|i| i.pointer.primary_pressed());
    let Some((f, wi, w)) = hit else {
        view.forms.pressed_off |= pressed && resp.hovered();
        return false;
    };
    // Mouse Enter / Exit: only a change of widget allocates.
    if view.forms.hover.as_ref().is_some_and(|(n, i, _)| *n == f.name && *i == wi) {
        view.forms.hover_seen = true;
    } else if view.forms.hover_new.is_none() {
        view.forms.hover_new = Some((f.name.clone(), wi, w.triggers));
    }
    // Empty signature fields are signed by clicking them (Use a certificate); push buttons run
    // their action.
    let pressable = f.kind == FormFieldKind::PushButton && !f.read_only() && f.button.is_some();
    let usable = allowed && (fillable(f) || pressable || f.kind == FormFieldKind::Signature);
    ui.ctx().set_cursor_icon(match (usable, f.kind) {
        (false, _) => egui::CursorIcon::NotAllowed,
        (true, FormFieldKind::Text) => egui::CursorIcon::Text,
        _ => egui::CursorIcon::PointingHand,
    });
    // Mouse Down, then the focus moves here (Blur on the field that had it, Focus on this one).
    if pressed && usable && !f.read_only() {
        view.forms.fire(&f.name, Trigger::MouseDown, w.triggers, 0);
        view.forms.focus_field(&f.name, w.triggers);
    }
    if resp.clicked() && allowed && usable && !f.read_only() {
        // A push button's first Mouse Up action runs below; the rest of its chain after it.
        let skip = usize::from(f.kind == FormFieldKind::PushButton && f.button.is_some());
        view.forms.fire(&f.name, Trigger::MouseUp, w.triggers, skip);
    }
    if !resp.clicked() {
        return resp.is_pointer_button_down_on();
    }
    if !allowed {
        view.forms.notice = Some(FormNotice::Security);
        return true;
    }
    match f.kind {
        _ if f.read_only() => view.forms.notice = Some(FormNotice::ReadOnly(f.name.clone())),
        FormFieldKind::PushButton => match &f.button {
            Some(a) => view.forms.button = Some((f.name.clone(), a.clone())),
            None => view.forms.notice = Some(FormNotice::NoAction(f.name.clone())),
        },
        FormFieldKind::Signature => view.sign.field = Some(f.name.clone()),
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

/// After the pages' input: fire Mouse Exit / Enter when the pointer changed widget, Blur when a
/// press landed off every field, and Blur / Focus when Tab moved the editor to another field.
pub(crate) fn after_input(view: &mut DocView, form: &[FormField]) {
    let fv = &mut view.forms;
    let new = fv.hover_new.take();
    if !fv.hover_seen || new.is_some() {
        if !fv.hover_seen
            && let Some((name, _, bits)) = fv.hover.take()
        {
            fv.fire(&name, Trigger::MouseExit, bits, 0);
        }
        if let Some((name, wi, bits)) = new.filter(|_| !fv.hover_seen) {
            fv.fire(&name, Trigger::MouseEnter, bits, 0);
            fv.hover = Some((name, wi, bits));
        }
    }
    fv.hover_seen = false;
    if std::mem::take(&mut fv.pressed_off)
        && fv.focus.is_none()
        && let Some((name, bits)) = fv.active.take()
    {
        fv.fire(&name, Trigger::OnBlur, bits, 0);
    }
    // Tab moved the editor to another field (a press already moved the focus itself).
    let editor = fv.focus.as_ref().map(|fx| (fx.name.as_str(), fx.widget));
    if editor.map(|(n, _)| n) != fv.editor_was.as_deref() {
        let moved = editor.map(|(n, w)| (n.to_string(), w));
        fv.editor_was = moved.as_ref().map(|(n, _)| n.clone());
        if let Some((name, widget)) = moved
            && fv.active.as_ref().is_none_or(|(n, _)| *n != name)
        {
            let bits = form.iter().find(|f| f.name == name).and_then(|f| f.widgets.get(widget)).map_or(0, |w| w.triggers);
            fv.focus_field(&name, bits);
        }
    }
}

/// The edit the focused field's draft makes, if it differs from the field's value.
pub(crate) fn draft_edit(focus: &Focus, form: &[FormField]) -> Option<Edit> {
    let f = form.iter().find(|f| f.name == focus.name)?;
    let value = match f.kind {
        FormFieldKind::Text if f.value.first().map(String::as_str).unwrap_or("") != focus.text => FieldValue::Text(focus.text.clone()),
        FormFieldKind::List if f.has(field_flags::MULTI_SELECT) && focus.picked != f.value => FieldValue::Choice(focus.picked.clone()),
        _ => return None,
    };
    Some(Edit::SetFieldValue { name: f.name.clone(), value })
}

/// Close the focused field's editor, queueing the edit for its draft if it differs from the
/// field's value.
pub(crate) fn commit(view: &mut DocView, form: &[FormField]) {
    let Some(focus) = view.forms.focus.take() else { return };
    if let Some(edit) = draft_edit(&focus, form) {
        view.pending_edit = Some(edit);
        view.forms.committed = Some(focus);
    }
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// The date picker under a focused date field. Returns the picked date, formatted with the
/// field's own format.
fn calendar(ctx: &egui::Context, view: &mut DocView, field: egui::Rect, fmt: &str, today: (i64, u32, u32)) -> Option<String> {
    use pdfcraft_engine::form_scripts::{DateTime, format_date, parse_date};
    let fx = view.forms.focus.as_mut()?;
    let (mut y, mut m) = fx.calendar.unwrap_or_else(|| match parse_date(&fx.text, fmt) {
        Some(d) => (d.y, d.m),
        None => (today.0 as i32, today.1),
    });
    let current = parse_date(&fx.text, fmt);
    let mut picked = None;
    let area = egui::Area::new(egui::Id::new(("date-picker", view.id.0)))
        .order(egui::Order::Foreground)
        .fixed_pos(field.left_bottom() + vec2(0.0, 2.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.small_button("‹").on_hover_text(tl!("Previous month")).clicked() {
                        (y, m) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
                    }
                    ui.label(
                        egui::RichText::new(crate::i18n::fmt(
                            tl!("{month} {y}"),
                            &[("month", tl!(MONTHS[(m.clamp(1, 12) - 1) as usize])), ("y", &y.to_string())],
                        ))
                        .strong(),
                    );
                    if ui.small_button("›").on_hover_text(tl!("Next month")).clicked() {
                        (y, m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
                    }
                });
                // Weekday of the 1st (0 = Sunday), Zeller-style via days since 1970-01-01 (a Thursday).
                let days_from_civil = |y: i32, m: u32, d: u32| -> i64 {
                    let (y, m) = if m <= 2 { (y as i64 - 1, m as i64 + 9) } else { (y as i64, m as i64 - 3) };
                    let era = y.div_euclid(400);
                    let yoe = y - era * 400;
                    let doy = (153 * m + 2) / 5 + d as i64 - 1;
                    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
                };
                let first = (days_from_civil(y, m, 1) + 4).rem_euclid(7) as usize;
                let len = (days_from_civil(if m == 12 { y + 1 } else { y }, if m == 12 { 1 } else { m + 1 }, 1) - days_from_civil(y, m, 1)) as usize;
                egui::Grid::new(("date-grid", view.id.0)).spacing([4.0, 2.0]).show(ui, |ui| {
                    for d in ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"] {
                        ui.label(egui::RichText::new(d).small());
                    }
                    ui.end_row();
                    for cell in 0..(first + len).div_ceil(7) * 7 {
                        if cell >= first && cell < first + len {
                            let day = (cell - first + 1) as u32;
                            let selected = current.is_some_and(|c| (c.y, c.m, c.d) == (y, m, day));
                            let is_today = (today.0 as i32, today.1, today.2) == (y, m, day);
                            let mut text = egui::RichText::new(day.to_string());
                            if is_today {
                                text = text.strong();
                            }
                            if ui
                                .selectable_label(selected, text)
                                .on_hover_text(crate::i18n::fmt(
                                    tl!("{month} {day}, {y}"),
                                    &[
                                        ("month", tl_ctx!("calendar date", MONTHS[(m - 1) as usize])),
                                        ("day", &day.to_string()),
                                        ("y", &y.to_string()),
                                    ],
                                ))
                                .clicked()
                            {
                                picked = Some(format_date(DateTime { y, m, d: day, hh: 0, mm: 0, ss: 0 }, fmt));
                            }
                        } else {
                            ui.label("");
                        }
                        if cell % 7 == 6 {
                            ui.end_row();
                        }
                    }
                });
            });
        });
    if let Some(fx) = view.forms.focus.as_mut() {
        fx.calendar = Some((y, m));
        fx.calendar_rect = Some(area.response.rect);
    }
    picked
}

/// Paint focus and hover frames on a page.
pub(crate) fn paint_page(ui: &egui::Ui, painter: &egui::Painter, xf: &PageXform, page: usize, info: &DocInfo, form: &[FormField], view: &DocView) {
    let pointer = ui.input(|i| i.pointer.hover_pos());
    for f in form {
        for (wi, w) in f.widgets.iter().enumerate().filter(|(_, w)| w.page == Some(page) && !w.hidden) {
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
pub(crate) fn overlay(ctx: &egui::Context, view: &mut DocView, info: &DocInfo, form: &[FormField], today: (i64, u32, u32)) -> Option<Edit> {
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
                let Some(fx) = view.forms.focus.as_mut() else { return };
                // Only fields with keystroke checks keep the text from before this frame.
                let before = fx.keystrokes.then(|| fx.text.clone());
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
                if let Some(before) = before
                    && r.changed()
                    && view.forms.keystroke.is_none()
                {
                    let (change, sel) = keystroke_diff(&before, &fx.text);
                    view.forms.keystroke = Some(KeystrokeCheck { field: f.name.clone(), before, change, sel, after: fx.text.clone() });
                }
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
                } else if r.lost_focus()
                    && (!multiline || !enter)
                    && !fx.calendar_rect.is_some_and(|c| ui.input(|i| i.pointer.interact_pos().is_some_and(|p| c.contains(p))))
                {
                    // Enter in a single-line field, or clicking elsewhere, commits.
                    commit(view, form);
                }
            });
            // Date fields: a calendar under the field (Acrobat's date picker).
            if let pdfcraft_engine::form_scripts::Format::Date(fmt) = &f.actions.format
                && let Some(picked) = calendar(ctx, view, rect, fmt, today)
            {
                if let Some(fx) = view.forms.focus.as_mut() {
                    fx.text = picked;
                }
                commit(view, form);
                return view.pending_edit.take();
            }
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
                            let Some(fx) = view.forms.focus.as_mut() else { return };
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
                        if multi && ui.button(tl!("Done")).clicked() {
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
            .flat_map(|x| x.widgets.iter().enumerate().filter(|(_, w)| w.page.is_some() && !w.hidden).map(move |(wi, w)| (w.tab, x, wi)))
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

impl crate::PdfCraftApp {
    /// Fire field triggers of tab `index` (queued in [`FormView::fires`]). What they ask for is
    /// handled as a script's requests are (pages, print, links and submits through the internet
    /// access settings); menu items from the safe list run here.
    pub(crate) fn fire_field_triggers(&mut self, index: usize, fires: Vec<Fire>) {
        if fires.is_empty() {
            return;
        }
        let Some(id) = self.views.get(index).map(|v| v.id) else { return };
        // A value just typed or toggled is in the document before its triggers run (a check
        // box's Mouse Up script sees it checked; Blur comes after the commit).
        if self.active == Some(index) && !self.apply_queued_edit(index) {
            return;
        }
        for fire in fires.into_iter().take(MAX_FIRES_PER_FRAME) {
            match self.session.run_field_trigger_from(id, &fire.field, fire.trigger, fire.skip) {
                Ok(o) => self.handle_trigger(index, id, o),
                Err(e) => log::warn!("{} {}: {e}", fire.field, fire.trigger.label()),
            }
        }
    }

    fn handle_trigger(&mut self, index: usize, id: pdfcraft_engine::DocId, o: pdfcraft_engine::js::TriggerOutcome) {
        if o.ran.is_empty() {
            return;
        }
        if let Some(info) = self.session.get(id).map(|d| d.info.clone())
            && let Some(view) = self.views.get_mut(index)
        {
            view.document_changed(&info);
        }
        let pages = self.session.get(id).map_or(0, |d| d.info.pages.len());
        for m in &o.menu {
            let Some(view) = self.views.get_mut(index) else { break };
            match m.as_str() {
                "NextPage" => view.step_page(true),
                "PrevPage" => view.step_page(false),
                "FirstPage" => view.go_to_page(0),
                "LastPage" => view.go_to_page(pages.saturating_sub(1)),
                _ => {}
            }
        }
        self.handle_js(id, pdfcraft_engine::js::JsOutput { alerts: o.alerts, console: o.console, requests: o.requests, errors: o.errors });
    }

    /// Check the keystroke tab `index` queued: a refused one puts the field's text back, a
    /// script that changed `event.change` puts its text in.
    pub(crate) fn check_field_keystroke(&mut self, index: usize) {
        let Some(view) = self.views.get_mut(index) else { return };
        let Some(k) = view.forms.keystroke.take() else { return };
        let id = view.id;
        let outcome = match self.session.field_keystroke(id, &k.field, &k.before, &k.change, k.sel.clone()) {
            Ok(o) => o,
            Err(e) => {
                log::warn!("{}: keystroke check: {e}", k.field);
                return;
            }
        };
        let text = if !outcome.rc {
            k.before.clone()
        } else if outcome.change != k.change {
            k.before.chars().take(k.sel.start).chain(outcome.change.chars()).chain(k.before.chars().skip(k.sel.end)).collect()
        } else {
            k.after.clone()
        };
        if let Some(fx) = self.views.get_mut(index).and_then(|v| v.forms.focus.as_mut()).filter(|fx| fx.name == k.field && fx.text == k.after) {
            fx.text = text;
        }
        if !outcome.alerts.is_empty() || !outcome.console.is_empty() || !outcome.errors.is_empty() {
            self.handle_js(
                id,
                pdfcraft_engine::js::JsOutput { alerts: outcome.alerts, console: outcome.console, requests: Vec::new(), errors: outcome.errors },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keystroke_diffs_name_the_replaced_characters() {
        assert_eq!(keystroke_diff("12", "123"), ("3".to_string(), 2..2));
        assert_eq!(keystroke_diff("1234", "14"), (String::new(), 1..3));
        assert_eq!(keystroke_diff("abc", "aXc"), ("X".to_string(), 1..2));
        assert_eq!(keystroke_diff("", "日本"), ("日本".to_string(), 0..0));
        assert_eq!(keystroke_diff("aaa", "aaaa"), ("a".to_string(), 3..3));
        assert_eq!(keystroke_diff("same", "same"), (String::new(), 4..4));
    }
}

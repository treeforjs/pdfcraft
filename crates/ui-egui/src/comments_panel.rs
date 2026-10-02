//! The Comments panel (Acrobat's right-hand comment list; 11-visual-spec §3, audit `a15-*`).
//!
//! Header "Comments N" with search; an "Add a comment" box that posts a sticky note on the
//! current page; cards grouped by page. Selecting a card selects the comment on the page and
//! opens its reply box and "…" menu (Edit, Set status, Delete). Status replies show as a badge.

use egui::{Align, Color32, CornerRadius, Layout, Sense, Stroke, vec2};
use printcraft_engine::{Edit, NoteIcon, ReviewState, Shape};
use printcraft_render::{Annotation, DocInfo};

use crate::canvas::DocView;
use crate::comments::{CommentPrefs, CommentTool, SortBy, color32, status_badge};
use crate::icons;
use crate::panels::Nav;
use crate::theme::{self, Tokens};

/// Accent bar of the selected card (11-visual-spec §3: `#0165DD`).
const CARD_ACCENT: Color32 = Color32::from_rgb(0x01, 0x65, 0xDD);

/// Reader-friendly names for annotation subtypes (the PDF names are shown in tooltips).
pub fn subtype_label(s: &str) -> &str {
    match s {
        "Text" => "Note",
        "FreeText" => "Text box",
        "StrikeOut" => "Strikethrough",
        "Square" => "Rectangle",
        "Circle" => "Oval",
        "Ink" => "Drawing",
        "PolyLine" => "Polyline",
        "FileAttachment" => "Attachment",
        "Caret" => "Insert text",
        other => other,
    }
}

pub fn subtype_icon(s: &str) -> &'static str {
    match s {
        "Text" => "message-square-text",
        "Highlight" => "highlighter",
        "Underline" => "underline",
        "StrikeOut" => "strikethrough",
        "Squiggly" => "spline",
        "FreeText" => "type",
        "Ink" => "pencil",
        "Square" => "square",
        "Circle" => "circle",
        "Line" => "move-right",
        "Polygon" | "PolyLine" => "pen-tool",
        "Stamp" => "stamp",
        "FileAttachment" => "paperclip",
        "Caret" => "text-select",
        "Redact" => "rectangle-horizontal",
        _ => "message-square-text",
    }
}

fn initials(name: &str) -> String {
    let s: String = name.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect();
    if s.is_empty() { "?".into() } else { s.to_uppercase() }
}

/// The number shown in the header: comments and their replies (not status changes).
pub fn count(info: &DocInfo) -> usize {
    info.annotations.iter().filter(|a| a.state.is_none()).count()
}

fn matches(a: &Annotation, q: &str) -> bool {
    let q = q.to_lowercase();
    [a.author.as_deref(), a.contents.as_deref(), Some(subtype_label(&a.subtype))].into_iter().flatten().any(|s| s.to_lowercase().contains(&q))
}

/// Draw the panel body. Returns an edit to apply (posting, replying, deleting…).
pub(crate) fn show(
    ui: &mut egui::Ui,
    t: &Tokens,
    info: &DocInfo,
    view: &mut DocView,
    prefs: &CommentPrefs,
    allowed: bool,
    nav: &mut Option<Nav>,
) -> Option<Edit> {
    let mut edit = None;
    // Search (the header's magnifier toggles it).
    if let Some(q) = view.comments.search.as_mut() {
        let r = ui.add(egui::TextEdit::singleline(q).hint_text("Search comments").desired_width(f32::INFINITY).id_salt("comment-search"));
        if view.comments.search_focus {
            r.request_focus();
            view.comments.search_focus = false;
        }
        ui.add_space(6.0);
    }
    // "Add a comment": a sticky note on the current page, near its top-right corner.
    if allowed {
        let r = ui.add(
            egui::TextEdit::singleline(&mut view.comments.add_box)
                .hint_text("Add a comment")
                .desired_width(f32::INFINITY)
                .margin(egui::Margin::symmetric(8, 6))
                .id_salt("comment-add-box"),
        );
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !view.comments.add_box.trim().is_empty() {
            let page = view.current.min(info.pages.len().saturating_sub(1));
            if let Some(p) = info.pages.get(page) {
                let at = p.view_to_user(p.width - 44.0, 24.0);
                let text = std::mem::take(&mut view.comments.add_box);
                edit = Some(Edit::AddAnnotation(printcraft_engine::NewAnnotation {
                    page,
                    shape: Shape::Note { at: [at[0] as f64, at[1] as f64], icon: NoteIcon::Comment },
                    style: prefs.style(CommentTool::Note),
                    contents: text.trim().to_string(),
                    author: prefs.author.clone(),
                }));
            }
        }
        ui.add_space(8.0);
    }
    if info.annotations.is_empty() {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            ui.add(icons::image("message-square-text", 36.0, t.text_faint));
            ui.add_space(8.0);
            ui.label(egui::RichText::new("No comments yet.").color(t.text_muted));
            if allowed {
                ui.label(egui::RichText::new("Pick a tool in the quick bar, or type above.").color(t.text_faint).small());
            }
        });
        return edit;
    }
    let query = view.comments.search.clone().filter(|q| !q.trim().is_empty());
    let replies_of = |a: &Annotation| -> Vec<&Annotation> {
        match &a.name {
            Some(nm) => info.annotations.iter().filter(|r| r.in_reply_to.as_deref() == Some(nm.as_str())).collect(),
            None => Vec::new(),
        }
    };
    let cv = &view.comments;
    let status_of = |a: &Annotation| replies_of(a).iter().rev().find_map(|r| r.state.clone()).unwrap_or_else(|| "None".into());
    let mut roots: Vec<&Annotation> = info
        .annotations
        .iter()
        .filter(|a| a.in_reply_to.is_none())
        .filter(|a| query.as_deref().is_none_or(|q| matches(a, q) || replies_of(a).iter().any(|r| matches(r, q))))
        .filter(|a| !cv.hidden_types.iter().any(|t| t == subtype_label(&a.subtype)))
        .filter(|a| !cv.hidden_authors.contains(&a.author.clone().unwrap_or_default()))
        .filter(|a| !cv.hidden_statuses.contains(&status_of(a)))
        .collect();
    let sort = cv.sort;
    let color_key = |a: &Annotation| {
        a.color.map(|c| format!("{:02X}{:02X}{:02X}", (c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8)).unwrap_or_default()
    };
    match sort {
        SortBy::Page => {}
        SortBy::Author => roots.sort_by_key(|a| a.author.clone().unwrap_or_default().to_lowercase()),
        SortBy::Date => roots.sort_by(|a, b| b.modified.cmp(&a.modified)),
        SortBy::Type => roots.sort_by_key(|a| subtype_label(&a.subtype).to_string()),
        SortBy::Color => roots.sort_by_key(|a| color_key(a)),
    }
    if roots.is_empty() {
        ui.label(egui::RichText::new("No comments match.").color(t.text_muted));
    }
    // Group headers follow the sort key (no headers when sorted by date).
    let group_of = |a: &Annotation| -> Option<String> {
        match sort {
            SortBy::Page => Some(format!("Page {}", info.pages.get(a.page).map(|p| p.label.as_str()).unwrap_or("?"))),
            SortBy::Author => Some(a.author.clone().unwrap_or_else(|| "Unknown author".into())),
            SortBy::Type => Some(subtype_label(&a.subtype).to_string()),
            SortBy::Color => Some(if a.color.is_some() { format!("#{}", color_key(a)) } else { "No colour".into() }),
            SortBy::Date => None,
        }
    };
    let mut page = usize::MAX;
    let mut group: Option<String> = None;
    for a in roots {
        if sort != SortBy::Page {
            let g = group_of(a);
            if g.is_some() && g != group {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(g.clone().unwrap_or_default()).font(theme::semibold(12.5)).color(t.text_muted));
                group = g;
            }
        } else if a.page != page {
            page = a.page;
            let n = info.annotations.iter().filter(|x| x.page == page && x.in_reply_to.is_none()).count();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add(icons::image("chevron-down", 14.0, t.text_muted));
                ui.label(egui::RichText::new(format!("Page {}", info.pages[page].label)).font(theme::semibold(12.5)).color(t.text_muted));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(egui::RichText::new(n.to_string()).color(t.text_faint).small()));
            });
            ui.add_space(2.0);
        }
        let thread = replies_of(a);
        if let Some(e) = card(ui, t, a, &thread, view, prefs, allowed, nav) {
            edit = Some(e);
        }
    }
    edit
}

#[allow(clippy::too_many_arguments)]
fn card(
    ui: &mut egui::Ui,
    t: &Tokens,
    a: &Annotation,
    thread: &[&Annotation],
    view: &mut DocView,
    prefs: &CommentPrefs,
    allowed: bool,
    nav: &mut Option<Nav>,
) -> Option<Edit> {
    let mut edit = None;
    let key = (a.page, a.index);
    let selected = view.comments.selected == Some(key);
    let color = a.color.map(|c| color32(c.map(f64::from))).unwrap_or(t.accent);
    let status = thread.iter().rev().find_map(|r| r.state.as_deref());
    let replies: Vec<&&Annotation> = thread.iter().filter(|r| r.state.is_none()).collect();
    let editing = view.comments.editing.as_ref().is_some_and(|(p, i, _)| (*p, *i) == key);
    let fill = if selected { t.hover.gamma_multiply(0.7) } else { Color32::TRANSPARENT };
    let mut open_menu = false;
    let frame = egui::Frame::NONE
        .fill(fill)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin { left: 10, right: 8, top: 10, bottom: 10 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let head = ui.horizontal(|ui| {
                avatar(ui, t, Some(subtype_icon(&a.subtype)), None, color);
                ui.label(egui::RichText::new(a.author.as_deref().unwrap_or("Unknown author")).font(theme::semibold(12.5)).color(t.text));
                if let Some(m) = &a.modified {
                    ui.add(egui::Label::new(egui::RichText::new(m).font(theme::regular(11.0)).color(t.text_faint)).truncate());
                }
                if selected && allowed {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        open_menu = icons::button(ui, "ellipsis", 22.0, false, "More").clicked();
                    });
                }
            });
            egui::Frame::NONE.inner_margin(egui::Margin { left: 34, right: 0, top: 4, bottom: 0 }).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if editing {
                    let (_, _, text) = view.comments.editing.as_mut().expect("checked");
                    let r = ui.add(egui::TextEdit::multiline(text).desired_rows(2).desired_width(f32::INFINITY).id_salt(("comment-edit", key)));
                    if !r.has_focus() && !ui.memory(|m| m.focused().is_some()) {
                        r.request_focus();
                    }
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(egui::RichText::new("Save").color(Color32::WHITE)).fill(t.accent).corner_radius(12)).clicked() {
                            let (page, index, text) = view.comments.editing.take().expect("checked");
                            edit = Some(Edit::SetAnnotationContents { page, index, text });
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            view.comments.editing = None;
                        }
                    });
                } else {
                    let body = a.contents.as_deref().unwrap_or("");
                    let text = if body.is_empty() {
                        egui::RichText::new(subtype_label(&a.subtype)).italics().color(t.text_faint)
                    } else {
                        egui::RichText::new(body).color(t.text)
                    };
                    let mut label = egui::Label::new(text.font(theme::regular(13.0))).wrap();
                    if !selected {
                        label = label.truncate();
                    }
                    ui.add(label);
                }
                if let Some((icon, label, c)) = status.and_then(status_badge) {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add(icons::image(icon, 13.0, c));
                        ui.label(egui::RichText::new(label).font(theme::medium(11.5)).color(c));
                    });
                }
            });
            // Replies, joined to the parent by a thread line.
            for r in &replies {
                ui.add_space(8.0);
                let resp = ui.horizontal(|ui| {
                    avatar(ui, t, None, Some(&initials(r.author.as_deref().unwrap_or("?"))), t.text_faint);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(r.author.as_deref().unwrap_or("Reply")).font(theme::semibold(12.0)));
                            if let Some(m) = &r.modified {
                                ui.add(egui::Label::new(egui::RichText::new(m).font(theme::regular(11.0)).color(t.text_faint)).truncate());
                            }
                        });
                        ui.add(
                            egui::Label::new(egui::RichText::new(r.contents.as_deref().unwrap_or("")).font(theme::regular(12.5)).color(t.text))
                                .wrap(),
                        );
                    });
                });
                let x = resp.response.rect.left() + 12.0;
                ui.painter().vline(x, (resp.response.rect.top() - 14.0)..=(resp.response.rect.top() - 2.0), Stroke::new(1.5, t.border));
            }
            if selected && allowed {
                ui.add_space(8.0);
                egui::Frame::NONE.inner_margin(egui::Margin { left: 34, right: 0, top: 0, bottom: 0 }).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut view.comments.reply)
                                .hint_text("Add a reply")
                                .desired_width(ui.available_width() - 56.0)
                                .id_salt(("comment-reply", key)),
                        );
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let ok = !view.comments.reply.trim().is_empty();
                        if (ui.add_enabled(ok, egui::Button::new("Post").corner_radius(12)).clicked() || enter) && ok {
                            let text = std::mem::take(&mut view.comments.reply).trim().to_string();
                            edit = Some(Edit::ReplyToAnnotation { page: a.page, index: a.index, text, author: prefs.author.clone() });
                        }
                    });
                });
            }
            head.response.rect
        });
    let rect = frame.response.rect;
    if selected {
        let bar = egui::Rect::from_min_size(rect.min, vec2(4.0, rect.height()));
        ui.painter().rect_filled(bar, CornerRadius { nw: 6, sw: 6, ne: 0, se: 0 }, CARD_ACCENT);
        if std::mem::take(&mut view.comments.reveal) {
            frame.response.scroll_to_me(Some(Align::Center));
        }
    }
    ui.painter().hline(rect.x_range().shrink(10.0), rect.bottom() + 2.0, Stroke::new(1.0, t.divider));
    ui.add_space(5.0);
    // The header row selects the card (and shows the comment on the page); unselected cards are
    // clickable everywhere.
    let hit = if selected { frame.inner } else { rect };
    let click = ui
        .interact(hit, ui.id().with(("card", key)), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!("{} — click to show it on the page", subtype_label(&a.subtype)));
    if click.clicked() {
        view.comments.selected = Some(key);
        view.comments.reply.clear();
        *nav = Some(Nav::Flash(a.page, a.rect));
    }
    if click.double_clicked() && allowed {
        view.comments.editing = Some((a.page, a.index, a.contents.clone().unwrap_or_default()));
    }
    let menu_id = ui.id().with(("card-menu", key));
    if open_menu {
        egui::Popup::open_id(ui.ctx(), menu_id);
    }
    egui::Popup::new(menu_id, ui.ctx().clone(), egui::PopupAnchor::Pointer, ui.layer_id())
        .open_memory(None)
        .kind(egui::PopupKind::Menu)
        .layout(Layout::top_down_justified(Align::Min))
        .show(|ui| {
            ui.set_min_width(160.0);
            if ui.button("Edit").clicked() {
                view.comments.editing = Some((a.page, a.index, a.contents.clone().unwrap_or_default()));
                ui.close();
            }
            ui.menu_button("Set status", |ui| {
                for s in [ReviewState::None, ReviewState::Accepted, ReviewState::Cancelled, ReviewState::Completed, ReviewState::Rejected] {
                    if ui.button(s.name()).clicked() {
                        edit = Some(Edit::SetAnnotationStatus { page: a.page, index: a.index, state: s, author: prefs.author.clone() });
                        ui.close();
                    }
                }
            });
            if ui.button("Properties…").clicked() {
                view.comments.props_request = Some((a.page, a.index));
                ui.close();
            }
            ui.separator();
            if ui.button("Delete").clicked() {
                view.comments.selected = None;
                edit = Some(Edit::DeleteAnnotation { page: a.page, index: a.index });
                ui.close();
            }
        });
    edit
}

/// A 26 pt round badge: a type glyph (comments) or initials (replies).
fn avatar(ui: &mut egui::Ui, t: &Tokens, icon: Option<&str>, text: Option<&str>, color: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
    let bg = if t.dark() { Color32::from_gray(0x55) } else { Color32::from_gray(0xD8) };
    ui.painter().circle_filled(r.center(), 12.0, bg);
    if let Some(i) = icon {
        icons::paint(ui, r, i, 13.0, legible(color, t));
    }
    if let Some(s) = text {
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, s, theme::semibold(10.5), t.text);
    }
}

/// Darken very light annotation colours (e.g. note yellow) so their glyph stays legible.
fn legible(c: Color32, t: &Tokens) -> Color32 {
    let lum = 0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32;
    if !t.dark() && lum > 150.0 {
        Color32::from_rgb((c.r() as f32 * 0.55) as u8, (c.g() as f32 * 0.55) as u8, (c.b() as f32 * 0.55) as u8)
    } else {
        c
    }
}

/// The Comments panel header's filter and sort controls (Acrobat: filter funnel and "…").
pub(crate) fn header_controls(ui: &mut egui::Ui, info: &DocInfo, view: &mut DocView) {
    let cv = &mut view.comments;
    let mut types: Vec<String> = info.annotations.iter().filter(|a| a.in_reply_to.is_none()).map(|a| subtype_label(&a.subtype).to_string()).collect();
    types.sort();
    types.dedup();
    let mut authors: Vec<String> =
        info.annotations.iter().filter(|a| a.in_reply_to.is_none()).map(|a| a.author.clone().unwrap_or_default()).collect();
    authors.sort();
    authors.dedup();
    let active = !(cv.hidden_types.is_empty() && cv.hidden_authors.is_empty() && cv.hidden_statuses.is_empty());
    let more = icons::button(ui, "ellipsis", 26.0, false, "Sort comments");
    egui::Popup::menu(&more).show(|ui| {
        ui.label(egui::RichText::new("Sort by").small());
        for (s, label) in
            [(SortBy::Page, "Page"), (SortBy::Author, "Author"), (SortBy::Date, "Date"), (SortBy::Type, "Type"), (SortBy::Color, "Colour")]
        {
            if ui.radio(cv.sort == s, label).clicked() {
                cv.sort = s;
                ui.close();
            }
        }
    });
    let filter = icons::button(ui, "filter", 26.0, active, "Filter comments");
    egui::Popup::menu(&filter).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_min_width(200.0);
        let toggle = |ui: &mut egui::Ui, list: &mut Vec<String>, value: &str, label: &str| {
            let mut shown = !list.iter().any(|x| x == value);
            if ui.checkbox(&mut shown, label).changed() {
                if shown {
                    list.retain(|x| x != value);
                } else {
                    list.push(value.to_string());
                }
            }
        };
        ui.label(egui::RichText::new("Type").small());
        for ty in &types {
            toggle(ui, &mut cv.hidden_types, ty, ty);
        }
        ui.separator();
        ui.label(egui::RichText::new("Author").small());
        for a in &authors {
            toggle(ui, &mut cv.hidden_authors, a, if a.is_empty() { "Unknown author" } else { a });
        }
        ui.separator();
        ui.label(egui::RichText::new("Status").small());
        for s in ["None", "Accepted", "Rejected", "Cancelled", "Completed"] {
            toggle(ui, &mut cv.hidden_statuses, s, s);
        }
        if active && ui.button("Show all").clicked() {
            cv.hidden_types.clear();
            cv.hidden_authors.clear();
            cv.hidden_statuses.clear();
        }
    });
}

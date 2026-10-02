//! Modal dialogs: Document Properties, Keyboard Shortcuts, About.

use egui::{Align, Layout};

use crate::theme::{self, Tokens};
use printcraft_engine::Edit;

use crate::{CloseRequest, Dialog, PrintCraftApp, PropsTab, panels::human_size, widgets};

const INFO_KEYS: [&str; 4] = ["Title", "Author", "Subject", "Keywords"];

pub fn show(app: &mut PrintCraftApp, ctx: &egui::Context) {
    password(app, ctx);
    save_prompt(app, ctx);
    let Some(dialog) = app.dialog else {
        app.props_draft = None;
        return;
    };
    // Seed the editable Description fields from the document when the dialog opens.
    if let (Dialog::Properties(_), Some((_, id))) = (dialog, app.active_ids())
        && app.props_draft.as_ref().is_none_or(|(d, _)| *d != id)
        && let Some(doc) = app.session.get(id)
    {
        app.props_draft = Some((id, INFO_KEYS.map(|k| doc.info_value(k).unwrap_or_default())));
    }
    let mut apply = false;
    let mut split_now: Option<printcraft_engine::SplitBy> = None;
    let mut split_ready: Option<printcraft_engine::SplitBy> = None;
    let mut recover: Option<bool> = None;
    let mut number_now: Option<Edit> = None;
    let mut apply_number = false;
    let mut link_command: Option<&'static str> = None;
    let mut protect_now = false;
    let mut boxes_now = false;
    let mut marks_now = false;
    let mut export_now = false;
    let mut props_now = false;
    let mut field_props_now = false;
    let mut redact_now: Option<Dialog> = None;
    let mut print_go = false;
    let mut replace_now = false;
    let t = Tokens::get(ctx);
    let mut close = false;
    let mut next = dialog;
    let modal = egui::Modal::new(egui::Id::new("dialog")).show(ctx, |ui| {
        ui.set_width(match dialog {
            Dialog::Properties(_) => 640.0,
            Dialog::Print => 820.0,
            Dialog::FieldProps => 600.0,
            _ => 520.0,
        });
        // Dialog controls are outlined (radio buttons, check boxes, combo boxes and number fields
        // would otherwise blend into the dialog, whose fill matches the theme's field colour).
        let w = &mut ui.visuals_mut().widgets;
        w.inactive.bg_stroke = egui::Stroke::new(1.0, t.border);
        w.inactive.weak_bg_fill = t.field;
        // Slider rails and check-box interiors use the plain fill.
        w.inactive.bg_fill = t.hover;
        w.hovered.bg_stroke = egui::Stroke::new(1.0, t.text_muted);
        match dialog {
            Dialog::Properties(tab) => {
                ui.label(egui::RichText::new("Document Properties").font(theme::semibold(18.0)));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    for (tb, label) in [
                        (PropsTab::Description, "Description"),
                        (PropsTab::Security, "Security"),
                        (PropsTab::Fonts, "Fonts"),
                        (PropsTab::Advanced, "Advanced"),
                    ] {
                        if widgets::mode_tab(ui, label, tab == tb).clicked() {
                            next = Dialog::Properties(tb);
                        }
                    }
                });
                ui.separator();
                let Some((_, id)) = app.active_ids() else { return };
                let Some(doc) = app.session.get(id) else { return };
                let i = &doc.info;
                let row = |ui: &mut egui::Ui, k: &str, v: String| {
                    ui.label(egui::RichText::new(k).color(t.text_muted));
                    ui.label(if v.is_empty() { egui::RichText::new("—").color(t.text_faint) } else { egui::RichText::new(v) });
                    ui.end_row();
                };
                egui::ScrollArea::vertical().max_height(460.0).auto_shrink([false, true]).show(ui, |ui| {
                    egui::Grid::new("props").num_columns(2).spacing([18.0, 8.0]).min_col_width(140.0).show(ui, |ui| match tab {
                        PropsTab::Description => {
                            row(ui, "File", doc.name.clone());
                            match app.props_draft.as_mut() {
                                Some((_, draft)) if doc.allows_modification() => {
                                    for (k, v) in INFO_KEYS.iter().zip(draft.iter_mut()) {
                                        let l = ui.label(egui::RichText::new(*k).color(t.text_muted));
                                        ui.add(
                                            egui::TextEdit::singleline(v)
                                                .desired_width(420.0)
                                                .background_color(t.field)
                                                .margin(egui::Margin::symmetric(6, 4))
                                                .id_salt(("info", *k)),
                                        )
                                        .labelled_by(l.id);
                                        ui.end_row();
                                    }
                                }
                                _ => {
                                    row(ui, "Title", i.title.clone().unwrap_or_default());
                                    row(ui, "Author", i.author.clone().unwrap_or_default());
                                    row(ui, "Subject", i.subject.clone().unwrap_or_default());
                                    row(ui, "Keywords", i.keywords.clone().unwrap_or_default());
                                }
                            }
                            row(ui, "Application", i.creator.clone().unwrap_or_default());
                            row(ui, "PDF producer", i.producer.clone().unwrap_or_default());
                        }
                        PropsTab::Security => match doc.security_summary() {
                            None => {
                                row(ui, "Security method", "No security".into());
                                row(ui, "Restrictions", "None — everything is allowed".into());
                                if doc.allows_security_change() && ui.button("Protect using password…").clicked() {
                                    link_command = Some("protect.password");
                                }
                            }
                            Some(sec) => {
                                row(ui, "Security method", "Password security".into());
                                row(ui, "Encryption", sec.method.clone());
                                row(
                                    ui,
                                    "Opened with",
                                    if sec.pending {
                                        "— (protection is applied when you save)".into()
                                    } else if sec.owner {
                                        "Owner password (no restrictions apply)".into()
                                    } else {
                                        "User password".into()
                                    },
                                );
                                if doc.allows_security_change() {
                                    ui.horizontal(|ui| {
                                        if ui.button("Change settings…").clicked() {
                                            link_command = Some("protect.password");
                                        }
                                        if ui.button("Remove security").clicked() {
                                            link_command = Some("protect.remove");
                                        }
                                    });
                                }
                                let p = sec.permissions;
                                let yes = |b: bool| if b { "Allowed".to_string() } else { "Not allowed".to_string() };
                                row(
                                    ui,
                                    "Printing",
                                    if !p.print() {
                                        "Not allowed".into()
                                    } else if p.print_high_quality() {
                                        "High resolution".into()
                                    } else {
                                        "Low resolution".into()
                                    },
                                );
                                row(ui, "Changing the document", yes(p.modify()));
                                row(ui, "Document assembly", yes(p.assemble()));
                                row(ui, "Content copying", yes(p.copy()));
                                row(ui, "Content copying for accessibility", yes(p.extract_for_accessibility()));
                                row(ui, "Commenting", yes(p.annotate()));
                                row(ui, "Filling of form fields", yes(p.fill_forms()));
                            }
                        },
                        PropsTab::Fonts => {
                            if i.fonts.is_empty() {
                                row(ui, "Fonts", "No fonts are referenced by the pages.".into());
                            }
                            for f in &i.fonts {
                                let mut detail = f.kind.clone();
                                if let Some(e) = &f.encoding {
                                    detail.push_str(&format!(" · {e}"));
                                }
                                detail.push_str(if f.subset {
                                    " · Embedded subset"
                                } else if f.embedded {
                                    " · Embedded"
                                } else {
                                    " · Not embedded (substituted)"
                                });
                                row(ui, &f.name, detail);
                            }
                        }
                        PropsTab::Advanced => {
                            row(ui, "PDF version", i.pdf_version.replace("Pdf", "").replace('_', "."));
                            row(ui, "Location", doc.path.clone().unwrap_or_default());
                            row(ui, "File size", format!("{} ({} bytes)", human_size(i.file_size), i.file_size));
                            let p = &i.pages[0];
                            row(ui, "Page size", format!("{:.2} × {:.2} in", p.width / 72.0, p.height / 72.0));
                            row(ui, "Number of pages", i.pages.len().to_string());
                            row(ui, "Tagged PDF", yes(i.tagged));
                            row(ui, "Form fields", i.fields.len().to_string());
                            row(ui, "Comments", i.annotations.len().to_string());
                            row(ui, "Layers", i.layers.len().to_string());
                            row(ui, "Attachments", i.attachments.len().to_string());
                            row(ui, "JavaScript", yes(i.has_javascript));
                        }
                    })
                });
            }
            Dialog::Split => {
                ui.label(egui::RichText::new("Split document").font(theme::semibold(18.0)));
                ui.add_space(8.0);
                let Some((vi, id)) = app.active_ids() else { return };
                let n = app.session.get(id).map(|d| d.info.pages.len()).unwrap_or(0);
                let selected: Vec<usize> = app.views[vi].selected.iter().copied().filter(|p| *p > 0).collect();
                let draft = &mut app.split_draft;
                if selected.is_empty() {
                    draft.at_selection = false;
                }
                ui.radio_value(&mut draft.at_selection, false, "By number of pages");
                ui.add_enabled_ui(!draft.at_selection, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(24.0);
                        ui.label("Pages per file");
                        ui.add(egui::DragValue::new(&mut draft.every).range(1..=n.max(1)));
                    });
                });
                ui.add_enabled_ui(!selected.is_empty(), |ui| {
                    ui.radio_value(&mut draft.at_selection, true, "Before each selected page (select pages in Organize)")
                });
                let by = if draft.at_selection {
                    printcraft_engine::SplitBy::Before(selected)
                } else {
                    printcraft_engine::SplitBy::PageCount(draft.every)
                };
                let files = printcraft_engine::split_ranges(n, &by).len();
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!("Creates {files} file{} from {n} pages.", if files == 1 { "" } else { "s" })).color(t.text_muted),
                );
                if files > 1 {
                    split_ready = Some(by);
                }
            }
            Dialog::ReplacePages => {
                let count = app.active_ids().and_then(|(_, id)| app.session.get(id)).map(|d| d.info.pages.len()).unwrap_or(1).max(1);
                let Some(d) = app.replace_draft.as_mut() else {
                    close = true;
                    return;
                };
                ui.label(egui::RichText::new("Replace Pages").font(theme::semibold(18.0)));
                ui.add_space(8.0);
                d.to = d.to.clamp(1, count);
                d.from = d.from.clamp(1, d.to);
                let n = d.to - d.from + 1;
                ui.horizontal(|ui| {
                    ui.label("Original: replace pages");
                    ui.add(egui::DragValue::new(&mut d.from).range(1..=count));
                    ui.label("to");
                    ui.add(egui::DragValue::new(&mut d.to).range(1..=count));
                    ui.label(egui::RichText::new(format!("of {count}")).color(t.text_muted));
                });
                let max_start = d.src_pages.saturating_sub(n) + 1;
                d.src_from = d.src_from.clamp(1, max_start.max(1));
                ui.horizontal(|ui| {
                    ui.label(format!("Replacement: pages of {}", d.name));
                    ui.add(egui::DragValue::new(&mut d.src_from).range(1..=max_start.max(1)));
                    ui.label(format!("to {}", d.src_from + n - 1));
                    ui.label(egui::RichText::new(format!("of {}", d.src_pages)).color(t.text_muted));
                });
                let fits = n <= d.src_pages;
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(if fits {
                        "Only the page content changes: links, comments, form fields and bookmarks on the original pages stay."
                    } else {
                        "The replacement file doesn't have that many pages."
                    })
                    .small()
                    .color(t.text_faint),
                );
                ui.add_space(10.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add_enabled_ui(fits, |ui| widgets::pill_button(ui, "OK", true)).inner.clicked() {
                        replace_now = true;
                        close = true;
                    }
                    if widgets::pill_button(ui, "Cancel", false).clicked() {
                        close = true;
                    }
                });
                return;
            }
            Dialog::RedactPages => {
                let n = app.active_ids().and_then(|(_, id)| app.session.get(id)).map_or(1, |d| d.info.pages.len());
                let (ok, cancel) = crate::redact_ui::pages_body(ui, &mut app.redact_pages_draft, n, &t);
                if ok {
                    redact_now = Some(dialog);
                }
                close = ok || cancel;
                return;
            }
            Dialog::RedactSearch => {
                let (go, cancel) = crate::redact_ui::search_body(ui, &mut app.redact_search, &t);
                if go {
                    redact_now = Some(dialog);
                }
                close = cancel;
                return;
            }
            Dialog::RedactProps => {
                let mut prefs = app.redact_prefs.clone();
                let (ok, cancel) = crate::redact_ui::props_body(ui, &mut prefs, &t);
                app.redact_prefs = prefs;
                close = ok || cancel;
                return;
            }
            Dialog::RedactApply => {
                let marks = app.active_ids().and_then(|(_, id)| app.session.get(id)).map_or(0, |d| d.redaction_marks());
                let (ok, cancel) = crate::redact_ui::apply_body(ui, marks, &t);
                if ok {
                    redact_now = Some(dialog);
                }
                close = ok || cancel;
                return;
            }
            Dialog::Print => {
                let Some((i, id)) = app.active_ids() else {
                    close = true;
                    return;
                };
                let Some(doc) = app.session.get(id) else { return };
                let sizes: Vec<(f64, f64)> = doc.info.pages.iter().map(|p| (p.width as f64, p.height as f64)).collect();
                let labels: Vec<String> = doc.info.pages.iter().map(|p| p.label.clone()).collect();
                let thumbs: std::collections::HashMap<usize, egui::TextureId> =
                    (0..sizes.len()).filter_map(|p| app.views[i].thumb_id(p).map(|t| (p, t))).collect();
                let (go, cancel) = crate::print_ui::body(ui, &mut app.print_draft, &t, &sizes, &labels, &|p| thumbs.get(&p).copied());
                print_go = go;
                close = go || cancel;
                return;
            }
            Dialog::RemoveHidden => {
                let (ok, cancel) = crate::redact_ui::hidden_body(ui, &mut app.hidden_draft, &t);
                if ok {
                    redact_now = Some(dialog);
                }
                close = ok || cancel;
                return;
            }
            Dialog::Sanitize => {
                let (ok, cancel) = crate::redact_ui::sanitize_body(ui, &t);
                if ok {
                    redact_now = Some(dialog);
                }
                close = ok || cancel;
                return;
            }
            Dialog::FieldProps => {
                let Some(d) = app.field_props.as_mut() else {
                    close = true;
                    return;
                };
                let (apply, cancel) = crate::prepare::body(ui, d, &t);
                field_props_now = apply;
                close = apply || cancel;
                return;
            }
            Dialog::CommentProps => {
                let (apply, cancel) = crate::comment_props::body(ui, app, &t);
                props_now = apply;
                close = apply || cancel;
                return;
            }
            Dialog::Signature => {
                let (apply, cancel) = crate::fill_sign::signature_pad(ui, &t, &mut app.signature_draft);
                if apply {
                    app.signature = Some(std::mem::take(&mut app.signature_draft));
                    app.quick_tool = crate::QuickTool::Fill(crate::fill_sign::FillTool::Signature);
                    app.toast = None;
                }
                close = apply || cancel;
                return;
            }
            Dialog::Export(kind) => {
                let (apply, cancel) = crate::export_ui::body(ui, app, &t, kind);
                export_now = apply;
                close = apply || cancel;
                return;
            }
            Dialog::Marks(kind) => {
                ui.set_width(720.0);
                let (apply, cancel) = crate::marks_ui::body(ui, app, &t, kind);
                marks_now = apply;
                close = apply || cancel;
                return;
            }
            Dialog::PageBoxes => {
                let (apply, cancel) = crate::pageboxes::body(ui, app, &t);
                boxes_now = apply;
                close = apply || cancel;
                return;
            }
            Dialog::Protect => {
                let (apply, cancel) = crate::protect::body(ui, app, &t);
                protect_now = apply;
                close = apply || cancel;
                return;
            }
            Dialog::NumberPages => {
                use printcraft_engine::LabelStyle as L;
                ui.label(egui::RichText::new("Number pages").font(theme::semibold(18.0)));
                ui.add_space(8.0);
                let Some((_, id)) = app.active_ids() else { return };
                let n = app.session.get(id).map(|d| d.info.pages.len()).unwrap_or(1).max(1);
                let d = &mut app.number_draft;
                d.to = d.to.clamp(1, n);
                d.from = d.from.clamp(1, d.to);
                // Editable values get a visible border (the dialog and field fills are alike).
                let boxed = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui) -> egui::Response| {
                    egui::Frame::new()
                        .stroke(egui::Stroke::new(1.0, t.border))
                        .corner_radius(egui::CornerRadius::same(5))
                        .inner_margin(egui::Margin::symmetric(4, 1))
                        .show(ui, |ui| add(ui))
                        .inner
                };
                egui::Grid::new("number_pages").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Pages");
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut d.from).range(1..=n));
                        ui.label("to");
                        ui.add(egui::DragValue::new(&mut d.to).range(1..=n));
                        ui.label(egui::RichText::new(format!("of {n}")).color(t.text_muted));
                    });
                    ui.end_row();
                    ui.label("Style");
                    let styles = [
                        (L::Decimal, "1, 2, 3"),
                        (L::LowerRoman, "i, ii, iii"),
                        (L::UpperRoman, "I, II, III"),
                        (L::LowerAlpha, "a, b, c"),
                        (L::UpperAlpha, "A, B, C"),
                        (L::None, "None (prefix only)"),
                    ];
                    let current = styles.iter().find(|(s, _)| *s == d.style).map_or("1, 2, 3", |(_, l)| *l);
                    egui::ComboBox::from_id_salt("label_style").selected_text(current).show_ui(ui, |ui| {
                        for (s, l) in styles {
                            ui.selectable_value(&mut d.style, s, l);
                        }
                    });
                    ui.end_row();
                    let l = ui.label("Prefix");
                    boxed(ui, &mut |ui| ui.add(egui::TextEdit::singleline(&mut d.prefix).desired_width(160.0).frame(egui::Frame::NONE)))
                        .labelled_by(l.id);
                    ui.end_row();
                    ui.label("Start");
                    ui.add(egui::DragValue::new(&mut d.start).range(1..=99_999));
                    ui.end_row();
                });
                d.to = d.to.max(d.from);
                let label = |k: u32| format!("{}{}", d.prefix, d.style.format(k));
                ui.add_space(8.0);
                let preview = if d.from == d.to {
                    label(d.start)
                } else {
                    format!("{}, {} … {}", label(d.start), label(d.start + 1), label(d.start + (d.to - d.from) as u32))
                };
                ui.label(egui::RichText::new(format!("Labels: {preview}. Later pages keep their labels.")).color(t.text_muted));
                number_now = Some(Edit::NumberPages { from: d.from - 1, to: d.to - 1, style: d.style, prefix: d.prefix.clone(), first: d.start });
            }
            Dialog::Recovery => {
                ui.horizontal(|ui| {
                    ui.add(crate::icons::image("clock-3", 22.0, t.accent));
                    ui.label(egui::RichText::new("Recover unsaved documents?").font(theme::semibold(18.0)));
                });
                ui.add_space(6.0);
                ui.label("PrintCraft didn't shut down normally. These documents had changes that were autosaved:");
                ui.add_space(8.0);
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                egui::Grid::new("recoverable").num_columns(2).spacing([18.0, 6.0]).show(ui, |ui| {
                    for m in &app.recoverable {
                        ui.label(egui::RichText::new(&m.name).font(theme::medium(13.0)));
                        let mins = now.saturating_sub(m.saved_at) / 60;
                        let when = match mins {
                            0 => "just now".to_string(),
                            1..=59 => format!("{mins} min ago"),
                            _ => format!("{} h ago", mins / 60),
                        };
                        let lock = if m.encrypted { " · password-protected" } else { "" };
                        ui.label(egui::RichText::new(format!("{when}{lock}")).color(t.text_muted));
                        ui.end_row();
                    }
                });
            }
            Dialog::Shortcuts => {
                ui.label(egui::RichText::new("Keyboard shortcuts").font(theme::semibold(18.0)));
                ui.add_space(8.0);
                let mac = cfg!(target_os = "macos") || cfg!(target_arch = "wasm32");
                // Registered commands first (always in sync with the real bindings), then the
                // keys the document view handles itself.
                let mut rows: Vec<(String, String)> = printcraft_engine::commands::COMMANDS
                    .iter()
                    .filter_map(|c| c.shortcut.map(|k| (k.label(mac), c.label.trim_end_matches('…').to_string())))
                    .collect();
                for (k, v) in [
                    ("⌘G / ⇧⌘G", "Next / previous match"),
                    ("⌘C", "Copy selected text"),
                    ("Double-click", "Select a word"),
                    ("Esc", "Clear selection / close find"),
                    ("⌘1", "Actual size"),
                    ("⌘0", "Zoom to page level"),
                    ("⌘2", "Fit to width"),
                    ("⌘+ / ⌘−", "Zoom in / out (also pinch or ⌘-scroll)"),
                    ("⇧⌘+ / ⇧⌘−", "Rotate view"),
                    ("Home / End", "First / last page"),
                    ("⌘← / ⌘→", "Previous / next page"),
                    ("Delete", "Delete selected pages (Organize)"),
                    ("⌘A", "Select all pages (Organize)"),
                ] {
                    rows.push((k.to_string(), v.to_string()));
                }
                egui::ScrollArea::vertical().max_height(460.0).show(ui, |ui| {
                    egui::Grid::new("keys").num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
                        for (k, v) in &rows {
                            ui.label(egui::RichText::new(k).font(egui::FontId::monospace(12.5)));
                            ui.label(v);
                            ui.end_row();
                        }
                    });
                });
            }
            Dialog::About => {
                ui.horizontal(|ui| {
                    widgets::artcraft_mark(ui, 40.0);
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("PrintCraft").font(theme::semibold(20.0)));
                        ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                    });
                });
                ui.add_space(6.0);
                ui.label("A clean-room, open-source PDF application written in Rust. MIT OR Apache-2.0.");
                ui.label(
                    egui::RichText::new("Rendering: hayro (bootstrap) · UI: egui · Icons: Lucide (ISC) · Fonts: Inter, JetBrains Mono (OFL)")
                        .color(t.text_muted)
                        .small(),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Part of").color(t.text_muted));
                    widgets::artcraft_logo(ui, 16.0);
                });
                ui.add_space(6.0);
                if let Some(cmd) = widgets::community_links(ui) {
                    link_command = Some(cmd);
                }
            }
        }
        ui.add_space(12.0);
        let changed = draft_changes(app).is_some_and(|c| !c.is_empty());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if dialog == Dialog::Recovery {
                if widgets::pill_button(ui, "Recover", true).clicked() {
                    recover = Some(true);
                    close = true;
                }
                if widgets::pill_button(ui, "Discard", false).clicked() {
                    recover = Some(false);
                    close = true;
                }
            } else if dialog == Dialog::NumberPages {
                if widgets::pill_button(ui, "OK", true).clicked() {
                    apply_number = true;
                    close = true;
                }
                if widgets::pill_button(ui, "Cancel", false).clicked() {
                    close = true;
                }
            } else if dialog == Dialog::Split {
                if ui.add_enabled_ui(split_ready.is_some(), |ui| widgets::pill_button(ui, "Split", true)).inner.clicked() {
                    split_now = split_ready.clone();
                    close = true;
                }
                if widgets::pill_button(ui, "Cancel", false).clicked() {
                    close = true;
                }
            } else if changed {
                if widgets::pill_button(ui, "OK", true).clicked() {
                    apply = true;
                    close = true;
                }
                if widgets::pill_button(ui, "Cancel", false).clicked() {
                    close = true;
                }
            } else if widgets::pill_button(ui, "Close", true).clicked() {
                close = true;
            }
        });
    });
    if let Some(yes) = recover {
        let keys: Vec<String> = app.recoverable.iter().map(|m| m.key.clone()).collect();
        if yes {
            app.recover(&keys);
        } else {
            app.discard_recovered(&keys);
        }
    }
    if replace_now && let Some(d) = app.replace_draft.take() {
        let n = d.to - d.from + 1;
        app.apply_edit(Edit::ReplacePages {
            pages: (d.from - 1..d.to).collect(),
            name: d.name.clone(),
            bytes: d.bytes.clone(),
            src_pages: (d.src_from - 1..d.src_from - 1 + n).collect(),
        });
    }
    if print_go {
        app.print_now();
    }
    match redact_now {
        Some(Dialog::RedactPages) => app.redact_pages(),
        Some(Dialog::RedactSearch) => {
            let n = app.redact_search();
            app.redact_search.found = Some(n);
        }
        Some(Dialog::RemoveHidden) => {
            let which: Vec<printcraft_engine::Hidden> = app.hidden_draft.found.iter().filter(|f| f.2 && f.1 > 0).map(|f| f.0).collect();
            let n: usize = app.hidden_draft.found.iter().filter(|f| f.2).map(|f| f.1).sum();
            if app.apply_edit(Edit::RemoveHidden { which }) {
                app.notify(format!("Removed {n} hidden item{}. Save to remove them from the file.", if n == 1 { "" } else { "s" }));
            }
        }
        Some(Dialog::Sanitize) => {
            if app.apply_edit(Edit::Sanitize) {
                app.notify("Document sanitized. Save to finish: saving rewrites the whole file.");
            }
        }
        Some(Dialog::RedactApply) => {
            let marks = app.active_ids().and_then(|(_, id)| app.session.get(id)).map_or(0, |d| d.redaction_marks());
            if app.apply_edit(Edit::ApplyRedactions { pages: None }) {
                app.notify(format!("Applied {marks} redaction mark{}. Save to remove the content from the file.", if marks == 1 { "" } else { "s" }));
            }
        }
        _ => {}
    }
    if field_props_now
        && let Some(d) = app.field_props.take()
        && let Some(props) = d.props()
        && app.apply_edit(Edit::SetFieldProps { name: d.field.clone(), props })
        && let Some(i) = app.active
    {
        // Keep the (possibly renamed) field selected.
        let prefix = d.field.rsplit_once('.').map(|(p, _)| format!("{p}.")).unwrap_or_default();
        app.views[i].prepare.selected = Some((format!("{prefix}{}", d.name.trim()), d.widget));
    }
    if props_now && let Some(d) = app.comment_props.take() {
        let mut edits = crate::comment_props::edits(&d);
        match edits.len() {
            0 => {}
            1 => {
                app.apply_edit(edits.remove(0));
            }
            _ => {
                app.apply_edit(Edit::Batch { label: "Change comment properties".into(), edits });
            }
        }
    }
    if export_now && let Dialog::Export(kind) = dialog {
        app.start_export(kind);
    }
    if marks_now && let (Dialog::Marks(kind), Some((_, id))) = (dialog, app.active_ids()) {
        let count = app.session.get(id).map(|d| d.info.pages.len()).unwrap_or(0);
        let edit = crate::marks_ui::edit(&app.marks_draft, kind, count);
        app.apply_edit(edit);
    }
    if boxes_now && let Some((i, id)) = app.active_ids() {
        let count = app.session.get(id).map(|d| d.info.pages.len()).unwrap_or(0);
        let edit = app.boxes_draft.edit(app.views[i].current, count);
        app.apply_edit(edit);
        app.boxes_draft.seeded = None;
    }
    if protect_now && app.apply_edit(app.protect_draft.edit()) {
        app.protect_draft = Default::default();
        app.notify("Password protection will be applied when you save");
    }
    if apply_number && let Some(edit) = number_now {
        app.apply_edit(edit);
    }
    if let Some(by) = split_now {
        app.split_active(&by);
    }
    if apply && let Some(edits) = draft_changes(app) {
        app.apply_edit(Edit::Batch { label: "Change document properties".into(), edits });
    }
    // Protect / Remove security replace the Properties dialog.
    let replaces = link_command.is_some_and(|c| c.starts_with("protect."));
    if modal.should_close() || close || replaces {
        app.dialog = None;
        app.props_draft = None;
    } else {
        app.dialog = Some(next);
    }
    if let Some(cmd) = link_command {
        app.execute(cmd);
    }
}

/// Info edits needed to make the document match the Description draft.
fn draft_changes(app: &PrintCraftApp) -> Option<Vec<Edit>> {
    let (id, draft) = app.props_draft.as_ref()?;
    let doc = app.session.get(*id)?;
    Some(
        INFO_KEYS
            .iter()
            .zip(draft.iter())
            .filter(|(k, v)| doc.info_value(k).unwrap_or_default().trim() != v.trim())
            .map(|(k, v)| Edit::SetInfo { key: (*k).to_string(), value: v.clone() })
            .collect(),
    )
}

/// "Save changes?" when closing a tab or quitting with unsaved edits.
fn save_prompt(app: &mut PrintCraftApp, ctx: &egui::Context) {
    let Some(req) = app.close_request else { return };
    let index = match req {
        CloseRequest::Tab(i) => Some(i),
        CloseRequest::Quit => app.first_dirty(),
    };
    let Some(name) = index.and_then(|i| app.views.get(i)).and_then(|v| app.session.get(v.id)).map(|d| d.name.clone()) else {
        // Nothing left to ask about (tab already gone or no dirty documents).
        app.resolve_close(ctx, Some(false));
        return;
    };
    let t = Tokens::get(ctx);
    let mut choice: Option<Option<bool>> = None;
    let modal = egui::Modal::new(egui::Id::new("save_prompt")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.horizontal(|ui| {
            ui.add(crate::icons::image("save", 22.0, t.accent));
            ui.label(egui::RichText::new(format!("Save changes to “{name}” before closing?")).font(theme::semibold(16.0)));
        });
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Your changes will be lost if you don't save them.").color(t.text_muted));
        ui.add_space(14.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, "Save", true).clicked() {
                choice = Some(Some(true));
            }
            if widgets::pill_button(ui, "Cancel", false).clicked() {
                choice = Some(None);
            }
            ui.add_space(24.0);
            if widgets::pill_button(ui, "Don't save", false).clicked() {
                choice = Some(Some(false));
            }
        });
    });
    if choice.is_none() && modal.should_close() {
        choice = Some(None);
    }
    if let Some(c) = choice {
        app.resolve_close(ctx, c);
    }
}

fn yes(b: bool) -> String {
    if b { "Yes".into() } else { "No".into() }
}

/// Password prompt for encrypted documents (Acrobat: "Password" dialog on open).
fn password(app: &mut PrintCraftApp, ctx: &egui::Context) {
    let Some(prompt) = app.password_prompt.as_mut() else { return };
    let t = Tokens::get(ctx);
    let mut submit = false;
    let mut cancel = false;
    let modal = egui::Modal::new(egui::Id::new("password")).show(ctx, |ui| {
        ui.set_width(400.0);
        ui.horizontal(|ui| {
            ui.add(crate::icons::image("lock", 22.0, t.accent));
            ui.label(egui::RichText::new("Password required").font(theme::semibold(17.0)));
        });
        ui.add_space(6.0);
        ui.label(format!("“{}” is protected. Enter a password to open it.", prompt.name));
        ui.add_space(8.0);
        let r = ui.add(egui::TextEdit::singleline(&mut prompt.input).password(true).hint_text("Password").desired_width(f32::INFINITY));
        // Enter submits. The field keeps focus (we request it every frame), so check the key
        // while it is focused as well as on the frame focus is lost.
        if (r.has_focus() || r.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            submit = true;
        }
        r.request_focus();
        if let Some(e) = &prompt.error {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(e).color(egui::Color32::from_rgb(0xD1, 0x3B, 0x3B)));
        }
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, "Open", true).clicked() {
                submit = true;
            }
            if widgets::pill_button(ui, "Cancel", false).clicked() {
                cancel = true;
            }
        });
    });
    if submit {
        let pw = prompt.input.clone();
        app.submit_password(Some(pw));
    } else if cancel || modal.should_close() {
        app.submit_password(None);
    }
}

//! Redact a PDF (execution plan M8.5–M8.6): mark text and areas with the Redact tool (drag across
//! text to mark it, drag elsewhere to mark a box), mark whole pages, find text or patterns and
//! mark every match (patterns include locale-specific formats), set the default box colour and
//! overlay text (custom or a redaction code), and apply all marks (with a confirmation, as Acrobat
//! asks) or clear them.

use egui::{Color32, CornerRadius, Pos2, Rect, Stroke};
use pdfcraft_engine::{Edit, NewAnnotation, REDACT_PATTERNS, RedactPattern, Rgb, Shape, Style, rect_quad};
use pdfcraft_render::DocInfo;

use crate::canvas::{DocView, PageXform};
use crate::theme::Tokens;
use crate::{PdfCraftApp, widgets};

const MARK_RED: Color32 = Color32::from_rgb(0xE3, 0x22, 0x22);

/// Redaction Tool Properties: what new marks look like once applied.
#[derive(Clone, Debug, PartialEq)]
pub struct RedactPrefs {
    /// Box colour (`None` = no box, the content just disappears).
    pub fill: Option<Rgb>,
    pub use_overlay: bool,
    pub overlay: String,
    /// Overlay text font, size (0 = auto), colour, alignment and repetition.
    pub look: pdfcraft_engine::OverlayLook,
    /// The redaction code set offered: an index into `REDACTION_CODE_SETS`, then the user's sets.
    pub code_set: usize,
}

impl Default for RedactPrefs {
    fn default() -> Self {
        Self { fill: Some([0.0, 0.0, 0.0]), use_overlay: false, overlay: String::new(), look: Default::default(), code_set: 0 }
    }
}

impl RedactPrefs {
    pub fn mark(&self, page: usize, quads: Vec<[f64; 8]>, author: &str) -> Edit {
        let shape = Shape::Redact { quads, overlay: if self.use_overlay { self.overlay.clone() } else { String::new() }, look: self.look };
        let mut style = Style::default_for(&shape);
        style.fill = self.fill;
        Edit::AddAnnotation(NewAnnotation { page, shape, style, contents: String::new(), author: author.to_string() })
    }
}

/// Redact pages: which pages to mark.
#[derive(Clone, Debug, PartialEq)]
pub struct PagesDraft {
    pub current: bool,
    pub from: usize,
    pub to: usize,
}

impl Default for PagesDraft {
    fn default() -> Self {
        Self { current: true, from: 1, to: 1 }
    }
}

/// Find text and redact.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchDraft {
    pub mode: SearchMode,
    pub text: String,
    /// Multiple words or phrases, one per line.
    pub list: String,
    /// For the list: match upper and lower case exactly, and only whole words.
    pub case_sensitive: bool,
    pub whole_words: bool,
    pub pattern: RedactPattern,
    /// The locale whose patterns are offered (`"all"`: every locale).
    pub locale: &'static str,
    /// Mark every pattern of [`Self::locale`] (and the international ones) at once.
    pub every_pattern: bool,
    /// The result of the last search (matches marked), shown in the dialog.
    pub found: Option<usize>,
    /// Why the list couldn't be searched, shown in the dialog.
    pub error: Option<String>,
    /// Import list… was pressed (handled by the app, which picks the file).
    pub import: bool,
}

/// What Find text and redact looks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    Phrase,
    Words,
    Patterns,
}

impl Default for SearchDraft {
    fn default() -> Self {
        Self {
            mode: SearchMode::Phrase,
            text: String::new(),
            list: String::new(),
            case_sensitive: false,
            whole_words: false,
            pattern: RedactPattern::Phone,
            locale: "all",
            every_pattern: false,
            found: None,
            error: None,
            import: false,
        }
    }
}

/// A box being drawn with the Redact tool: (page, start).
pub type AreaDrag = Option<(usize, Pos2)>;

fn to_user(xf: &PageXform, info: &DocInfo, page: usize, p: Pos2) -> [f64; 2] {
    let (vx, vy) = xf.screen_to_view(p);
    let u = info.pages[page].view_to_user(vx, vy);
    [u[0] as f64, u[1] as f64]
}

/// Redact tool input on a page. Presses on text are left to text selection (the selection is
/// marked when it ends, see [`after_text`]); presses elsewhere draw a box. Returns `true` when
/// the gesture is a box.
pub(crate) fn page_input(
    ui: &egui::Ui,
    resp: &egui::Response,
    xf: &PageXform,
    page: usize,
    info: &DocInfo,
    over_text: impl Fn(Pos2) -> bool,
    view: &mut DocView,
) -> bool {
    let pointer = ui.input(|i| i.pointer.hover_pos().or(i.pointer.interact_pos()));
    if let Some((dp, start)) = view.redact_drag
        && dp == page
    {
        if resp.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
            view.redact_drag = None;
            let end = pointer.unwrap_or(start);
            let r = Rect::from_two_pos(start, end).intersect(xf.rect);
            if r.width() >= 3.0 && r.height() >= 3.0 {
                let (a, b) = (to_user(xf, info, page, r.min), to_user(xf, info, page, r.max));
                view.pending_redaction = Some((page, vec![rect_quad([a[0], a[1], b[0], b[1]])]));
            }
        }
        return true;
    }
    let Some(p) = pointer.filter(|p| xf.rect.contains(*p)) else { return false };
    if !over_text(p) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if resp.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(p);
        if !over_text(origin) {
            view.redact_drag = Some((page, origin));
            return true;
        }
    }
    false
}

/// A finished text selection with the Redact tool becomes a mark.
pub(crate) fn after_text(resp: &egui::Response, page: usize, info: &DocInfo, view: &mut DocView) {
    if !(resp.drag_stopped() || resp.double_clicked()) || view.redact_drag.is_some() {
        return;
    }
    if let Some((p, quads)) = view.selection_quads(info).filter(|(p, _)| *p == page) {
        view.clear_selection();
        view.pending_redaction = Some((p, quads));
    }
}

/// The box being drawn.
pub(crate) fn paint(ui: &egui::Ui, painter: &egui::Painter, page: usize, view: &DocView) {
    if let (Some((dp, start)), Some(p)) = (view.redact_drag, ui.input(|i| i.pointer.hover_pos()))
        && dp == page
    {
        let r = Rect::from_two_pos(start, p);
        painter.rect_filled(r, CornerRadius::ZERO, MARK_RED.gamma_multiply(0.12));
        painter.rect_stroke(r, CornerRadius::ZERO, Stroke::new(1.5, MARK_RED), egui::StrokeKind::Inside);
    }
}

impl PdfCraftApp {
    /// Mark every match of the search draft on every page; returns how many were marked.
    pub fn redact_search(&mut self) -> usize {
        let Some((_, id)) = self.active_ids() else { return 0 };
        let Some(doc) = self.session.get(id) else { return 0 };
        let d = self.redact_search.clone();
        if d.mode == SearchMode::Phrase && d.text.trim().is_empty() {
            return 0;
        }
        self.redact_search.error = None;
        let words = match d.mode {
            SearchMode::Words => match pdfcraft_engine::RedactWordList::parse(&d.list, d.case_sensitive, d.whole_words) {
                Ok(w) => Some(w),
                Err(pdfcraft_engine::RedactWordListError::Empty) => return 0,
                Err(_) => {
                    self.redact_search.error = Some(crate::i18n::fmt(
                        tl!("The list is too long: at most {n} entries of up to {m} characters each."),
                        &[("n", &pdfcraft_engine::MAX_REDACT_WORDS.to_string()), ("m", &pdfcraft_engine::MAX_REDACT_WORD_CHARS.to_string())],
                    ));
                    return 0;
                }
            },
            _ => None,
        };
        let config = pdfcraft_render::RenderConfig { password: doc.password.as_deref().map(std::sync::Arc::from), ..Default::default() };
        let mut r = pdfcraft_render::PageRenderer::new(doc.bytes.clone(), config);
        let mut edits = Vec::new();
        let author = self.comment_prefs.author.clone();
        for page in 0..doc.info.pages.len() {
            let out = r.render(pdfcraft_render::RenderRequest { page, kind: pdfcraft_render::RequestKind::Text, scale: 1.0, ..Default::default() });
            let Some(text) = out.text else { continue };
            let hits = match (d.mode, &words) {
                (SearchMode::Patterns, _) => match pdfcraft_engine::redact_patterns_for(d.locale).filter(|_| d.every_pattern) {
                    Some(patterns) => text.find_with(|c| {
                        let mut all: Vec<std::ops::Range<usize>> = patterns.iter().flat_map(|p| pdfcraft_engine::find_pattern(*p, c)).collect();
                        all.sort_by_key(|r| (r.start, r.end));
                        all.dedup();
                        all
                    }),
                    None => text.find_with(|c| pdfcraft_engine::find_pattern(d.pattern, c)),
                },
                (SearchMode::Words, Some(w)) => text.find_with(|c| w.find(c)),
                _ => text.find(&d.text),
            };
            for h in hits {
                let quads: Vec<[f64; 8]> = text.line_rects(h).into_iter().map(|r| doc.info.pages[page].view_rect_to_quad(r)).collect();
                if !quads.is_empty() {
                    edits.push(self.redact_prefs.mark(page, quads, &author));
                }
            }
        }
        let n = edits.len();
        if n > 0 {
            self.apply_edit(Edit::Batch { label: "Mark for redaction".into(), edits });
        }
        n
    }

    /// Import list…: read a text file (one word or phrase per line) into the list.
    pub(crate) fn import_redact_words(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let dialog = rfd::AsyncFileDialog::new().set_title(tl!("Import list…")).add_filter("Text", &["txt", "csv"]);
            self.ask_one(crate::pickers::Ask::File(dialog), None, |app, p| {
                let read = std::fs::metadata(&p).and_then(|m| {
                    if m.len() > pdfcraft_engine::MAX_REDACT_LIST_BYTES as u64 {
                        Err(std::io::Error::other("larger than 1 MiB"))
                    } else {
                        std::fs::read(&p)
                    }
                });
                match read {
                    Ok(bytes) => {
                        let d = &mut app.redact_search;
                        d.list = String::from_utf8_lossy(&bytes).trim_start_matches('\u{FEFF}').to_string();
                        d.mode = SearchMode::Words;
                        d.found = None;
                        d.error = None;
                    }
                    Err(e) => app.notify_fmt("Couldn't read {name}: {e}", &[("name", &p.display().to_string()), ("e", &e.to_string())]),
                }
            });
        }
    }

    /// Mark the pages of the Redact pages draft.
    pub fn redact_pages(&mut self) {
        let Some((i, id)) = self.active_ids() else { return };
        let Some(doc) = self.session.get(id) else { return };
        let n = doc.info.pages.len();
        let d = self.redact_pages_draft.clone();
        let pages: Vec<usize> = if d.current { vec![self.views[i].current] } else { (d.from.max(1) - 1..d.to.min(n)).collect() };
        let author = self.comment_prefs.author.clone();
        let edits: Vec<Edit> = pages
            .iter()
            .map(|&p| {
                let c = doc.info.pages[p].crop;
                self.redact_prefs.mark(p, vec![rect_quad([c[0] as f64, c[1] as f64, c[2] as f64, c[3] as f64])], &author)
            })
            .collect();
        match <[Edit; 1]>::try_from(edits) {
            Ok([one]) => {
                self.apply_edit(one);
            }
            Err(edits) if edits.is_empty() => {}
            Err(edits) => {
                self.apply_edit(Edit::Batch { label: "Mark pages for redaction".into(), edits });
            }
        }
    }
}

/// Redact pages ("Mark Page Range"). Returns (apply, cancel).
pub(crate) fn pages_body(ui: &mut egui::Ui, d: &mut PagesDraft, pages: usize, _t: &Tokens) -> (bool, bool) {
    ui.label(egui::RichText::new(tl!("Mark Page Range")).font(crate::theme::semibold(18.0)));
    ui.add_space(8.0);
    ui.radio_value(&mut d.current, true, tl!("Current page"));
    ui.horizontal(|ui| {
        ui.radio_value(&mut d.current, false, tl!("Pages from"));
        ui.add_enabled(!d.current, egui::DragValue::new(&mut d.from).range(1..=pages));
        ui.label(tl!("to"));
        ui.add_enabled(!d.current, egui::DragValue::new(&mut d.to).range(1..=pages));
        ui.label(crate::i18n::fmt(tl!("of {name}"), &[("name", &pages.to_string())]));
    });
    d.to = d.to.max(d.from);
    ui.add_space(12.0);
    buttons(ui, "OK", true)
}

/// Find text and redact. Returns (search, cancel).
pub(crate) fn search_body(ui: &mut egui::Ui, d: &mut SearchDraft, t: &Tokens) -> (bool, bool) {
    ui.set_width(420.0);
    ui.label(egui::RichText::new(tl!("Find text and redact")).font(crate::theme::semibold(18.0)));
    ui.add_space(8.0);
    ui.radio_value(&mut d.mode, SearchMode::Phrase, tl!("Single word or phrase"));
    let mut enter = false;
    ui.add_enabled_ui(d.mode == SearchMode::Phrase, |ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut d.text).hint_text(tl!("Text to find")).desired_width(380.0));
        enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    });
    ui.add_space(6.0);
    ui.radio_value(&mut d.mode, SearchMode::Words, tl!("Multiple words or phrases"));
    ui.add_enabled_ui(d.mode == SearchMode::Words, |ui| {
        // A long imported list scrolls rather than pushing the buttons off the dialog.
        egui::ScrollArea::vertical().id_salt("redact-word-list").max_height(120.0).show(ui, |ui| {
            ui.add(egui::TextEdit::multiline(&mut d.list).hint_text(tl!("One word or phrase per line")).desired_rows(4).desired_width(370.0));
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut d.case_sensitive, tl!("Case-sensitive"));
            ui.checkbox(&mut d.whole_words, tl!("Whole words only"));
            #[cfg(not(target_arch = "wasm32"))]
            if ui.button(tl!("Import list…")).clicked() {
                d.import = true;
            }
        });
    });
    ui.add_space(6.0);
    ui.radio_value(&mut d.mode, SearchMode::Patterns, tl!("Patterns"));
    ui.add_enabled_ui(d.mode == SearchMode::Patterns, |ui| {
        ui.horizontal_wrapped(|ui| {
            let locale_name = |id: &str| -> &'static str {
                pdfcraft_engine::REDACT_PATTERN_LOCALES.iter().find(|(l, _)| *l == id).map_or(tl!("All locales"), |(_, n)| tl!(n))
            };
            egui::ComboBox::from_id_salt("redact-locale").selected_text(locale_name(d.locale)).width(160.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut d.locale, "all", tl!("All locales"));
                for (id, name) in pdfcraft_engine::REDACT_PATTERN_LOCALES {
                    ui.selectable_value(&mut d.locale, id, tl!(name));
                }
            });
            let offered: Vec<RedactPattern> =
                REDACT_PATTERNS.into_iter().filter(|p| d.locale == "all" || p.locale() == "intl" || p.locale() == d.locale).collect();
            if !offered.contains(&d.pattern)
                && let Some(p) = offered.first()
            {
                d.pattern = *p;
            }
            ui.add_enabled_ui(!d.every_pattern || d.locale == "all", |ui| {
                egui::ComboBox::from_id_salt("redact-pattern").selected_text(tl!(d.pattern.label())).width(240.0).show_ui(ui, |ui| {
                    for p in offered {
                        ui.selectable_value(&mut d.pattern, p, tl!(p.label()));
                    }
                });
            });
        });
        ui.add_enabled_ui(d.locale != "all", |ui| {
            ui.checkbox(&mut d.every_pattern, tl!("Mark every pattern of this locale"));
        });
    });
    ui.add_space(6.0);
    ui.label(egui::RichText::new(tl!("Text in images isn't found; recognise text (OCR) first.")).small().color(t.text_faint));
    if let Some(e) = &d.error {
        ui.label(egui::RichText::new(e.as_str()).color(Color32::from_rgb(0xC8, 0x3A, 0x3A)));
    } else if let Some(n) = d.found {
        ui.label(
            egui::RichText::new(if n == 0 {
                tl!("No matches.").to_string()
            } else {
                crate::i18n::fmt(tl!("{n} match(es) marked for redaction."), &[("n", &n.to_string())])
            })
            .color(t.text_muted),
        );
    }
    ui.add_space(12.0);
    let (go, cancel) = buttons(ui, "Mark all", true);
    (go || enter, cancel)
}

/// Redaction Tool Properties. Returns (apply, cancel).
/// The codes in an overlay text ("A, B" → ["A", "B"]).
pub fn picked_codes(overlay: &str) -> Vec<String> {
    overlay.split(',').map(str::trim).filter(|c| !c.is_empty()).map(str::to_string).collect()
}

/// `overlay` with `code` added at the end, or taken out when it is there already; codes are
/// separated by ", " and the text is kept within the overlay limit.
pub fn toggle_code(overlay: &str, code: &str) -> String {
    let mut codes = picked_codes(overlay);
    if let Some(i) = codes.iter().position(|c| c == code) {
        codes.remove(i);
    } else {
        codes.push(code.trim().to_string());
    }
    let text = codes.join(", ");
    if text.chars().count() > pdfcraft_engine::redaction_codes::MAX_OVERLAY_CHARS { overlay.to_string() } else { text }
}

/// Redaction Tool Properties; `custom` are the user's code sets.
pub(crate) fn props_body(
    ui: &mut egui::Ui,
    d: &mut RedactPrefs,
    custom: &[pdfcraft_engine::redaction_codes::CustomCodeSet],
    _t: &Tokens,
) -> (bool, bool) {
    ui.set_width(380.0);
    ui.label(egui::RichText::new(tl!("Redaction Tool Properties")).font(crate::theme::semibold(18.0)));
    ui.add_space(8.0);
    egui::Grid::new("redact-props").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
        ui.label(tl!("Redacted area fill colour:"));
        ui.horizontal(|ui| {
            let mut none = d.fill.is_none();
            if ui.checkbox(&mut none, tl!("No colour")).changed() {
                d.fill = if none { None } else { Some([0.0, 0.0, 0.0]) };
            }
        });
        ui.end_row();
        ui.label("");
        if let Some(c) = crate::comments::swatch_grid(ui, d.fill) {
            d.fill = Some(c);
        }
        ui.end_row();
        ui.label("");
        ui.checkbox(&mut d.use_overlay, tl!("Use overlay text"));
        ui.end_row();
        let l = ui.label(tl!("Custom text:"));
        ui.add_enabled(d.use_overlay, egui::TextEdit::singleline(&mut d.overlay).desired_width(220.0)).labelled_by(l.id);
        ui.end_row();
        // Redaction codes: each code picked joins the overlay text ("(b)(6), (b)(7)(C)"), and
        // picking it again takes it out. The user's own sets follow the built-in ones.
        ui.label(tl!("Redaction code:"));
        ui.add_enabled_ui(d.use_overlay, |ui| {
            ui.horizontal(|ui| {
                let builtin = pdfcraft_engine::REDACTION_CODE_SETS;
                let count = builtin.len().saturating_add(custom.len());
                if d.code_set >= count {
                    d.code_set = 0;
                }
                let name = |i: usize| -> String {
                    match builtin.get(i) {
                        Some(s) => s.name.to_string(),
                        None => custom.get(i.saturating_sub(builtin.len())).map(|s| s.name.clone()).unwrap_or_default(),
                    }
                };
                egui::ComboBox::from_id_salt("redact-code-set").selected_text(name(d.code_set)).show_ui(ui, |ui| {
                    for i in 0..count {
                        ui.selectable_value(&mut d.code_set, i, name(i));
                    }
                });
                let codes: Vec<(String, String)> = match builtin.get(d.code_set) {
                    Some(s) => s.codes.iter().map(|c| (c.code.to_string(), c.description.to_string())).collect(),
                    None => custom
                        .get(d.code_set.saturating_sub(builtin.len()))
                        .map(|s| s.codes.iter().map(|c| (c.code.clone(), c.description.clone())).collect())
                        .unwrap_or_default(),
                };
                let picked = picked_codes(&d.overlay);
                let shown = if picked.is_empty() {
                    tl!("Choose codes").to_string()
                } else {
                    crate::i18n::fmt(tl!("{n} chosen"), &[("n", &picked.len().to_string())])
                };
                egui::ComboBox::from_id_salt("redact-code").selected_text(shown).show_ui(ui, |ui| {
                    for (code, description) in &codes {
                        let on = picked.iter().any(|p| p == code);
                        let r = ui.selectable_label(on, code.as_str());
                        let r = if description.is_empty() { r } else { r.on_hover_text(description.as_str()) };
                        if r.clicked() {
                            d.overlay = toggle_code(&d.overlay, code);
                        }
                    }
                });
            });
        });
        ui.end_row();
        let on = d.use_overlay;
        let look = &mut d.look;
        ui.label(tl!("Font:"));
        ui.add_enabled_ui(on, |ui| {
            egui::ComboBox::from_id_salt("overlay-font").selected_text(look.font.name()).show_ui(ui, |ui| {
                for f in pdfcraft_engine::OverlayFont::ALL {
                    ui.selectable_value(&mut look.font, f, f.name());
                }
            });
        });
        ui.end_row();
        ui.label(tl!("Font size:"));
        ui.add_enabled_ui(on, |ui| {
            ui.horizontal(|ui| {
                let mut auto = look.size <= 0.0;
                if ui.checkbox(&mut auto, tl!("Auto-size text to fit redaction region")).changed() {
                    look.size = if auto { 0.0 } else { 10.0 };
                }
                if !auto {
                    ui.add(egui::DragValue::new(&mut look.size).range(2.0..=144.0).suffix(" pt"));
                }
            });
        });
        ui.end_row();
        ui.label(tl!("Font colour:"));
        ui.add_enabled_ui(on, |ui| {
            if let Some(c) = crate::comments::swatch_grid(ui, Some(look.color)) {
                look.color = c;
            }
        });
        ui.end_row();
        ui.label("");
        ui.add_enabled(on, egui::Checkbox::new(&mut look.repeat, tl!("Repeat overlay text")));
        ui.end_row();
        ui.label(tl!("Text alignment:"));
        ui.add_enabled_ui(on, |ui| {
            ui.horizontal(|ui| {
                for (a, label) in [(0u8, tl!("Left")), (1, tl!("Center")), (2, tl!("Right"))] {
                    ui.radio_value(&mut look.align, a, label);
                }
            });
        });
        ui.end_row();
    });
    ui.add_space(12.0);
    buttons(ui, "OK", true)
}

/// Apply redactions confirmation. Returns (apply, cancel).
pub(crate) fn apply_body(ui: &mut egui::Ui, marks: usize, t: &Tokens) -> (bool, bool) {
    ui.set_width(420.0);
    ui.label(egui::RichText::new(tl!("Apply redactions")).font(crate::theme::semibold(18.0)));
    ui.add_space(8.0);
    ui.label(if marks == 1 {
        tl!("You are about to apply 1 redaction mark. Text, images and drawings under the marks, and comments and form fields that overlap them, are removed permanently.").to_string()
    } else {
        crate::i18n::fmt(tl!("You are about to apply {n} redaction marks. Text, images and drawings under the marks, and comments and form fields that overlap them, are removed permanently."), &[("n", &marks.to_string())])
    });
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(tl!("Save the document afterwards: saving rewrites the whole file so no trace of the removed content stays in it."))
            .small()
            .color(t.text_muted),
    );
    ui.add_space(12.0);
    buttons(ui, "Apply", true)
}

fn buttons(ui: &mut egui::Ui, ok: &str, primary: bool) -> (bool, bool) {
    let (mut a, mut c) = (false, false);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        // `ok` stays English internally; only the display is translated.
        if widgets::pill_button(ui, tl!(ok), primary).clicked() {
            a = true;
        }
        if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
            c = true;
        }
    });
    (a, c)
}

/// Remove Hidden Information: each category with what was found, checked by default when
/// something was.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HiddenDraft {
    pub found: Vec<(pdfcraft_engine::Hidden, usize, bool)>,
}

impl PdfCraftApp {
    pub fn open_remove_hidden(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        let Some(doc) = self.session.get(id) else { return };
        self.hidden_draft = HiddenDraft { found: doc.hidden_info().into_iter().map(|(h, n)| (h, n, n > 0)).collect() };
        self.dialog = Some(crate::Dialog::RemoveHidden);
    }
}

/// Returns (remove, cancel).
pub(crate) fn hidden_body(ui: &mut egui::Ui, d: &mut HiddenDraft, t: &Tokens) -> (bool, bool) {
    ui.set_width(440.0);
    ui.label(egui::RichText::new(tl!("Remove hidden information")).font(crate::theme::semibold(18.0)));
    ui.add_space(4.0);
    ui.label(egui::RichText::new(tl!("Select the items to remove from this document.")).color(t.text_muted));
    ui.add_space(8.0);
    let total: usize = d.found.iter().map(|f| f.1).sum();
    for (h, n, on) in d.found.iter_mut() {
        ui.add_enabled_ui(*n > 0, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(on, tl!(h.label()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(if *n == 0 { tl!("None found").to_string() } else { n.to_string() }).color(t.text_muted));
                });
            });
        });
    }
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(tl!("Form fields are flattened: their values stay visible. Saving rewrites the whole file.")).small().color(t.text_faint),
    );
    ui.add_space(12.0);
    let any = d.found.iter().any(|f| f.2 && f.1 > 0);
    let (mut a, mut c) = (false, false);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.add_enabled_ui(any && total > 0, |ui| widgets::pill_button(ui, tl!("Remove"), true)).inner.clicked() {
            a = true;
        }
        if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
            c = true;
        }
    });
    (a, c)
}

/// Sanitize Document confirmation. Returns (sanitize, cancel).
pub(crate) fn sanitize_body(ui: &mut egui::Ui, t: &Tokens) -> (bool, bool) {
    ui.set_width(440.0);
    ui.label(egui::RichText::new(tl!("Sanitize document")).font(crate::theme::semibold(18.0)));
    ui.add_space(8.0);
    ui.label(tl!("Sanitizing removes hidden information from the document: metadata, file attachments, comments, form fields (flattened), hidden text and layers, bookmarks, links, actions and scripts, and private application data."));
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(tl!("Save the document afterwards; saving rewrites the whole file so nothing removed stays in it."))
            .small()
            .color(t.text_muted),
    );
    ui.add_space(12.0);
    buttons(ui, "Sanitize", true)
}

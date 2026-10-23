//! Add Header and Footer, Add Watermark and Add Background dialogs (Acrobat: Edit a PDF ▸ …;
//! dialog snapshots d13, d14, d12d). Each has the shared Page Range Options and a schematic
//! preview of the current page. "Update" opens the same dialog in replace mode.

use egui::{Align, Color32, CornerRadius, Layout, Rect, Stroke, pos2, vec2};
use pdfcraft_engine::{Background, Edit, HeaderFooter, MarkKind, Watermark};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subset {
    All,
    Even,
    Odd,
}

/// Page Range Options (shared by the three dialogs). Pages are 1-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRange {
    pub all: bool,
    pub from: usize,
    pub to: usize,
    pub subset: Subset,
}

impl Default for PageRange {
    fn default() -> Self {
        Self { all: true, from: 1, to: 1, subset: Subset::All }
    }
}

impl PageRange {
    /// The pages (0-based) the range selects in a document of `count` pages.
    pub fn pages(&self, count: usize) -> Vec<usize> {
        let (a, b) = if self.all { (1, count) } else { (self.from.max(1), self.to.min(count)) };
        (a..=b)
            .filter(|p| match self.subset {
                Subset::All => true,
                Subset::Even => p % 2 == 0,
                Subset::Odd => p % 2 == 1,
            })
            .map(|p| p - 1)
            .collect()
    }

    pub(crate) fn ui(&mut self, ui: &mut egui::Ui, count: usize) {
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.all, true, tl!("All pages"));
            ui.radio_value(&mut self.all, false, tl!("Pages from"));
            ui.add_enabled(!self.all, egui::DragValue::new(&mut self.from).range(1..=count.max(1)));
            ui.label(tl!("to"));
            if self.to == 1 && self.from == 1 {
                self.to = count.max(1);
            }
            ui.add_enabled(!self.all, egui::DragValue::new(&mut self.to).range(1..=count.max(1)));
            ui.label(tl!("Subset"));
            let label = match self.subset {
                Subset::All => tl!("All pages in range"),
                Subset::Even => tl!("Even pages only"),
                Subset::Odd => tl!("Odd pages only"),
            };
            egui::ComboBox::from_id_salt("mark-subset").selected_text(label).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.subset, Subset::All, tl!("All pages in range"));
                ui.selectable_value(&mut self.subset, Subset::Even, tl!("Even pages only"));
                ui.selectable_value(&mut self.subset, Subset::Odd, tl!("Odd pages only"));
            });
        });
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MarksDraft {
    pub hf: HeaderFooter,
    pub wm: Watermark,
    pub bg: Background,
    pub range: PageRange,
    /// Replace existing marks of this kind (Update).
    pub replace: bool,
    /// Which header/footer box Insert Page Number / Insert Date type into.
    pub focused_box: usize,
    pub page_format: usize,
    pub date_format: usize,
    /// Watermark: size the text to the page.
    pub fit: bool,
    /// Source ▸ File (watermark or background): the file picked, and the page of a PDF (1-based).
    pub use_file: bool,
    pub file: Option<(String, std::sync::Arc<Vec<u8>>)>,
    pub file_page: usize,
    /// Bates numbering: other files the numbers continue into, in order, and the suffix their
    /// numbered copies get.
    pub bates_files: Vec<(String, std::sync::Arc<Vec<u8>>)>,
    pub bates_suffix: String,
}

impl Default for MarksDraft {
    fn default() -> Self {
        Self {
            hf: HeaderFooter::default(),
            wm: Watermark::default(),
            bg: Background::default(),
            range: PageRange::default(),
            replace: false,
            focused_box: 1,
            page_format: 0,
            date_format: 0,
            fit: true,
            use_file: false,
            file: None,
            file_page: 1,
            bates_files: Vec::new(),
            bates_suffix: "_bates".into(),
        }
    }
}

/// A Bates number token's options: digits, start, prefix and suffix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatesOptions {
    pub digits: usize,
    pub start: u64,
    pub prefix: String,
    pub suffix: String,
}

/// The longest prefix or suffix a Bates number takes in the dialog.
const MAX_BATES_AFFIX: usize = 40;

/// The options of the first `<<Bates Number#digits#start#prefix#suffix>>` token in `text`.
pub fn bates_options(text: &str) -> Option<BatesOptions> {
    let at = text.find("<<Bates Number")?;
    let rest = text.get(at + 2..)?;
    let token = rest.get(..rest.find(">>")?)?;
    let parts: Vec<&str> = token.split('#').collect();
    Some(BatesOptions {
        digits: parts.get(1).and_then(|d| d.parse::<usize>().ok()).unwrap_or(6).clamp(1, 15),
        start: parts.get(2).and_then(|d| d.parse::<u64>().ok()).unwrap_or(1),
        prefix: parts.get(3).map(|s| s.to_string()).unwrap_or_default(),
        suffix: parts.get(4).map(|s| s.to_string()).unwrap_or_default(),
    })
}

/// The Bates token for `o`. `#`, `<` and `>` would end the token early, so they are left out.
pub fn bates_token(o: &BatesOptions) -> String {
    let clean = |s: &str| s.chars().filter(|c| !matches!(c, '#' | '<' | '>') && !c.is_control()).take(MAX_BATES_AFFIX).collect::<String>();
    format!("<<Bates Number#{}#{}#{}#{}>>", o.digits.clamp(1, 15), o.start, clean(&o.prefix), clean(&o.suffix))
}

/// `text` with every Bates token replaced by the token for `o`.
fn with_bates_options(text: &str, o: &BatesOptions) -> String {
    let token = bates_token(o);
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("<<Bates Number") {
        let Some(end) = rest.get(i..).and_then(|r| r.find(">>")) else { break };
        out.push_str(rest.get(..i).unwrap_or_default());
        out.push_str(&token);
        rest = rest.get(i + end + 2..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

const PAGE_FORMATS: [&str; 5] = ["<<1>>", "<<1 of n>>", "<<1/n>>", "<<Page 1>>", "<<Page 1 of n>>"];
const DATE_FORMATS: [&str; 6] = ["<<m/d/yyyy>>", "<<mm/dd/yyyy>>", "<<d/m/yyyy>>", "<<yyyy-mm-dd>>", "<<m/d/yy>>", "<<mmmm d, yyyy>>"];
const BOX_NAMES: [&str; 6] =
    ["Left Header Text", "Center Header Text", "Right Header Text", "Left Footer Text", "Center Footer Text", "Right Footer Text"];

fn color_button(ui: &mut egui::Ui, c: &mut [f64; 3]) {
    let mut f = c.map(|v| v as f32);
    if egui::color_picker::color_edit_button_rgb(ui, &mut f).changed() {
        *c = f.map(f64::from);
    }
}

/// A schematic of the current page with the marks laid out (no rendering needed).
fn preview(ui: &mut egui::Ui, t: &Tokens, size: (f64, f64), draw: impl FnOnce(&egui::Painter, Rect, f32)) {
    let (w, h) = (size.0.max(1.0), size.1.max(1.0));
    let s = (190.0 / w.max(h)) as f32;
    let (r, _) = ui.allocate_exact_size(vec2(210.0, 210.0), egui::Sense::hover());
    let page = Rect::from_center_size(r.center(), vec2(w as f32 * s, h as f32 * s));
    let painter = ui.painter_at(r);
    painter.rect_filled(page, CornerRadius::ZERO, Color32::WHITE);
    painter.rect_stroke(page, CornerRadius::ZERO, Stroke::new(1.0, t.border), egui::StrokeKind::Outside);
    draw(&painter, page, s);
}

fn rgb32(c: [f64; 3]) -> Color32 {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(b(c[0]), b(c[1]), b(c[2]))
}

/// The dialog for `kind`; returns (apply, cancel).
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens, kind: MarkKind) -> (bool, bool) {
    let Some((i, id)) = app.active_ids() else { return (false, true) };
    let current = app.views[i].current;
    let Some(doc) = app.session.get(id) else { return (false, true) };
    let count = doc.info.pages.len();
    let page = doc.info.pages.get(current).map(|p| (p.width as f64, p.height as f64)).unwrap_or((612.0, 792.0));
    let d = &mut app.marks_draft;
    let mut browse = false;
    let mut browse_bates = false;
    let verb = if d.replace { tl!("Update") } else { tl!("Add") };
    let title = match kind {
        MarkKind::HeaderFooter | MarkKind::Bates => crate::i18n::fmt(tl!("{verb} Header and Footer"), &[("verb", verb)]),
        MarkKind::Watermark => crate::i18n::fmt(tl!("{verb} Watermark"), &[("verb", verb)]),
        MarkKind::Background => crate::i18n::fmt(tl!("{verb} Background"), &[("verb", verb)]),
    };
    ui.label(egui::RichText::new(title).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    match kind {
        MarkKind::HeaderFooter | MarkKind::Bates => {
            ui.horizontal(|ui| {
                ui.label(tl!("Font: Helvetica"));
                ui.label(tl!("Size"));
                ui.add(egui::DragValue::new(&mut d.hf.font_size).range(4.0..=72.0).speed(0.5));
                ui.checkbox(&mut d.hf.underline, tl!("Underline"));
                color_button(ui, &mut d.hf.color);
                ui.add_space(16.0);
                ui.label(tl!("Margins (in)"));
                for (label, k) in [(tl!("Top"), 0), (tl!("Bottom"), 1), (tl!("Left"), 2), (tl!("Right"), 3)] {
                    ui.label(label);
                    let mut v = d.hf.margins[k] / 72.0;
                    if ui.add(egui::DragValue::new(&mut v).range(0.0..=10.0).speed(0.05).max_decimals(2)).changed() {
                        d.hf.margins[k] = v * 72.0;
                    }
                }
            });
            ui.add_space(8.0);
            egui::Grid::new("hf-boxes").num_columns(3).spacing([10.0, 4.0]).show(ui, |ui| {
                for row in 0..2 {
                    for col in 0..3 {
                        ui.label(egui::RichText::new(tl!(BOX_NAMES[row * 3 + col])).small());
                    }
                    ui.end_row();
                    for col in 0..3 {
                        let k = row * 3 + col;
                        let r = ui.add_sized([220.0, 40.0], egui::TextEdit::multiline(&mut d.hf.text[k]).desired_rows(2).id_salt(("hf-box", k)));
                        if r.gained_focus() || r.has_focus() {
                            d.focused_box = k;
                        }
                    }
                    ui.end_row();
                }
            });
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("hf-page-format").selected_text(PAGE_FORMATS[d.page_format].trim_matches(['<', '>'])).show_ui(
                    ui,
                    |ui| {
                        for (k, f) in PAGE_FORMATS.iter().enumerate() {
                            ui.selectable_value(&mut d.page_format, k, f.trim_matches(['<', '>']));
                        }
                    },
                );
                if ui.button(tl!("Insert Page Number")).clicked() {
                    d.hf.text[d.focused_box].push_str(PAGE_FORMATS[d.page_format]);
                }
                ui.add_space(12.0);
                egui::ComboBox::from_id_salt("hf-date-format").selected_text(DATE_FORMATS[d.date_format].trim_matches(['<', '>'])).show_ui(
                    ui,
                    |ui| {
                        for (k, f) in DATE_FORMATS.iter().enumerate() {
                            ui.selectable_value(&mut d.date_format, k, f.trim_matches(['<', '>']));
                        }
                    },
                );
                if ui.button(tl!("Insert Date")).clicked() {
                    d.hf.text[d.focused_box].push_str(DATE_FORMATS[d.date_format]);
                }
                ui.add_space(12.0);
                ui.label(tl!("Start page number"));
                ui.add(egui::DragValue::new(&mut d.hf.start_number).range(1..=999_999));
            });
            if let Some(mut o) = d.hf.text.iter().find_map(|x| bates_options(x)) {
                browse_bates |= bates_section(ui, t, d, &mut o);
            }
        }
        MarkKind::Watermark => {
            ui.horizontal(|ui| {
                ui.label(tl!("Source"));
                ui.radio_value(&mut d.use_file, false, tl!("Text"));
                ui.radio_value(&mut d.use_file, true, tl!("File"));
            });
            if d.use_file {
                browse |= file_source(ui, d, MarkKind::Watermark);
            }
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.add_enabled_ui(!d.use_file, |ui| {
                        ui.label(tl!("Text"));
                        ui.add(
                            egui::TextEdit::multiline(&mut d.wm.text)
                                .desired_rows(2)
                                .desired_width(320.0)
                                .hint_text("CONFIDENTIAL")
                                .id_salt("wm-text"),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.add_enabled_ui(!d.use_file, |ui| ui.checkbox(&mut d.fit, tl!("Fit to page")));
                        ui.add_enabled_ui(!d.fit, |ui| {
                            ui.label(tl!("Size"));
                            if d.wm.font_size == 0.0 {
                                d.wm.font_size = 72.0;
                            }
                            ui.add(egui::DragValue::new(&mut d.wm.font_size).range(6.0..=300.0));
                        });
                        color_button(ui, &mut d.wm.color);
                    });
                    ui.horizontal(|ui| {
                        ui.label(tl!("Rotation"));
                        for r in [-45.0, 0.0, 45.0] {
                            ui.radio_value(&mut d.wm.rotation, r, if r == 0.0 { tl!("None").to_string() } else { format!("{r}°") });
                        }
                        ui.add(egui::DragValue::new(&mut d.wm.rotation).range(-180.0..=180.0).suffix("°"));
                    });
                    ui.horizontal(|ui| {
                        ui.label(tl!("Opacity"));
                        let mut pct = d.wm.opacity * 100.0;
                        if ui.add(egui::Slider::new(&mut pct, 0.0..=100.0).suffix("%")).changed() {
                            d.wm.opacity = pct / 100.0;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(tl!("Location"));
                        ui.radio_value(&mut d.wm.behind, true, tl!("Appear behind page"));
                        ui.radio_value(&mut d.wm.behind, false, tl!("Appear on top of page"));
                    });
                    // Fitted text takes a share of the page diagonal (pictures their own slider).
                    if !d.use_file && d.fit {
                        ui.horizontal(|ui| {
                            ui.label(tl!("Scale relative to target page"));
                            let mut pct = d.wm.scale * 100.0;
                            if ui.add(egui::Slider::new(&mut pct, 5.0..=100.0).suffix("%")).changed() {
                                d.wm.scale = pct / 100.0;
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        ui.label(tl!("Position"));
                        let max = pdfcraft_engine::MAX_MARK_OFFSET;
                        let l = ui.label(tl!("Horizontal offset"));
                        ui.add(egui::DragValue::new(&mut d.wm.offset[0]).range(-max..=max).speed(1.0).suffix(" pt")).labelled_by(l.id);
                        let l = ui.label(tl!("Vertical offset"));
                        ui.add(egui::DragValue::new(&mut d.wm.offset[1]).range(-max..=max).speed(1.0).suffix(" pt")).labelled_by(l.id);
                        if d.wm.offset != [0.0; 2] && ui.small_button(tl!("Center")).clicked() {
                            d.wm.offset = [0.0; 2];
                        }
                    });
                    ui.horizontal(|ui| {
                        let (mut print, mut screen) = (d.wm.visibility.prints(), d.wm.visibility.on_screen());
                        let a = ui.checkbox(&mut print, tl!("Show when printing")).changed();
                        let b = ui.checkbox(&mut screen, tl!("Show when displaying on screen")).changed();
                        // Neither can't be chosen: unticking the last one ticks the other.
                        if a || b {
                            if !print && !screen {
                                if a {
                                    screen = true;
                                } else {
                                    print = true;
                                }
                            }
                            d.wm.visibility = pdfcraft_engine::MarkVisibility::from_flags(print, screen).unwrap_or_default();
                        }
                    });
                });
            });
        }
        MarkKind::Background => {
            ui.horizontal(|ui| {
                ui.radio_value(&mut d.use_file, false, tl!("From colour"));
                ui.add_enabled_ui(!d.use_file, |ui| color_button(ui, &mut d.bg.color));
                ui.add_space(12.0);
                ui.radio_value(&mut d.use_file, true, tl!("File"));
            });
            if d.use_file {
                browse |= file_source(ui, d, MarkKind::Background);
            }
            ui.horizontal(|ui| {
                ui.label(tl!("Opacity"));
                let mut pct = d.bg.opacity * 100.0;
                if ui.add(egui::Slider::new(&mut pct, 0.0..=100.0).suffix("%")).changed() {
                    d.bg.opacity = pct / 100.0;
                }
            });
        }
    }
    ui.add_space(8.0);
    ui.label(egui::RichText::new(tl!("Page Range Options")).font(theme::semibold(12.5)));
    d.range.ui(ui, count);
    ui.add_space(8.0);
    // Preview of the current page.
    let hf = d.hf.clone();
    let (wm, bg, fit) = (d.wm.clone(), d.bg.clone(), d.fit);
    let file_name = d.use_file.then(|| d.file.as_ref().map(|f| f.0.clone())).flatten();
    ui.horizontal(|ui| {
        preview(ui, t, page, |p, r, s| match kind {
            MarkKind::HeaderFooter | MarkKind::Bates => {
                for (k, text) in hf.text.iter().enumerate() {
                    if text.trim().is_empty() {
                        continue;
                    }
                    let shown = text.replace("<<", "").replace(">>", "");
                    let y = if k < 3 { r.top() + hf.margins[0] as f32 * s } else { r.bottom() - hf.margins[1] as f32 * s };
                    let (x, align) = match k % 3 {
                        0 => (r.left() + hf.margins[2] as f32 * s, egui::Align2::LEFT_CENTER),
                        1 => (r.center().x, egui::Align2::CENTER_CENTER),
                        _ => (r.right() - hf.margins[3] as f32 * s, egui::Align2::RIGHT_CENTER),
                    };
                    p.text(pos2(x, y), align, shown, egui::FontId::proportional((hf.font_size as f32 * s).max(5.0)), rgb32(hf.color));
                }
            }
            MarkKind::Watermark if file_name.is_some() => {
                let side = r.width().min(r.height()) * wm.scale as f32;
                let centre = r.center() + vec2(wm.offset[0] as f32 * s, -(wm.offset[1] as f32) * s);
                p.rect_stroke(
                    Rect::from_center_size(centre, vec2(side, side)),
                    CornerRadius::ZERO,
                    egui::Stroke::new(1.0, Color32::GRAY),
                    egui::StrokeKind::Inside,
                );
                p.text(centre, egui::Align2::CENTER_CENTER, file_name.clone().unwrap_or_default(), egui::FontId::proportional(10.0), Color32::GRAY);
            }
            MarkKind::Watermark => {
                let text = if wm.text.trim().is_empty() { "CONFIDENTIAL" } else { wm.text.trim() };
                let size = if fit {
                    (r.width().hypot(r.height()) * wm.scale as f32 / (text.len().max(1) as f32 * 0.55)).clamp(6.0, 80.0)
                } else {
                    (wm.font_size as f32 * s).max(5.0)
                };
                let galley = p.layout_no_wrap(text.to_string(), egui::FontId::proportional(size), rgb32(wm.color).gamma_multiply(wm.opacity as f32));
                let angle = -(wm.rotation as f32).to_radians();
                let half = galley.size() / 2.0;
                let (sn, cs) = angle.sin_cos();
                let offset = vec2(half.x * cs - half.y * sn, half.x * sn + half.y * cs);
                let centre = r.center() + vec2(wm.offset[0] as f32 * s, -(wm.offset[1] as f32) * s);
                p.add(egui::epaint::TextShape::new(centre - offset, galley, Color32::BLACK).with_angle(angle));
            }
            MarkKind::Background if file_name.is_some() => {
                p.rect_stroke(r.shrink(6.0), CornerRadius::ZERO, egui::Stroke::new(1.0, Color32::GRAY), egui::StrokeKind::Inside);
                p.text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    file_name.clone().unwrap_or_default(),
                    egui::FontId::proportional(10.0),
                    Color32::GRAY,
                );
            }
            MarkKind::Background => {
                p.rect_filled(r, CornerRadius::ZERO, rgb32(bg.color).gamma_multiply(bg.opacity as f32));
            }
        });
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(crate::i18n::fmt(
                    tl!("Preview: page {p} of {n}"),
                    &[("p", &(current + 1).to_string()), ("n", &count.to_string())],
                ))
                .small()
                .color(t.text_faint),
            );
            let n = d.range.pages(count).len();
            let applies =
                if n == 1 { tl!("Applies to 1 page").to_string() } else { crate::i18n::fmt(tl!("Applies to {n} pages"), &[("n", &n.to_string())]) };
            ui.label(egui::RichText::new(applies).small().color(t.text_faint));
            if d.replace {
                ui.label(egui::RichText::new(tl!("Replaces the existing one on those pages.")).small().color(t.text_faint));
            }
        });
    });
    ui.add_space(10.0);
    let (mut apply, mut cancel) = (false, false);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let ready = match kind {
            MarkKind::HeaderFooter | MarkKind::Bates => d.hf.text.iter().any(|x| !x.trim().is_empty()),
            MarkKind::Watermark => {
                if d.use_file {
                    d.file.is_some()
                } else {
                    !d.wm.text.trim().is_empty()
                }
            }
            MarkKind::Background => !d.use_file || d.file.is_some(),
        } && !d.range.pages(count).is_empty();
        if ui.add_enabled_ui(ready, |ui| widgets::pill_button(ui, tl!("OK"), true)).inner.clicked() {
            apply = true;
        }
        if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
            cancel = true;
        }
    });
    // Browse…: the picker answers on a later frame, into the draft.
    #[cfg(not(target_arch = "wasm32"))]
    if browse {
        let dialog = rfd::AsyncFileDialog::new()
            .add_filter(tl!("PDF or image"), &["pdf", "png", "jpg", "jpeg", "tif", "tiff", "gif", "bmp", "jp2", "j2k", "jpx"]);
        let epoch = app.dialog_epoch();
        app.ask_one(crate::pickers::Ask::File(dialog), None, move |app, p| {
            // Only into the dialog that asked: not one closed meanwhile, or opened again since.
            if app.dialog_epoch() != epoch {
                return;
            }
            app.marks_draft.file =
                std::fs::read(&p).ok().map(|b| (p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), std::sync::Arc::new(b)));
        });
    }
    #[cfg(target_arch = "wasm32")]
    let _ = browse;
    if browse_bates {
        app.bates_files_dialog();
    }
    (apply, cancel)
}

/// Bates Number Options (digits, start, prefix, suffix, rewriting every Bates token) and the
/// other files the numbering continues into. Returns true when Add files… was clicked.
fn bates_section(ui: &mut egui::Ui, t: &Tokens, d: &mut MarksDraft, o: &mut BatesOptions) -> bool {
    let before = o.clone();
    let mut browse = false;
    ui.add_space(8.0);
    ui.label(egui::RichText::new(tl!("Bates Number Options")).font(theme::semibold(12.5)));
    ui.horizontal(|ui| {
        let l = ui.label(tl!("Number of digits"));
        ui.add(egui::DragValue::new(&mut o.digits).range(1..=15)).labelled_by(l.id);
        let l = ui.label(tl!("Start number"));
        ui.add(egui::DragValue::new(&mut o.start).range(0..=999_999_999_999u64)).labelled_by(l.id);
        let l = ui.label(tl!("Prefix"));
        ui.add(egui::TextEdit::singleline(&mut o.prefix).desired_width(80.0).char_limit(MAX_BATES_AFFIX).id_salt("bates-prefix")).labelled_by(l.id);
        let l = ui.label(tl!("Suffix"));
        ui.add(egui::TextEdit::singleline(&mut o.suffix).desired_width(80.0).char_limit(MAX_BATES_AFFIX).id_salt("bates-suffix")).labelled_by(l.id);
    });
    if *o != before {
        for text in &mut d.hf.text {
            *text = with_bates_options(text, o);
        }
    }
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(tl!("The numbers continue into these files, in order. Each gets a numbered copy in a folder you choose; the files themselves are left as they are."))
            .small()
            .color(t.text_muted),
    );
    let mut remove = None;
    for (k, (name, _)) in d.bates_files.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{}.", k + 2)).color(t.text_faint));
            ui.label(name);
            if crate::icons::button(ui, "x", 20.0, false, &crate::i18n::fmt(tl!("Remove {name}"), &[("name", name)])).clicked() {
                remove = Some(k);
            }
        });
    }
    if let Some(k) = remove.filter(|k| *k < d.bates_files.len()) {
        d.bates_files.remove(k);
    }
    ui.horizontal(|ui| {
        #[cfg(not(target_arch = "wasm32"))]
        if ui.button(tl!("Add files…")).clicked() {
            browse = true;
        }
        if !d.bates_files.is_empty() {
            let l = ui.label(tl!("File name suffix"));
            ui.add(egui::TextEdit::singleline(&mut d.bates_suffix).desired_width(90.0).char_limit(40).id_salt("bates-file-suffix")).labelled_by(l.id);
            if ui.small_button(tl!("Clear")).clicked() {
                d.bates_files.clear();
            }
        }
    });
    browse
}

/// Files whose Bates numbering continues from the open document's.
pub(crate) struct BatesRun {
    pub files: Vec<(String, std::sync::Arc<Vec<u8>>)>,
    pub hf: HeaderFooter,
    pub start: u64,
    pub replace: bool,
    pub suffix: String,
}

impl PdfCraftApp {
    /// Add files… under Bates Number Options: PDFs the numbering continues into.
    pub(crate) fn bates_files_dialog(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let dialog = rfd::AsyncFileDialog::new().add_filter("PDF", &["pdf"]).set_title(tl!("Add files to number").to_string());
            let epoch = self.dialog_epoch();
            self.ask(crate::pickers::Ask::Files(dialog), None, move |app, paths| {
                if app.dialog_epoch() != epoch {
                    return;
                }
                for p in paths {
                    match std::fs::read(&p) {
                        Ok(b) => {
                            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file.pdf".into());
                            app.add_bates_file(name, b);
                        }
                        Err(e) => app.notify_fmt("Couldn't read {name}: {e}", &[("name", &p.display().to_string()), ("e", &e.to_string())]),
                    }
                }
            });
        }
    }

    /// A file for the Bates list (also the entry point for tests and automation).
    pub fn add_bates_file(&mut self, name: String, bytes: Vec<u8>) {
        if self.marks_draft.bates_files.len() >= pdfcraft_engine::MAX_CREATE_FILES {
            self.notify_tr("That's as many files as one Bates numbering run takes");
            return;
        }
        self.marks_draft.bates_files.push((name, std::sync::Arc::new(bytes)));
    }

    /// After the open document is numbered: number the listed files on from `start`, writing
    /// each numbered copy into a folder (asked for, unless tests set one) in the background.
    pub(crate) fn start_bates_files(&mut self, run: BatesRun) {
        if self.bates_batch.is_some() {
            self.notify_tr("Bates numbering of other files is already running");
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        match self.export_dir_override.clone() {
            Some(dir) => self.bates_files_into(run, dir.into()),
            None => {
                let dialog = rfd::AsyncFileDialog::new().set_title(tl!("Choose a folder for the numbered files").to_string());
                self.ask_one(crate::pickers::Ask::Folder(dialog), None, move |app, dir| app.bates_files_into(run, dir));
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = run;
            self.notify_tr("Numbering other files arrives on the web with folder pickers");
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn bates_files_into(&mut self, run: BatesRun, dir: std::path::PathBuf) {
        use std::sync::{Arc, Mutex};
        if self.bates_batch.is_some() {
            self.notify_tr("Bates numbering of other files is already running");
            return;
        }
        let progress = Arc::new(Mutex::new(crate::ocr_ui::BatchProgress { total: run.files.len(), ..Default::default() }));
        let p = progress.clone();
        let lang = crate::i18n::current();
        let work = move || {
            crate::i18n::set_current(lang);
            // Last-resort guard (AGENTS.md §4): a panic numbering one file ends the run with an
            // error instead of taking the app down.
            let summary = pdfcraft_engine::guard(|| {
                let mut session = pdfcraft_engine::Session::new();
                let mut next = run.start;
                let (mut done, mut last) = (0usize, None);
                for (k, (name, bytes)) in run.files.iter().enumerate() {
                    if let Ok(mut s) = p.lock() {
                        s.done = k;
                    }
                    let stem = std::path::Path::new(name).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into());
                    let numbered = session.bates_number_file(name, bytes.clone(), None, &run.hf, next, run.replace);
                    let saved = numbered.and_then(|f| {
                        let target = pdfcraft_platform::staging::unused_output_path(&dir.join(format!("{stem}{}.pdf", run.suffix)))
                            .map_err(|e| e.to_string())?;
                        crate::editing::write_atomically(&target.to_string_lossy(), &f.bytes).map_err(|e| e.to_string())?;
                        Ok(f.last)
                    });
                    match saved {
                        Ok(l) => {
                            done += 1;
                            last = Some(l);
                            next = l.saturating_add(1);
                        }
                        // Numbers stay unbroken: a file that fails stops the run.
                        Err(e) => {
                            return crate::i18n::fmt(
                                tl!("Numbered {done} of {total} files; {name} couldn't be numbered: {e}"),
                                &[("done", &done.to_string()), ("total", &run.files.len().to_string()), ("name", name), ("e", &e)],
                            );
                        }
                    }
                }
                crate::i18n::fmt(
                    tl!("Numbered {n} more files, up to {last}"),
                    &[("n", &done.to_string()), ("last", &last.unwrap_or(run.start).to_string())],
                )
            })
            .unwrap_or_else(|m| crate::i18n::fmt(tl!("That didn't work: an internal error stopped it ({m})."), &[("m", m.as_str())]));
            if let Ok(mut s) = p.lock() {
                s.done = s.total;
                s.message = Some(summary);
            }
        };
        if self.run_inline {
            work();
        } else if std::thread::Builder::new().name("pdfcraft-bates-files".into()).spawn(work).is_err() {
            self.notify_tr("Bates numbering of other files couldn't start; try again");
            return;
        }
        self.bates_batch = Some(progress);
        self.poll_bates();
    }

    /// Progress of numbering other files, and its summary once done.
    pub(crate) fn poll_bates(&mut self) {
        let Some(b) = self.bates_batch.clone() else { return };
        let state = b.lock().ok().map(|mut s| s.message.take().ok_or((s.done, s.total)));
        match state {
            Some(Ok(m)) => {
                self.bates_batch = None;
                self.notify(m);
            }
            Some(Err((done, total))) => {
                let m = crate::i18n::fmt(
                    tl!("Bates numbering… file {d} of {t}"),
                    &[("d", &(done + 1).min(total.max(1)).to_string()), ("t", &total.max(1).to_string())],
                );
                if self.toast.as_ref().is_none_or(|t| t.0 != m) {
                    self.notify(m);
                }
                if let Some(ctx) = &self.ctx {
                    ctx.request_repaint_after(std::time::Duration::from_millis(200));
                }
            }
            None => self.bates_batch = None,
        }
    }
}

fn mark_file(d: &MarksDraft) -> Option<pdfcraft_engine::MarkFile> {
    let (name, bytes) = d.file.clone().filter(|_| d.use_file)?;
    Some(pdfcraft_engine::MarkFile { name, bytes, page: d.file_page.max(1) - 1 })
}

/// Source ▸ File: Browse…, the page of a PDF, and the size relative to the page. Returns true
/// when Browse… was clicked (the caller shows the picker once it no longer borrows the draft).
fn file_source(ui: &mut egui::Ui, d: &mut MarksDraft, kind: MarkKind) -> bool {
    // Browse… is desktop-only.
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    let mut browse = false;
    ui.horizontal(|ui| {
        #[cfg(not(target_arch = "wasm32"))]
        if ui.button(tl!("Browse…")).clicked() {
            browse = true;
        }
        ui.label(d.file.as_ref().map(|f| f.0.clone()).unwrap_or_else(|| tl!("No file chosen").to_string()));
        if d.file.as_ref().is_some_and(|f| f.1.starts_with(b"%PDF")) {
            ui.label(tl!("Page number"));
            ui.add(egui::DragValue::new(&mut d.file_page).range(1..=9999));
        }
    });
    ui.horizontal(|ui| {
        ui.label(tl!("Scale relative to target page"));
        let scale = if kind == MarkKind::Background { &mut d.bg.scale } else { &mut d.wm.scale };
        let mut pct = *scale * 100.0;
        if ui.add(egui::Slider::new(&mut pct, 5.0..=100.0).suffix("%")).changed() {
            *scale = pct / 100.0;
        }
    });
    browse
}

/// The edit the dialog's OK makes.
pub fn edit(d: &MarksDraft, kind: MarkKind, count: usize) -> Edit {
    let pages = d.range.pages(count);
    match kind {
        MarkKind::HeaderFooter | MarkKind::Bates => Edit::AddHeaderFooter { pages, settings: d.hf.clone(), replace: d.replace },
        MarkKind::Watermark => {
            let mut wm = d.wm.clone();
            if d.fit {
                wm.font_size = 0.0;
            }
            Edit::AddWatermark { pages, settings: wm, replace: d.replace, file: mark_file(d) }
        }
        MarkKind::Background => Edit::AddBackground { pages, settings: d.bg.clone(), replace: d.replace, file: mark_file(d) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bates_tokens_are_read_and_rewritten() {
        let o = bates_options("Case <<Bates Number#4#17#ABC#-X>> page").unwrap();
        assert_eq!(o, BatesOptions { digits: 4, start: 17, prefix: "ABC".into(), suffix: "-X".into() });
        let o2 = BatesOptions { digits: 8, start: 3, prefix: "P#<>".into(), suffix: String::new() };
        assert_eq!(with_bates_options("a <<Bates Number#6#1##>> b <<Bates Number>>", &o2), "a <<Bates Number#8#3#P#>> b <<Bates Number#8#3#P#>>");
        // Hostile tokens: unterminated, huge or non-numeric values, multi-byte text.
        assert_eq!(bates_options("<<Bates Number#99#x"), None);
        let o = bates_options("<<Bates Number#99#99999999999999999999999#é>>").unwrap();
        assert_eq!((o.digits, o.start, o.prefix.as_str()), (15, 1, "é"));
        assert_eq!(with_bates_options("é<<Bates Number#2#5", &o2), "é<<Bates Number#2#5");
        assert!(bates_options("no token").is_none());
    }

    #[test]
    fn page_ranges_with_subsets() {
        let r = PageRange::default();
        assert_eq!(r.pages(4), [0, 1, 2, 3]);
        let r = PageRange { all: false, from: 2, to: 9, subset: Subset::Even };
        assert_eq!(r.pages(6), [1, 3, 5]);
        let r = PageRange { all: true, subset: Subset::Odd, ..PageRange::default() };
        assert_eq!(r.pages(5), [0, 2, 4]);
    }
}

//! Print production ▸ Output Preview, in the app: a side panel with the simulation (profile,
//! overprinting, paper colour, black ink, the Show filter), the separations of the current page
//! with each ink switched on or off and its percentage under the pointer, total area coverage
//! with the areas over a limit painted on the page, colour warnings drawn on the page, and the
//! Object Inspector for whatever is under a click.
//!
//! Speed: nothing here runs while the panel is closed. With it open, the page is separated by
//! PdfCraft's ink rasterizer on a worker thread from a [`PressSnapshot`] (inline on the web and
//! in tests), once per page, document version and settings; the frame only compares a small key,
//! paints the cached picture and reads the ink under the pointer from the cached plates. One job
//! of each kind runs at a time, and a newer request waits for it.

use std::sync::{Arc, Mutex, PoisonError};

use egui::{Align, Align2, Color32, CornerRadius, Layout, Pos2, Rect, Sense, Stroke, StrokeKind, vec2};
use pdfcraft_engine::DocId;
use pdfcraft_engine::output_preview::{OutputPreview, Preview, Show, SimulationInfo, SimulationProfile};
use pdfcraft_engine::preflight::{ColourWarnings, Inspection, ObjectKind};
use pdfcraft_render::DocInfo;
use serde_json::{Value, json};

use crate::canvas::PageXform;
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, RightPanel, icons};

/// Cells per inch the preview separates at (the ink rasterizer lowers it on large pages and
/// with many inks).
pub const PREVIEW_DPI: f64 = 100.0;
/// The Object Inspector's grid (its ink readout).
const INSPECT_DPI: f64 = 72.0;
/// Findings drawn on the page at most.
const MAX_DRAWN_FINDINGS: usize = 200;

/// What the panel shows on the page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviewMode {
    /// The page as the inks switched on would print.
    #[default]
    Separations,
    /// Overprint, rich black and registration colour findings, outlined on the page.
    Warnings,
    /// Click on the page to list the objects there.
    Inspector,
}

impl PreviewMode {
    pub const ALL: [PreviewMode; 3] = [PreviewMode::Separations, PreviewMode::Warnings, PreviewMode::Inspector];

    pub fn id(self) -> &'static str {
        match self {
            PreviewMode::Separations => "separations",
            PreviewMode::Warnings => "warnings",
            PreviewMode::Inspector => "inspector",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.id() == id.trim())
    }

    fn label(self) -> &'static str {
        match self {
            PreviewMode::Separations => tl!("Separations"),
            PreviewMode::Warnings => tl!("Color warnings"),
            PreviewMode::Inspector => tl!("Object inspector"),
        }
    }
}

/// What an analysis was made from: a page of one document version, with the settings it used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    doc: DocId,
    page: usize,
    generation: u64,
    settings: String,
}

type Slot<T> = Arc<Mutex<Option<Result<T, String>>>>;

impl Key {
    /// A whole document version.
    pub(crate) fn document(doc: DocId, generation: u64) -> Self {
        Key { doc, page: 0, generation, settings: String::new() }
    }
}

/// A running (or finished, not yet collected) analysis.
pub(crate) struct Task<T> {
    pub(crate) key: Key,
    slot: Slot<T>,
}

impl<T: Send + 'static> Task<T> {
    /// Run `work` on a worker thread (inline when `inline`, and always on the web), behind a
    /// panic guard. `ctx` is asked to repaint when it finishes.
    pub(crate) fn start(key: Key, inline: bool, ctx: Option<egui::Context>, work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Self {
        let slot: Slot<T> = Arc::default();
        let out = slot.clone();
        let job = move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                .unwrap_or_else(|_| Err("the analysis stopped on an internal error; the document is unchanged".into()));
            *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        if inline {
            job();
        } else if let Err(e) = std::thread::Builder::new().name("pdfcraft-output-preview".into()).spawn(job) {
            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(Err(format!("could not start the analysis: {e}")));
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = inline;
            job();
        }
        Task { key, slot }
    }

    pub(crate) fn take(&self) -> Option<Result<T, String>> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// The current page's preview, ready to paint.
struct Shown {
    key: Key,
    tex: Option<egui::TextureHandle>,
    /// The statistics and plates (the picture itself is in `tex`).
    preview: Preview,
    simulation: SimulationInfo,
}

/// A finished preview before its picture is a texture.
struct Rendered {
    preview: Preview,
    simulation: SimulationInfo,
}

/// Output Preview's state in the app.
#[derive(Default)]
pub struct OutputPreviewUi {
    pub mode: PreviewMode,
    shown: Option<Shown>,
    preview_error: Option<(Key, String)>,
    preview_task: Option<Task<Rendered>>,
    warnings: Option<(Key, Result<ColourWarnings, String>)>,
    warnings_task: Option<Task<ColourWarnings>>,
    inspection: Option<(Key, [f64; 2], Result<Inspection, String>)>,
    inspect_task: Option<(Task<Inspection>, [f64; 2])>,
    /// The point the Object Inspector should look at next (document, page, user space).
    pub inspect_at: Option<(DocId, usize, [f64; 2])>,
    /// The page point under the pointer this frame (document, page, user space).
    pub hover: Option<(DocId, usize, [f64; 2])>,
    /// A warning hovered in the panel: drawn filled on the page.
    hovered_finding: Option<usize>,
}

impl OutputPreviewUi {
    /// Whether an analysis is running.
    pub fn busy(&self) -> bool {
        self.preview_task.is_some() || self.warnings_task.is_some() || self.inspect_task.is_some()
    }

    /// Forget every result (the panel closed, or the document went away).
    pub fn clear(&mut self) {
        *self = OutputPreviewUi { mode: self.mode, ..Default::default() };
    }
}

/// The settings a preview depends on, as a short key (the profile by id, never its bytes).
fn fingerprint(doc: &pdfcraft_engine::Document, color: &pdfcraft_engine::color_settings::ColorManagement) -> String {
    let s = doc.output_preview();
    format!(
        "{}|{}{}{}|{}|{:?}|{}|{}|{:?}|{:?}|{:?}",
        s.profile.id(),
        u8::from(s.simulate_overprint),
        u8::from(s.simulate_paper),
        u8::from(s.simulate_black),
        s.show.id(),
        s.hidden_inks,
        s.coverage_warning,
        s.coverage_limit,
        s.warning_colour,
        doc.ink_manager(),
        color.settings(),
    )
}

fn show_label(s: Show) -> &'static str {
    match s {
        Show::All => tl!("All"),
        Show::DeviceCmyk => "DeviceCMYK",
        Show::NotDeviceCmyk => tl!("Not DeviceCMYK"),
        Show::Spot => tl!("Spot colors"),
        Show::Rgb => "RGB",
        Show::Gray => tl!("Gray"),
        Show::IccBased => tl!("ICC-based"),
        Show::Images => tl!("Images"),
        Show::SolidColor => tl!("Solid color"),
        Show::Text => tl!("Text"),
        Show::LineArt => tl!("Line art"),
    }
}

pub(crate) fn swatch(c: [f64; 3]) -> Color32 {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(b(c[0]), b(c[1]), b(c[2]))
}

fn percent(v: f64) -> String {
    if v.is_finite() { format!("{:.0}%", v) } else { "—".into() }
}

impl PdfCraftApp {
    /// Print production ▸ Output preview: open the panel (or close it when it shows `mode`).
    pub fn open_output_preview(&mut self, mode: PreviewMode) {
        if self.right == Some(RightPanel::OutputPreview) && self.output_preview.mode == mode && mode == PreviewMode::Separations {
            self.choose_right_panel(None);
            self.output_preview.clear();
            return;
        }
        self.output_preview.mode = mode;
        self.choose_right_panel(Some(RightPanel::OutputPreview));
    }

    /// Change the active document's Output Preview settings (session only, not an undo step).
    fn change_output_preview(&mut self, id: DocId, change: impl FnOnce(&mut OutputPreview)) {
        let Some(doc) = self.session.get(id) else { return };
        let mut s = doc.output_preview().clone();
        change(&mut s);
        if s.simulate_paper {
            s.simulate_black = true;
        }
        if let Err(e) = self.session.set_output_preview(id, s) {
            self.notify_error(e);
        }
    }

    /// Start or collect the analyses the panel needs for the active document's current page.
    /// Called by the panel each frame it shows (never while it is closed).
    fn poll_output_preview(&mut self, ctx: &egui::Context) {
        let Some((vi, id)) = self.active_ids() else { return };
        let page = self.views.get(vi).map_or(0, |v| v.current);
        let inline = self.run_inline;
        let Some(doc) = self.session.get(id) else { return };
        let generation = doc.edit_generation();
        let settings = fingerprint(doc, self.session.color_management());
        let state = &mut self.output_preview;
        let wanted = Key { doc: id, page, generation, settings };

        // The separations preview.
        if let Some(task) = &state.preview_task
            && let Some(done) = task.take()
        {
            let key = task.key.clone();
            state.preview_task = None;
            match done {
                Ok(Rendered { mut preview, simulation }) => {
                    let (w, h) = (preview.width, preview.height);
                    let rgb = std::mem::take(&mut preview.rgb);
                    let tex = (w > 0 && h > 0 && rgb.len() == w.saturating_mul(h).saturating_mul(3))
                        .then(|| ctx.load_texture("output-preview", egui::ColorImage::from_rgb([w, h], &rgb), egui::TextureOptions::LINEAR));
                    state.preview_error = None;
                    state.shown = Some(Shown { key, tex, preview, simulation });
                }
                Err(e) => {
                    state.shown = None;
                    state.preview_error = Some((key, e));
                }
            }
        }
        let have = state.shown.as_ref().map(|s| &s.key).or(state.preview_error.as_ref().map(|e| &e.0));
        if state.mode == PreviewMode::Separations && have != Some(&wanted) && state.preview_task.is_none() {
            match doc.press_snapshot() {
                Ok(snapshot) => {
                    let ctx = (!inline).then(|| ctx.clone());
                    state.preview_task = Some(Task::start(wanted.clone(), inline, ctx, move || {
                        let (preview, simulation) = snapshot.render_output_preview(page, PREVIEW_DPI).map_err(|e| e.to_string())?;
                        Ok(Rendered { preview, simulation })
                    }));
                }
                Err(e) => state.preview_error = Some((wanted.clone(), e.to_string())),
            }
        }

        // Colour warnings (they read the document as it is: no settings).
        let plain = Key { settings: String::new(), ..wanted.clone() };
        if let Some(task) = &state.warnings_task
            && let Some(done) = task.take()
        {
            state.warnings = Some((task.key.clone(), done));
            state.warnings_task = None;
            state.hovered_finding = None;
        }
        if state.mode == PreviewMode::Warnings && state.warnings.as_ref().map(|w| &w.0) != Some(&plain) && state.warnings_task.is_none() {
            match doc.press_snapshot() {
                Ok(snapshot) => {
                    let ctx = (!inline).then(|| ctx.clone());
                    state.warnings_task =
                        Some(Task::start(plain.clone(), inline, ctx, move || snapshot.colour_warnings(page).map_err(|e| e.to_string())));
                }
                Err(e) => state.warnings = Some((plain.clone(), Err(e.to_string()))),
            }
        }

        // The Object Inspector, for the last click.
        if let Some((task, point)) = &state.inspect_task
            && let Some(done) = task.take()
        {
            state.inspection = Some((task.key.clone(), *point, done));
            state.inspect_task = None;
        }
        if state.inspect_task.is_none()
            && let Some((doc_id, at_page, point)) = state.inspect_at.take()
            && doc_id == id
        {
            let key = Key { page: at_page, ..wanted };
            match doc.press_snapshot() {
                Ok(snapshot) => {
                    let ctx = (!inline).then(|| ctx.clone());
                    let task = Task::start(key, inline, ctx, move || {
                        snapshot.inspect_objects(at_page, point[0], point[1], INSPECT_DPI).map_err(|e| e.to_string())
                    });
                    state.inspect_task = Some((task, point));
                }
                Err(e) => state.inspection = Some((key, point, Err(e.to_string()))),
            }
        }
        if state.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }
    }

    /// `ui.state`'s `output_preview`: what the panel shows (only while it is open).
    pub fn output_preview_state(&self) -> Value {
        let s = &self.output_preview;
        let settings = self.active_ids().and_then(|(_, id)| self.session.get(id)).map(|d| {
            let o = d.output_preview();
            json!({
                "simulation_profile": o.profile.id(),
                "simulate_overprint": o.simulate_overprint,
                "simulate_paper": o.simulate_paper,
                "simulate_black": o.simulate_black || o.simulate_paper,
                "show": o.show.id(),
                "hidden_inks": o.hidden_inks,
                "coverage_warning": o.coverage_warning,
                "coverage_limit": o.coverage_limit,
            })
        });
        let preview = s.shown.as_ref().map(|v| {
            let c = &v.preview.coverage;
            let sample = s
                .hover
                .filter(|(d, p, _)| *d == v.key.doc && *p == v.key.page)
                .and_then(|(_, _, [x, y])| pdfcraft_engine::pdfa::separations::sample(&v.preview.raster, x, y));
            json!({
                "page": v.key.page + 1,
                "inks": v.preview.inks.iter().map(|i| json!({ "name": i.info.name, "shown": i.shown, "max": i.max })).collect::<Vec<_>>(),
                "coverage_max": c.max,
                "over_limit": c.over_cells > 0,
                "over_percent": c.over_percent,
                "warned_pixels": c.warned_pixels,
                "simulation": v.simulation.used,
                "textured": v.tex.is_some(),
                "pointer": sample.map(|s| json!({ "total": s.total, "inks": s.inks })),
            })
        });
        let warnings = s.warnings.as_ref().map(|(k, r)| match r {
            Ok(w) => json!({ "page": k.page + 1, "findings": w.findings.len(), "kinds": w.findings.iter().map(|f| f.kind.id()).collect::<Vec<_>>() }),
            Err(e) => json!({ "page": k.page + 1, "error": e }),
        });
        let inspection = s.inspection.as_ref().map(|(k, p, r)| match r {
            Ok(i) => json!({
                "page": k.page + 1,
                "point": p,
                "objects": i.objects.iter().map(|o| json!({ "kind": o.kind.as_str(), "fill": o.fill.as_ref().map(|c| &c.space) })).collect::<Vec<_>>(),
            }),
            Err(e) => json!({ "page": k.page + 1, "point": p, "error": e }),
        });
        json!({
            "mode": s.mode.id(),
            "busy": s.busy(),
            "settings": settings,
            "preview": preview,
            "preview_error": s.preview_error.as_ref().map(|e| &e.1),
            "warnings": warnings,
            "inspection": inspection,
        })
    }

    /// `ui.set` keys for Output Preview (`None`: not one of them).
    pub(crate) fn set_output_preview_option(&mut self, key: &str, value: &str) -> Option<Result<(), String>> {
        let id = self.active_ids().map(|(_, id)| id);
        let flag = |v: &str| match v.trim() {
            "true" | "on" | "1" => Ok(true),
            "false" | "off" | "0" => Ok(false),
            _ => Err(format!("{key} must be on or off")),
        };
        let result = match key {
            "output-preview" => match PreviewMode::from_id(value) {
                Some(m) => {
                    self.output_preview.mode = m;
                    self.choose_right_panel(Some(RightPanel::OutputPreview));
                    Ok(())
                }
                None if value == "closed" => {
                    self.choose_right_panel(None);
                    self.output_preview.clear();
                    Ok(())
                }
                None => Err("output-preview must be separations, warnings, inspector or closed".into()),
            },
            "output-preview-inspect" => {
                let Some(id) = id else { return Some(Err("open a document first".into())) };
                let page = self.active.and_then(|i| self.views.get(i)).map_or(0, |v| v.current);
                let parts: Vec<f64> = value.split(',').filter_map(|v| v.trim().parse().ok()).collect();
                match parts.as_slice() {
                    [x, y] if x.is_finite() && y.is_finite() => {
                        self.output_preview.mode = PreviewMode::Inspector;
                        self.choose_right_panel(Some(RightPanel::OutputPreview));
                        self.output_preview.inspect_at = Some((id, page, [*x, *y]));
                        Ok(())
                    }
                    _ => Err("output-preview-inspect takes x,y in PDF user space".into()),
                }
            }
            "output-preview-ink" | "output-preview-hide-ink" => {
                let Some(id) = id else { return Some(Err("open a document first".into())) };
                let name = value.trim();
                if name.is_empty() || name.len() > pdfcraft_engine::output_preview::MAX_INK_NAME {
                    return Some(Err("give an ink name".into()));
                }
                let hide = key == "output-preview-hide-ink";
                let name = name.to_string();
                self.change_output_preview(id, |s| {
                    if hide {
                        s.hidden_inks.insert(name);
                    } else {
                        s.hidden_inks.remove(&name);
                    }
                });
                Ok(())
            }
            "output-preview-coverage" => {
                let Some(id) = id else { return Some(Err("open a document first".into())) };
                match value.trim() {
                    "off" => {
                        self.change_output_preview(id, |s| s.coverage_warning = false);
                        Ok(())
                    }
                    v => match v.parse::<f64>() {
                        Ok(limit) if (pdfcraft_engine::output_preview::MIN_LIMIT..=pdfcraft_engine::output_preview::MAX_LIMIT).contains(&limit) => {
                            self.change_output_preview(id, |s| {
                                s.coverage_warning = true;
                                s.coverage_limit = limit;
                            });
                            Ok(())
                        }
                        _ => Err("output-preview-coverage takes off or a limit from 1 to 2000 (percent)".into()),
                    },
                }
            }
            "output-preview-overprint" | "output-preview-paper" | "output-preview-black" => {
                let Some(id) = id else { return Some(Err("open a document first".into())) };
                match flag(value) {
                    Ok(on) => {
                        self.change_output_preview(id, |s| match key {
                            "output-preview-overprint" => s.simulate_overprint = on,
                            "output-preview-paper" => s.simulate_paper = on,
                            _ => {
                                s.simulate_black = on;
                                if !on {
                                    s.simulate_paper = false;
                                }
                            }
                        });
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            "output-preview-show" => {
                let Some(id) = id else { return Some(Err("open a document first".into())) };
                match Show::from_id(value) {
                    Some(show) => {
                        self.change_output_preview(id, |s| s.show = show);
                        Ok(())
                    }
                    None => Err(format!("output-preview-show must be one of {}", Show::ALL.iter().map(|s| s.id()).collect::<Vec<_>>().join(", "))),
                }
            }
            _ => return None,
        };
        Some(result)
    }
}

/// The Output Preview panel (in place of the other right-hand panels while it is chosen).
pub(crate) fn right_panel(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    app.poll_output_preview(&ctx);
    let Some((vi, id)) = app.active_ids() else { return };
    let page = app.views.get(vi).map_or(0, |v| v.current);
    egui::Panel::right("output_preview_panel")
        .resizable(true)
        .default_size(340.0)
        .size_range(280.0..=520.0)
        .frame(
            egui::Frame::NONE
                .fill(t.panel)
                .inner_margin(egui::Margin { left: 14, right: 12, top: 12, bottom: 8 })
                .stroke(Stroke::new(1.0, t.divider)),
        )
        .show(ui, |ui| {
            let mut close = false;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!("Output preview")).font(theme::semibold(15.5)));
                let n = (page + 1).to_string();
                ui.label(egui::RichText::new(crate::i18n::fmt(tl!("Page {n}"), &[("n", &n)])).font(theme::medium(12.5)).color(t.text_faint));
                if app.output_preview.busy() {
                    ui.add(egui::Spinner::new().size(13.0));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icons::button(ui, "x", 26.0, false, tl!("Close")).clicked() {
                        close = true;
                    }
                });
            });
            if close {
                app.choose_right_panel(None);
                app.output_preview.clear();
                return;
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for m in PreviewMode::ALL {
                    if crate::widgets::mode_tab(ui, m.label(), app.output_preview.mode == m).clicked() {
                        app.output_preview.mode = m;
                    }
                }
            });
            ui.add_space(6.0);
            egui::ScrollArea::vertical().id_salt("output-preview").auto_shrink([false, false]).show(ui, |ui| {
                simulation_section(app, ui, &t, id);
                ui.add_space(6.0);
                match app.output_preview.mode {
                    PreviewMode::Separations => separations_section(app, ui, &t, id),
                    PreviewMode::Warnings => warnings_section(app, ui, &t),
                    PreviewMode::Inspector => inspector_section(app, ui, &t),
                }
                ui.add_space(10.0);
                if crate::widgets::ghost_button(ui, "droplet", tl!("Ink manager…")).clicked() {
                    app.run_command("prepress.ink_manager");
                }
            });
        });
}

fn simulation_section(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, id: DocId) {
    let Some(doc) = app.session.get(id) else { return };
    let s = doc.output_preview().clone();
    let cmyk: Vec<(String, String)> = app
        .session
        .color_management()
        .profiles()
        .into_iter()
        .filter(|p| p.family == pdfcraft_engine::color_settings::ProfileFamily::Cmyk)
        .map(|p| {
            let shown = if p.description.is_empty() || p.description == p.id { p.id.clone() } else { format!("{} ({})", p.description, p.id) };
            (p.id, shown)
        })
        .collect();
    let current = match &s.profile {
        SimulationProfile::OutputIntent => tl!("Output intent").to_string(),
        SimulationProfile::Working => tl!("Working CMYK space").to_string(),
        SimulationProfile::Profile { name, .. } => name.clone(),
    };
    let mut choose: Option<String> = None;
    let mut next = s.clone();
    egui::Grid::new("output-preview-sim").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        let l = ui.label(egui::RichText::new(tl!("Simulation profile")).color(t.text_muted));
        egui::ComboBox::from_id_salt("output-preview-profile")
            .selected_text(current)
            .width(180.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(s.profile == SimulationProfile::OutputIntent, tl!("Output intent")).clicked() {
                    choose = Some("output_intent".into());
                }
                if ui.selectable_label(s.profile == SimulationProfile::Working, tl!("Working CMYK space")).clicked() {
                    choose = Some("working".into());
                }
                for (pid, shown) in &cmyk {
                    if ui.selectable_label(s.profile.id() == pid && matches!(s.profile, SimulationProfile::Profile { .. }), shown).clicked() {
                        choose = Some(pid.clone());
                    }
                }
            })
            .response
            .labelled_by(l.id);
        ui.end_row();
        let l = ui.label(egui::RichText::new(tl!("Show")).color(t.text_muted));
        egui::ComboBox::from_id_salt("output-preview-show")
            .selected_text(show_label(s.show))
            .width(180.0)
            .show_ui(ui, |ui| {
                for v in Show::ALL {
                    ui.selectable_value(&mut next.show, v, show_label(v));
                }
            })
            .response
            .labelled_by(l.id);
        ui.end_row();
    });
    ui.checkbox(&mut next.simulate_overprint, tl!("Simulate overprinting"));
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut next.simulate_paper, tl!("Simulate paper color"));
        let mut black = next.simulate_black || next.simulate_paper;
        if ui.add_enabled(!next.simulate_paper, egui::Checkbox::new(&mut black, tl!("Simulate black ink"))).changed() {
            next.simulate_black = black;
        }
    });
    if let Some(problem) = app.output_preview.shown.as_ref().and_then(|v| v.simulation.problem.clone()) {
        ui.label(egui::RichText::new(problem).small().color(t.text_faint));
    }
    if let Some(choice) = choose {
        let profile = match choice.as_str() {
            "output_intent" => Ok(SimulationProfile::OutputIntent),
            "working" => Ok(SimulationProfile::Working),
            other => app.session.simulation_profile(other),
        };
        match profile {
            Ok(p) => next.profile = p,
            Err(e) => app.notify_error(e),
        }
    }
    if next != s {
        app.change_output_preview(id, |o| *o = next);
    }
}

fn separations_section(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, id: DocId) {
    crate::widgets::section_title(ui, tl!("Separations"));
    if let Some((_, e)) = &app.output_preview.preview_error {
        ui.label(egui::RichText::new(e.clone()).color(t.text_muted));
        return;
    }
    let Some(shown) = app.output_preview.shown.as_ref() else {
        ui.label(egui::RichText::new(tl!("Analyzing…")).color(t.text_muted));
        return;
    };
    let hover = app.output_preview.hover.filter(|(d, p, _)| *d == shown.key.doc && *p == shown.key.page);
    let sample = hover.and_then(|(_, _, [x, y])| pdfcraft_engine::pdfa::separations::sample(&shown.preview.raster, x, y));
    let at = |name: &str| sample.as_ref().and_then(|s| s.inks.iter().find(|(n, _)| n == name).map(|(_, v)| *v));
    let hidden = app.session.get(id).map(|d| d.output_preview().hidden_inks.clone()).unwrap_or_default();
    let mut toggle: Option<(String, bool)> = None;
    let mut all_on = false;
    let rows = shown.preview.inks.len();
    let row_h = 26.0;
    // Hundreds of spot inks: only the rows on screen are laid out.
    egui::ScrollArea::vertical().id_salt("output-preview-inks").max_height(row_h * 10.0).auto_shrink([false, true]).show_rows(
        ui,
        row_h,
        rows,
        |ui, range| {
            for ink in shown.preview.inks.get(range).unwrap_or_default() {
                ui.horizontal(|ui| {
                    ui.set_height(row_h - 4.0);
                    let mut on = !hidden.contains(&ink.info.name);
                    let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                    ui.painter().rect_filled(r, CornerRadius::same(3), swatch(ink.info.preview));
                    ui.painter().rect_stroke(r, CornerRadius::same(3), Stroke::new(1.0, t.border), StrokeKind::Inside);
                    let name = if ink.info.process { tl!(ink.info.name.as_str()).to_string() } else { ink.info.name.clone() };
                    let colour = if on { t.text } else { t.text_faint };
                    if ui.checkbox(&mut on, egui::RichText::new(name).color(colour)).changed() {
                        toggle = Some((ink.info.name.clone(), on));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(egui::RichText::new(at(&ink.info.name).map_or_else(|| "—".to_string(), percent)).monospace().color(t.text_muted));
                    });
                });
            }
        },
    );
    if !hidden.is_empty() && ui.link(tl!("Show all inks")).clicked() {
        all_on = true;
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Total area coverage")).font(theme::medium(13.0)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let total = sample.as_ref().map_or_else(|| "—".to_string(), |s| percent(s.total));
            ui.label(egui::RichText::new(total).monospace().color(t.text));
        });
    });
    let c = shown.preview.coverage.clone();
    let max = crate::i18n::fmt(tl!("Highest on this page: {n}"), &[("n", &percent(c.max))]);
    ui.label(egui::RichText::new(max).small().color(t.text_muted));
    let mut s = app.session.get(id).map(|d| d.output_preview().clone()).unwrap_or_default();
    let before = s.clone();
    ui.horizontal(|ui| {
        ui.checkbox(&mut s.coverage_warning, tl!("Highlight coverage over"));
        ui.add(egui::DragValue::new(&mut s.coverage_limit).range(1.0..=2000.0).suffix("%").speed(1.0));
        let mut c = s.warning_colour;
        if ui.color_edit_button_srgb(&mut c).changed() {
            s.warning_colour = c;
        }
    });
    if c.warning && c.over_cells > 0 {
        let over = crate::i18n::fmt(tl!("{n} of the page is over the limit"), &[("n", &format!("{:.1}%", c.over_percent))]);
        ui.label(egui::RichText::new(over).small().color(t.text_muted));
    }
    let r = &shown.preview.raster;
    let unmeasured: usize = r.unmeasured.values().sum();
    if unmeasured > 0 || r.truncated {
        ui.label(egui::RichText::new(tl!("Some objects can't be measured and are left out.")).small().color(t.text_faint));
    }
    if s != before {
        app.change_output_preview(id, |o| *o = s);
    }
    if let Some((name, on)) = toggle {
        app.change_output_preview(id, |o| {
            if on {
                o.hidden_inks.remove(&name);
            } else if o.hidden_inks.len() < pdfcraft_engine::output_preview::MAX_HIDDEN_INKS {
                o.hidden_inks.insert(name);
            }
        });
    }
    if all_on {
        app.change_output_preview(id, |o| o.hidden_inks.clear());
    }
}

fn warnings_section(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    crate::widgets::section_title(ui, tl!("Color warnings"));
    let state = &mut app.output_preview;
    let Some((_, result)) = state.warnings.as_ref() else {
        ui.label(egui::RichText::new(tl!("Analyzing…")).color(t.text_muted));
        return;
    };
    let w = match result {
        Ok(w) => w,
        Err(e) => {
            ui.label(egui::RichText::new(e.clone()).color(t.text_muted));
            return;
        }
    };
    if w.findings.is_empty() {
        ui.label(egui::RichText::new(tl!("No overprint, rich black or registration color on this page.")).color(t.text_muted));
        return;
    }
    let mut hovered = None;
    let row_h = 44.0;
    egui::ScrollArea::vertical().id_salt("output-preview-findings").max_height(row_h * 9.0).auto_shrink([false, true]).show_rows(
        ui,
        row_h,
        w.findings.len(),
        |ui, range| {
            for i in range {
                let Some(f) = w.findings.get(i) else { continue };
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h - 4.0), Sense::hover());
                if resp.hovered() {
                    hovered = Some(i);
                    ui.painter().rect_filled(rect, CornerRadius::same(6), t.hover);
                }
                let warn = f.severity == pdfcraft_engine::pdfa::colour_warnings::Severity::Warning;
                let icon = if warn { "triangle-alert" } else { "info" };
                icons::paint(ui, Rect::from_min_size(rect.min + vec2(4.0, 6.0), vec2(16.0, 16.0)), icon, 15.0, finding_colour(warn, t));
                let title = match f.kind.id() {
                    "overprint-state" | "overprint-paint" => tl!("Overprint"),
                    "overprint-white" => tl!("White overprint"),
                    "rich-black" => tl!("Rich black"),
                    "rich-black-small-text" => tl!("Small rich black text"),
                    _ => tl!("Registration color"),
                };
                let text = if f.occurrences > 1 { format!("{title} ×{}", f.occurrences) } else { title.to_string() };
                ui.painter().text(rect.left_top() + vec2(26.0, 4.0), Align2::LEFT_TOP, &text, theme::medium(12.5), t.text);
                crate::widgets::row_text(ui, rect.left_top() + vec2(26.0, 28.0), &f.message, theme::regular(11.0), t.text_muted, rect.width() - 30.0);
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &text));
            }
        },
    );
    state.hovered_finding = hovered;
}

fn finding_colour(warning: bool, t: &Tokens) -> Color32 {
    if warning { Color32::from_rgb(0xE0, 0x6C, 0x1A) } else { t.accent }
}

fn colour_line(c: &pdfcraft_engine::preflight::ObjectColour) -> String {
    let inks: Vec<String> = c.inks.iter().map(|(n, v)| format!("{n} {}", percent(*v))).collect();
    if inks.is_empty() { c.space.clone() } else { format!("{} · {}", c.space, inks.join(", ")) }
}

fn inspector_section(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    crate::widgets::section_title(ui, tl!("Object inspector"));
    let state = &app.output_preview;
    let Some((_, point, result)) = state.inspection.as_ref() else {
        let text = if state.inspect_task.is_some() { tl!("Analyzing…") } else { tl!("Click on the page to list the objects under the pointer.") };
        ui.label(egui::RichText::new(text).color(t.text_muted));
        return;
    };
    let found = match result {
        Ok(f) => f,
        Err(e) => {
            ui.label(egui::RichText::new(e.clone()).color(t.text_muted));
            return;
        }
    };
    ui.label(egui::RichText::new(format!("x {:.1}  y {:.1} pt", point[0], point[1])).small().monospace().color(t.text_faint));
    if let Some(s) = &found.sample {
        let inks: Vec<String> = s.inks.iter().filter(|(_, v)| *v > 0.0).map(|(n, v)| format!("{n} {}", percent(*v))).collect();
        let line = crate::i18n::fmt(tl!("Inks here: {inks} (total {total})"), &[("inks", &inks.join(", ")), ("total", &percent(s.total))]);
        ui.label(egui::RichText::new(line).small().color(t.text_muted));
    }
    if found.objects.is_empty() {
        ui.label(egui::RichText::new(tl!("Nothing is painted here.")).color(t.text_muted));
        return;
    }
    for (i, o) in found.objects.iter().enumerate() {
        let kind = match o.kind {
            ObjectKind::Text => tl!("Text"),
            ObjectKind::Path => tl!("Path"),
            ObjectKind::Image => tl!("Image"),
            ObjectKind::Shading => tl!("Shading"),
            ObjectKind::Form => tl!("Form XObject"),
            ObjectKind::Annotation => tl!("Annotation"),
        };
        egui::CollapsingHeader::new(egui::RichText::new(kind).font(theme::medium(13.0))).id_salt(("inspected", i)).default_open(i == 0).show(
            ui,
            |ui| {
                let line = |ui: &mut egui::Ui, label: &str, value: String| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(label).small().color(t.text_muted));
                        ui.label(egui::RichText::new(value).small());
                    });
                };
                if let Some(c) = &o.fill {
                    line(ui, tl!("Fill"), colour_line(c));
                }
                if let Some(c) = &o.stroke {
                    line(ui, tl!("Stroke"), colour_line(c));
                }
                let op = &o.overprint;
                if op.fill || op.stroke {
                    line(ui, tl!("Overprint"), format!("op {} · OP {} · OPM {}", op.fill, op.stroke, op.mode));
                }
                let tr = &o.transparency;
                if tr.is_transparent() {
                    line(ui, tl!("Transparency"), format!("{} · ca {} · CA {}", tr.blend_mode, tr.fill_alpha, tr.stroke_alpha));
                }
                if let Some(f) = &o.text {
                    let embedded = if f.embedded { tl!("embedded") } else { tl!("not embedded") };
                    line(ui, tl!("Font"), format!("{} · {:.1} pt · {embedded}", f.font, f.effective_size));
                }
                if let Some(im) = &o.image {
                    line(ui, tl!("Image"), format!("{} × {} px · {} · {:.0} ppi", im.width, im.height, im.colour_space, im.ppi[0].min(im.ppi[1])));
                }
                line(ui, tl!("Rendering intent"), o.rendering_intent.clone());
            },
        );
    }
}

/// Draw Output Preview on page `page` and read the pointer there. Called by the page view for
/// every page on screen, only while the panel is open.
#[allow(clippy::too_many_arguments)]
pub(crate) fn page(
    ui: &egui::Ui,
    resp: &egui::Response,
    painter: &egui::Painter,
    xf: &PageXform,
    page: usize,
    info: &DocInfo,
    doc: DocId,
    state: &mut OutputPreviewUi,
    t: &Tokens,
) {
    let Some(p) = info.pages.get(page) else { return };
    let pointer = ui.input(|i| i.pointer.hover_pos()).filter(|pos| xf.rect.contains(*pos));
    let to_user = |pos: Pos2| {
        let (vx, vy) = xf.screen_to_view(pos);
        let u = p.view_to_user(vx, vy);
        [f64::from(u[0]), f64::from(u[1])]
    };
    if let Some(pos) = pointer {
        state.hover = Some((doc, page, to_user(pos)));
    }
    let rect = |r: [f64; 4]| xf.user_rect(info, page, [r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32]);
    match state.mode {
        PreviewMode::Separations => {
            let Some(shown) = state.shown.as_ref().filter(|s| s.key.doc == doc && s.key.page == page) else { return };
            if let Some(tex) = &shown.tex {
                let crop = shown.preview.raster.crop;
                painter.rect_filled(xf.rect, CornerRadius::ZERO, Color32::WHITE);
                xf.paint_user_image(painter, tex.id(), p, crop, 0, Color32::WHITE);
            }
            let c = &shown.preview.coverage;
            if c.warning
                && c.over_cells > 0
                && let Some([x, y]) = c.max_at
            {
                let at = rect([x, y, x, y]).center();
                painter.circle_stroke(at, 7.0, Stroke::new(2.0, Color32::WHITE));
                painter.circle_stroke(at, 7.0, Stroke::new(1.2, Color32::BLACK));
            }
        }
        PreviewMode::Warnings => {
            let Some((key, Ok(w))) = state.warnings.as_ref() else { return };
            if key.doc != doc || key.page != page {
                return;
            }
            for (i, f) in w.findings.iter().enumerate().take(MAX_DRAWN_FINDINGS) {
                let Some(b) = f.bbox else { continue };
                if !b.iter().all(|v| v.is_finite()) {
                    continue;
                }
                let warn = f.severity == pdfcraft_engine::pdfa::colour_warnings::Severity::Warning;
                let colour = finding_colour(warn, t);
                let r = rect(b).expand(1.5);
                if state.hovered_finding == Some(i) {
                    painter.rect_filled(r, CornerRadius::same(2), colour.gamma_multiply(0.25));
                }
                painter.rect_stroke(r, CornerRadius::same(2), Stroke::new(1.5, colour), StrokeKind::Outside);
            }
        }
        PreviewMode::Inspector => {
            if pointer.is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                if resp.clicked()
                    && let Some(pos) = pointer
                {
                    state.inspect_at = Some((doc, page, to_user(pos)));
                }
            }
            let Some((key, point, Ok(found))) = state.inspection.as_ref() else { return };
            if key.doc != doc || key.page != page {
                return;
            }
            for (i, o) in found.objects.iter().enumerate().rev() {
                if !o.bbox.iter().all(|v| v.is_finite()) {
                    continue;
                }
                let width = if i == 0 { 2.0 } else { 1.0 };
                painter.rect_stroke(
                    rect(o.bbox),
                    CornerRadius::ZERO,
                    Stroke::new(width, t.accent.gamma_multiply(if i == 0 { 1.0 } else { 0.5 })),
                    StrokeKind::Outside,
                );
            }
            let at = rect([point[0], point[1], point[0], point[1]]).center();
            painter.line_segment([at - vec2(6.0, 0.0), at + vec2(6.0, 0.0)], Stroke::new(1.5, t.accent));
            painter.line_segment([at - vec2(0.0, 6.0), at + vec2(0.0, 6.0)], Stroke::new(1.5, t.accent));
        }
    }
}

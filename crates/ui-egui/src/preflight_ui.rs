//! Preflight profiles, object-located results and explicit selected fixes.

use crate::{PdfCraftApp, theme::Tokens};
use pdfcraft_engine::preflight::{Profile, ResultSet, Rule, Settings};
use std::collections::BTreeSet;

#[derive(Default)]
pub struct PreflightState {
    pub settings: Settings,
    pub result: Option<ResultSet>,
    pub selected: BTreeSet<usize>,
}

impl PdfCraftApp {
    pub fn preflight_analyze(&mut self) -> bool {
        if !self.commit_form_typing() {
            return false;
        }
        self.preflight.result = None;
        self.preflight.selected.clear();
        let Some((_, id)) = self.active_ids() else { return false };
        let Some(doc) = self.session.get(id) else { return false };
        match doc.preflight(&self.preflight.settings) {
            Ok(result) => {
                self.preflight.result = Some(result);
                self.preflight.selected.clear();
                true
            }
            Err(e) => {
                self.notify_error(e);
                false
            }
        }
    }

    pub fn preflight_fix_selected(&mut self) {
        if !self.commit_form_typing() {
            return;
        }
        let Some(result) = self.preflight.result.as_ref() else { return };
        let Some((view, id)) = self.active_ids() else { return };
        if id != result.document {
            self.notify("Preflight belongs to a different document; analyze again");
            return;
        }
        let selected = self.preflight.selected.iter().copied().collect::<Vec<_>>();
        match self.session.preflight_fix(result, &selected) {
            Ok(result) => {
                self.preflight.result = Some(result);
                self.preflight.selected.clear();
                if let Some(info) = self.session.get(id).map(|d| d.info.clone())
                    && let Some(view) = self.views.get_mut(view)
                {
                    view.document_changed(&info);
                }
                self.notify("Selected preflight fixes applied. Undo restores the previous document.");
            }
            Err(e) => self.notify_error(e),
        }
    }

    pub fn preflight_export_report(&mut self) {
        if !self.preflight_analyze() {
            return;
        }
        let Some((_, id)) = self.active_ids() else { return };
        let Some(result) = self.preflight.result.as_ref().filter(|r| r.document == id) else { return };
        let bytes = result.report.text().into_bytes();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let write = move |app: &mut Self, path: std::path::PathBuf| match crate::editing::write_atomically(&path.to_string_lossy(), &bytes) {
                Ok(()) => app.notify("Preflight report exported"),
                Err(e) => app.notify_error(e),
            };
            match self.save_override.clone() {
                Some(path) => write(self, path.into()),
                None => {
                    let dialog = rfd::AsyncFileDialog::new()
                        .set_title("Export preflight report")
                        .add_filter("Text", &["txt"])
                        .set_file_name("preflight-report.txt");
                    self.ask_one(crate::pickers::Ask::Save(dialog), None, write);
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        if let Err(e) = crate::editing::download("preflight-report.txt", &bytes) {
            self.notify_error(e);
        }
    }
}

pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    ui.set_width(660.0);
    ui.set_min_height(480.0);
    ui.heading("Preflight");
    ui.label("Inspect document structure, then select individual fixes. Full standards conformance is not established by these checks.");
    let before = app.preflight.settings.clone();
    ui.horizontal(|ui| {
        ui.label("Profile:");
        egui::ComboBox::from_id_salt("preflight-profile").selected_text(app.preflight.settings.profile.label()).show_ui(ui, |ui| {
            for profile in Profile::ALL {
                ui.selectable_value(&mut app.preflight.settings.profile, profile, profile.label());
            }
        });
    });
    if before.profile != app.preflight.settings.profile {
        app.preflight.settings.checks = None;
    }
    ui.horizontal(|ui| {
        ui.label("Check:");
        let selected = app.preflight.settings.checks.as_ref().and_then(|r| r.first()).map(|r| r.label()).unwrap_or("All profile checks");
        egui::ComboBox::from_id_salt("preflight-check").selected_text(selected).show_ui(ui, |ui| {
            ui.selectable_value(&mut app.preflight.settings.checks, None, "All profile checks");
            for rule in app.preflight.settings.profile.rules() {
                ui.selectable_value(&mut app.preflight.settings.checks, Some(vec![rule]), rule.label());
            }
        });
    });
    if before != app.preflight.settings {
        app.preflight.result = None;
        app.preflight.selected.clear();
    }
    let (mut analyze, mut fix, mut export, mut close) = (false, false, false, false);
    ui.horizontal(|ui| {
        analyze = ui.button("Analyze").clicked();
        fix = ui.add_enabled(!app.preflight.selected.is_empty(), egui::Button::new("Apply selected fixes")).clicked();
        export = ui.button("Export text report…").clicked();
        close = ui.button("Close").clicked();
    });
    ui.separator();
    let active = app.active_ids();
    let mut go_to = None;
    egui::ScrollArea::vertical().max_height(300.0).auto_shrink([false, false]).show(ui, |ui| {
        let Some(result) = app.preflight.result.as_ref() else {
            ui.label("Choose a profile and Analyze to inspect this document.");
            return;
        };
        if active.is_none_or(|(_, id)| id != result.document) {
            ui.label("These results belong to another document. Analyze this document first.");
            return;
        }
        ui.label(format!("Findings: {} · Objects checked: {}", result.report.findings.len(), result.report.objects_checked));
        if result.report.truncated {
            ui.colored_label(t.text_muted, "Report limits were reached; coverage is incomplete.");
        }
        if result.report.findings.is_empty() {
            ui.label("No issues found in the selected checks. This is not a conformance certificate.");
        }
        for rule in Rule::ALL {
            let findings = result.report.findings.iter().filter(|f| f.rule == rule).collect::<Vec<_>>();
            if findings.is_empty() {
                continue;
            }
            egui::CollapsingHeader::new(format!("{} ({})", rule.label(), findings.len())).default_open(true).id_salt(rule.id()).show(ui, |ui| {
                for f in findings {
                    ui.push_id(f.id, |ui| {
                        ui.label(format!("{}: {}", f.severity.id(), f.message));
                        if let Some(location) = &f.location {
                            ui.label(egui::RichText::new(location.display()).small().color(t.text_muted));
                            ui.horizontal_wrapped(|ui| {
                                for page in location.pages.iter().take(20) {
                                    if ui.link(format!("Page {}", page + 1)).clicked() {
                                        go_to = Some(*page);
                                    }
                                }
                            });
                        }
                        if let Some(fix) = f.fixup {
                            let mut selected = app.preflight.selected.contains(&f.id);
                            if ui.checkbox(&mut selected, fix.label()).changed() {
                                if selected {
                                    app.preflight.selected.insert(f.id);
                                } else {
                                    app.preflight.selected.remove(&f.id);
                                }
                            }
                        }
                        ui.add_space(4.0);
                    });
                }
            });
        }
    });
    if let Some(page) = go_to {
        if let Some((index, _)) = active
            && let Some(view) = app.views.get_mut(index)
        {
            view.go_to_page(page);
        }
        close = true;
    }
    if analyze {
        app.preflight_analyze();
    }
    if fix {
        app.preflight_fix_selected();
    }
    if export {
        app.preflight_export_report();
    }
    close
}

//! Redact → Redaction code sets: create, edit and delete the user's own code sets (beside the
//! built-in U.S. FOIA and Privacy Act sets), and import or export them as a JSON file. The sets
//! are the engine Session's, offered in Redaction Properties and saved with the app's settings.

use std::sync::Arc;

use egui::{Align, Layout};
use pdfcraft_engine::redaction_codes::{CustomCodeSet, MAX_CUSTOM_CODES, MAX_CUSTOM_DESCRIPTION_CHARS, MAX_CUSTOM_NAME_CHARS};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// The set being edited.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeSetsDraft {
    /// The saved set being edited (its name when it was opened); `None` for a new set.
    pub editing: Option<String>,
    pub name: String,
    /// (code, description) rows.
    pub codes: Vec<(String, String)>,
    /// The last refusal or report.
    pub message: Option<String>,
}

impl CodeSetsDraft {
    /// Edit the saved `set`.
    pub fn open(set: &CustomCodeSet) -> Self {
        Self {
            editing: Some(set.name.clone()),
            name: set.name.clone(),
            codes: set.codes.iter().map(|c| (c.code.clone(), c.description.clone())).collect(),
            message: None,
        }
    }

    /// The set as typed (empty rows dropped), or why it can't be saved.
    pub fn set(&self) -> Result<CustomCodeSet, String> {
        let rows: Vec<(String, String)> = self.codes.iter().filter(|(c, d)| !c.trim().is_empty() || !d.trim().is_empty()).cloned().collect();
        CustomCodeSet::new(&self.name, &rows)
    }
}

impl PdfCraftApp {
    /// Save the set being edited (renaming replaces the old one).
    pub fn save_code_set_draft(&mut self) {
        let set = match self.code_sets_draft.set() {
            Ok(s) => s,
            Err(e) => {
                self.code_sets_draft.message = Some(e);
                return;
            }
        };
        let old = self.code_sets_draft.editing.clone();
        let before = self.session.redaction_code_sets().to_vec();
        if let Some(old) = old.filter(|o| !o.eq_ignore_ascii_case(&set.name)) {
            self.session.delete_redaction_code_set(&old);
        }
        let name = set.name.clone();
        match self.session.save_redaction_code_set(set) {
            Ok(()) => {
                self.code_sets_draft.editing = Some(name.clone());
                self.code_sets_draft.message = Some(crate::i18n::fmt(tl!("Saved {name}."), &[("name", &name)]));
            }
            Err(e) => {
                // Put back a set deleted for a rename.
                for s in before {
                    let _ = self.session.save_redaction_code_set(s);
                }
                self.code_sets_draft.message = Some(e);
            }
        }
    }

    /// Import a code set file's bytes.
    pub fn import_code_sets(&mut self, bytes: &[u8]) {
        self.code_sets_draft.message = Some(match self.session.import_redaction_code_sets(bytes) {
            Ok(names) => crate::i18n::fmt(tl!("Imported {names}."), &[("names", &names.join(", "))]),
            Err(e) => e,
        });
    }

    /// The user's sets as a code set file.
    pub fn code_sets_file(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(&pdfcraft_engine::redaction_sets::code_sets_json(self.session.redaction_code_sets())).unwrap_or_default()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn pick_code_sets_file(&mut self) {
        let dialog = rfd::AsyncFileDialog::new().add_filter(tl!("Code set file"), &["json"]).set_title(tl!("Import code sets"));
        self.ask_one(crate::pickers::Ask::File(dialog), None, |app, path| {
            let max = pdfcraft_engine::redaction_sets::MAX_CODE_SET_FILE as u64;
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(u64::MAX);
            if size > max {
                app.code_sets_draft.message = Some(tl!("The file is too large for a code set file.").to_string());
                return;
            }
            match std::fs::read(&path) {
                Ok(bytes) => app.import_code_sets(&bytes),
                Err(e) => app.code_sets_draft.message = Some(e.to_string()),
            }
        });
    }
}

/// Draw the dialog; returns `true` to close.
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    ui.label(egui::RichText::new(tl!("Redaction code sets")).font(theme::semibold(18.0)));
    ui.label(
        egui::RichText::new(tl!("Your own codes to cite on redactions, offered in Redaction Properties beside the built-in sets."))
            .color(t.text_muted),
    );
    ui.add_space(6.0);
    let sets: Vec<String> = app.session.redaction_code_sets().iter().map(|s| s.name.clone()).collect();
    let mut open = None;
    let (mut import, mut export, mut new) = (false, false, false);
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(tl!("Saved sets")).font(theme::medium(13.0)));
        if sets.is_empty() {
            ui.label(egui::RichText::new(tl!("None yet")).color(t.text_faint));
        }
        for s in &sets {
            let on = app.code_sets_draft.editing.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(s));
            if ui.selectable_label(on, s).clicked() {
                open = Some(s.clone());
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        new = widgets::pill_button(ui, tl!("New set"), false).clicked();
        #[cfg(not(target_arch = "wasm32"))]
        {
            import = widgets::pill_button(ui, tl!("Import…"), false).clicked();
        }
        export = ui.add_enabled_ui(!sets.is_empty(), |ui| widgets::pill_button(ui, tl!("Export…"), false)).inner.clicked();
    });
    if let Some(name) = open
        && let Some(set) = app.session.redaction_code_set(&name)
    {
        app.code_sets_draft = CodeSetsDraft::open(set);
    }
    if new {
        app.code_sets_draft = CodeSetsDraft { codes: vec![(String::new(), String::new())], ..Default::default() };
    }
    if import {
        #[cfg(not(target_arch = "wasm32"))]
        app.pick_code_sets_file();
    }
    if export {
        let bytes = app.code_sets_file();
        app.write_files(&[("redaction-code-sets.json".to_string(), Arc::new(bytes))], tl!("Export code sets"));
    }
    ui.separator();
    let (mut save, mut delete) = (false, false);
    {
        let d = &mut app.code_sets_draft;
        if d.editing.is_none() && d.codes.is_empty() && d.name.is_empty() {
            ui.label(egui::RichText::new(tl!("Choose a saved set or make a new one.")).color(t.text_faint));
        } else {
            ui.horizontal(|ui| {
                let l = ui.label(tl!("Set name"));
                ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(260.0).char_limit(MAX_CUSTOM_NAME_CHARS).id_salt("code-set-name"))
                    .labelled_by(l.id);
            });
            ui.add_space(4.0);
            let mut remove = None;
            egui::ScrollArea::vertical().id_salt("code-set-codes").max_height(240.0).auto_shrink([false, true]).show(ui, |ui| {
                egui::Grid::new("code-set-grid").num_columns(3).spacing([8.0, 4.0]).show(ui, |ui| {
                    ui.label(egui::RichText::new(tl!("Code")).color(t.text_muted));
                    ui.label(egui::RichText::new(tl!("Description")).color(t.text_muted));
                    ui.label("");
                    ui.end_row();
                    for (i, (code, desc)) in d.codes.iter_mut().enumerate() {
                        ui.add(egui::TextEdit::singleline(code).desired_width(120.0).char_limit(MAX_CUSTOM_NAME_CHARS).id_salt(("code", i)));
                        ui.add(egui::TextEdit::singleline(desc).desired_width(300.0).char_limit(MAX_CUSTOM_DESCRIPTION_CHARS).id_salt(("desc", i)));
                        if ui.small_button("✕").on_hover_text(tl!("Remove this code")).clicked() {
                            remove = Some(i);
                        }
                        ui.end_row();
                    }
                });
            });
            if let Some(i) = remove
                && i < d.codes.len()
            {
                d.codes.remove(i);
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(d.codes.len() < MAX_CUSTOM_CODES, egui::Button::new(tl!("Add code"))).clicked() {
                    d.codes.push((String::new(), String::new()));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    save = widgets::pill_button(ui, tl!("Save set"), true).clicked();
                    if d.editing.is_some() {
                        delete = widgets::pill_button(ui, tl!("Delete set"), false).clicked();
                    }
                });
            });
        }
        if let Some(m) = &d.message {
            ui.label(egui::RichText::new(m).color(t.text_muted));
        }
    }
    if save {
        app.save_code_set_draft();
    }
    if delete && let Some(name) = app.code_sets_draft.editing.clone() {
        app.session.delete_redaction_code_set(&name);
        app.code_sets_draft = CodeSetsDraft { message: Some(crate::i18n::fmt(tl!("Deleted {name}."), &[("name", &name)])), ..Default::default() };
    }
    ui.add_space(8.0);
    let mut close = false;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        close = widgets::pill_button(ui, tl!("Done"), false).clicked();
    });
    close
}

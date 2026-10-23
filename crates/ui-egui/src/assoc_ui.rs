//! The Attachments panel's Associated files section (PDF 2.0, ISO 32000-2 §14.13): every file
//! associated with the document, a page or an annotation, with its relationship (Source, Data,
//! Alternative, …), size and description; add one to the document or the current page with a
//! relationship, save one, or remove it. Each add and remove is one undo step. The list is read
//! again only when the document changes while the panel is open.

use std::sync::Arc;

use pdfcraft_engine::{AfRelationship, AfSelector, AfTarget, AssociatedFiles, DocId, Edit, NewAssociatedFile};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons};

/// Where a new associated file goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Owner {
    #[default]
    Document,
    CurrentPage,
}

/// Every owner's associated files, or why they couldn't be read.
type Listing = Result<Vec<(AfTarget, AssociatedFiles)>, String>;

/// The section's state.
#[derive(Default)]
pub struct AssocFiles {
    /// The listing, for (document, edit generation).
    cache: Option<(DocId, u64, Listing)>,
    pub relationship: Option<AfRelationship>,
    pub description: String,
    pub owner: Owner,
    /// Tests and automation: the file Add file… takes instead of asking.
    pub file_override: Option<(String, Vec<u8>)>,
}

/// What the section asked for.
pub enum AssocAction {
    Add,
    Save(AfTarget, usize),
    Remove(AfTarget, usize),
}

/// Files listed at most (the rest are counted).
const MAX_ROWS: usize = 500;

impl AssocFiles {
    /// The listing for `doc`, read again when the document changed.
    fn listing(&mut self, doc: &pdfcraft_engine::Document) -> &Listing {
        let generation = doc.edit_generation();
        if !self.cache.as_ref().is_some_and(|(d, g, _)| *d == doc.id && *g == generation) {
            let r = doc.all_associated_files().map_err(|e| e.to_string());
            self.cache = Some((doc.id, generation, r));
        }
        match &self.cache {
            Some((_, _, r)) => r,
            None => &EMPTY,
        }
    }

    /// For the control channel: every owner's files (read on request).
    pub fn state(doc: Option<&pdfcraft_engine::Document>) -> serde_json::Value {
        let Some(doc) = doc else { return serde_json::Value::Null };
        match &doc.all_associated_files().map_err(|e| e.to_string()) {
            Ok(list) => serde_json::json!(list
                .iter()
                .map(|(t, files)| serde_json::json!({
                    "owner": owner_label(t),
                    "files": files.files.iter().map(|f| serde_json::json!({ "name": f.name, "relationship": f.relationship, "size": f.size, "description": f.description, "mime": f.mime })).collect::<Vec<_>>(),
                    "issues": files.issues,
                }))
                .collect::<Vec<_>>()),
            Err(e) => serde_json::json!({ "error": e }),
        }
    }
}

static EMPTY: Listing = Ok(Vec::new());

fn owner_label(t: &AfTarget) -> String {
    match t {
        AfTarget::Document => tl!("Document").to_string(),
        AfTarget::Page(p) => crate::i18n::fmt(tl!("Page {label}"), &[("label", &(p + 1).to_string())]),
        AfTarget::Annotation { page, .. } => crate::i18n::fmt(tl!("Comment on page {label}"), &[("label", &(page + 1).to_string())]),
        AfTarget::Object(n) => crate::i18n::fmt(tl!("Object {n}"), &[("n", &n.to_string())]),
    }
}

/// The section, below the attachments. Returns what was asked for.
pub(crate) fn section(ui: &mut egui::Ui, t: &Tokens, state: &mut AssocFiles, doc: &pdfcraft_engine::Document, editable: bool) -> Option<AssocAction> {
    let mut action = None;
    ui.add_space(10.0);
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Associated files")).font(theme::semibold(13.5)));
        ui.label(egui::RichText::new("PDF 2.0").small().color(t.text_faint))
            .on_hover_text(tl!("Files linked to the document, a page or a comment, each with its relationship (source, data, alternative…)."));
    });
    let listing = state.listing(doc).clone();
    match &listing {
        Err(e) => {
            ui.label(egui::RichText::new(e).small().color(theme::DANGER));
        }
        Ok(list) if list.iter().all(|(_, f)| f.files.is_empty()) => {
            ui.label(egui::RichText::new(tl!("No associated files.")).small().color(t.text_muted));
        }
        Ok(list) => {
            let mut rows = 0usize;
            for (target, files) in list {
                for issue in files.issues.iter().take(5) {
                    ui.label(egui::RichText::new(issue).small().color(t.text_muted));
                }
                for f in &files.files {
                    rows += 1;
                    if rows > MAX_ROWS {
                        break;
                    }
                    ui.push_id(("af", owner_label(target), f.index), |ui| {
                        ui.horizontal(|ui| {
                            ui.add(icons::image("file-symlink", 16.0, t.icon));
                            ui.vertical(|ui| {
                                ui.set_width(ui.available_width() - 64.0);
                                let name = if f.name.is_empty() { tl!("(no name)").to_string() } else { f.name.clone() };
                                ui.add(egui::Label::new(egui::RichText::new(name).font(theme::medium(13.0))).truncate());
                                let rel = f.relationship.clone().unwrap_or_else(|| "Unspecified".into());
                                let mut meta = format!("{}  ·  {}", tl!(rel_label(&rel)), owner_label(target));
                                if let Some(s) = f.size {
                                    meta = format!("{meta}  ·  {}", crate::panels::human_size(usize::try_from(s).unwrap_or(usize::MAX)));
                                }
                                if let Some(d) = f.description.as_deref().filter(|d| !d.is_empty()) {
                                    meta = format!("{meta}  ·  {d}");
                                }
                                ui.add(egui::Label::new(egui::RichText::new(meta).small().color(t.text_faint)).truncate());
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if editable && icons::button(ui, "trash-2", 26.0, false, tl!("Remove associated file")).clicked() {
                                    action = Some(AssocAction::Remove(target.clone(), f.index));
                                }
                                if f.embedded && icons::button(ui, "file-down", 26.0, false, tl!("Save associated file…")).clicked() {
                                    action = Some(AssocAction::Save(target.clone(), f.index));
                                }
                            });
                        });
                    });
                }
            }
            if rows > MAX_ROWS {
                ui.label(
                    egui::RichText::new(crate::i18n::fmt(tl!("{n} more not shown"), &[("n", &(rows - MAX_ROWS).to_string())]))
                        .small()
                        .color(t.text_muted),
                );
            }
        }
    }
    if editable {
        ui.add_space(6.0);
        egui::Frame::new().fill(t.hover).corner_radius(egui::CornerRadius::same(6)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(tl!("Add an associated file")).font(theme::medium(12.5)));
            ui.horizontal(|ui| {
                ui.radio_value(&mut state.owner, Owner::Document, tl!("To the document"));
                ui.radio_value(&mut state.owner, Owner::CurrentPage, tl!("To this page"));
            });
            let rel = state.relationship.unwrap_or(AfRelationship::Unspecified);
            ui.horizontal(|ui| {
                ui.label(tl!("Relationship:"));
                egui::ComboBox::from_id_salt("af-relationship").selected_text(tl!(rel_label(rel.name()))).show_ui(ui, |ui| {
                    for r in AfRelationship::ALL {
                        if ui.selectable_label(rel == r, tl!(rel_label(r.name()))).clicked() {
                            state.relationship = Some(r);
                        }
                    }
                });
            });
            ui.add(
                egui::TextEdit::singleline(&mut state.description)
                    .hint_text(tl!("Description (optional)"))
                    .desired_width(f32::INFINITY)
                    .id_salt("af-description"),
            );
            if crate::widgets::pill_button(ui, tl!("Add file…"), true).clicked() {
                action = Some(AssocAction::Add);
            }
        });
    }
    action
}

/// The relationship as people read it.
fn rel_label(name: &str) -> &str {
    match name {
        "Source" => "Source",
        "Data" => "Data",
        "Alternative" => "Alternative",
        "Supplement" => "Supplement",
        "EncryptedPayload" => "Encrypted payload",
        "FormData" => "Form data",
        "Schema" => "Schema",
        _ => "Unspecified",
    }
}

impl PdfCraftApp {
    /// Handle what the Associated files section asked for.
    pub(crate) fn assoc_action(&mut self, action: AssocAction) {
        let Some((i, id)) = self.active_ids() else { return };
        match action {
            AssocAction::Remove(target, index) => {
                self.apply_edit(Edit::RemoveAssociatedFile { target, select: AfSelector::Index(index) });
            }
            AssocAction::Save(target, index) => {
                let Some(doc) = self.session.get(id) else { return };
                match doc.associated_file_data(
                    &target,
                    &AfSelector::Index(index),
                    pdfcraft_engine::associated_files::MAX_FILE_BYTES.saturating_mul(4),
                ) {
                    Ok((info, data)) => {
                        let name = if info.name.trim().is_empty() { "associated-file".to_string() } else { info.name.clone() };
                        self.write_files(&[(name, Arc::new(data))], "Save associated file");
                    }
                    Err(e) => self.notify_error(e),
                }
            }
            AssocAction::Add => {
                let page = self.views.get(i).map_or(0, |v| v.current);
                let target = match self.assoc.owner {
                    Owner::Document => AfTarget::Document,
                    Owner::CurrentPage => AfTarget::Page(page),
                };
                match self.assoc.file_override.take() {
                    Some((name, data)) => self.add_associated_file(id, target, name, data),
                    #[cfg(not(target_arch = "wasm32"))]
                    None => {
                        let dialog = rfd::AsyncFileDialog::new().set_title(tl!("Add an associated file"));
                        self.ask_one(crate::pickers::Ask::File(dialog), Some(id), move |app, p| {
                            let max = pdfcraft_engine::associated_files::MAX_FILE_BYTES as u64;
                            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            match std::fs::metadata(&p) {
                                Ok(m) if m.len() > max => {
                                    app.notify_fmt("{name} is larger than {n} MB", &[("name", &name), ("n", &(max >> 20).to_string())])
                                }
                                Ok(_) => match std::fs::read(&p) {
                                    Ok(data) => app.add_associated_file(id, target, name, data),
                                    Err(e) => app.notify_fmt("Couldn't read {name}: {e}", &[("name", &name), ("e", &e.to_string())]),
                                },
                                Err(e) => app.notify_fmt("Couldn't read {name}: {e}", &[("name", &name), ("e", &e.to_string())]),
                            }
                        });
                    }
                    #[cfg(target_arch = "wasm32")]
                    None => {}
                }
            }
        }
    }

    /// Associate `data` as `name` with `target` of document `id` (one undo step).
    fn add_associated_file(&mut self, id: DocId, target: AfTarget, name: String, data: Vec<u8>) {
        if self.active_ids().map(|(_, a)| a) != Some(id) {
            return self.notify_tr("The document changed; add the file again");
        }
        let description = Some(self.assoc.description.trim().to_string()).filter(|d| !d.is_empty());
        let mime = pdfcraft_engine::associated_files::guess_mime(&name).map(str::to_string);
        let file = NewAssociatedFile {
            name,
            data: Arc::new(data),
            mime,
            relationship: self.assoc.relationship.unwrap_or(AfRelationship::Unspecified),
            description,
        };
        if let Err(e) = file.validate() {
            return self.notify_error(e);
        }
        if self.apply_edit(Edit::AddAssociatedFile { target, file }) {
            self.assoc.description.clear();
        }
    }
}

//! Preferences → Signatures, below the plain settings: trusted certificates with what each is
//! trusted for, the timestamp servers, and the saved signature appearances. Every change goes
//! through the session (the same calls the automation tools make) and is saved with the app's
//! settings.

use egui::{Color32, CornerRadius, Stroke};
use pdfcraft_engine::sign::CertTrust;
use pdfcraft_engine::signature_settings::AppearanceGraphic;

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// A card around one list entry.
fn card(ui: &mut egui::Ui, t: &Tokens, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
    ui.add_space(4.0);
}

/// What a trusted certificate row asks for.
enum CertRowAction {
    Set(String, CertTrust),
    Remove(String),
    /// Show the trusted certificate at this index.
    View(usize),
}

pub(crate) fn section(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) {
    ui.add_space(10.0);
    widgets::section_title(ui, tl!("Trusted certificates"));
    ui.label(
        egui::RichText::new(tl!("Signatures whose certificates chain to one of these are trusted. Choose what each one is trusted for."))
            .small()
            .color(t.text_muted),
    );
    // What the cards show, read without copying the store (this page may stay open).
    let rows: Vec<(String, String, String, CertTrust)> = {
        let store = app.session.trust_store();
        store
            .certs
            .iter()
            .map(|c| {
                let issuer = c.issuer.common_name().map(str::to_string).unwrap_or_else(|| c.issuer.display());
                let line = crate::i18n::fmt(tl!("Issued by {issuer}, valid to {date}"), &[("issuer", &issuer), ("date", &c.not_after.to_string())]);
                (c.fingerprint(), c.display_name(), line, store.trust_of(c))
            })
            .collect()
    };
    let mut action = None;
    if rows.is_empty() {
        ui.label(egui::RichText::new(tl!("No trusted certificates yet.")).color(t.text_muted));
    }
    for (i, (fp, name, line, trust)) in rows.into_iter().enumerate() {
        ui.push_id(("trusted-cert", i), |ui| {
            card(ui, t, |ui| {
                ui.label(egui::RichText::new(name).font(theme::semibold(13.0)));
                ui.label(egui::RichText::new(line).small().color(t.text_muted));
                let mut next = trust;
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut next.root, tl!("Trusted root"));
                    ui.checkbox(&mut next.signatures, tl!("Signed documents"));
                    ui.checkbox(&mut next.certified_documents, tl!("Certified documents"));
                });
                if next != trust {
                    action = Some(CertRowAction::Set(fp.clone(), next));
                }
                ui.horizontal(|ui| {
                    if ui.button(tl!("Show certificate…")).clicked() {
                        action = Some(CertRowAction::View(i));
                    }
                    if ui.button(tl!("Remove")).clicked() {
                        action = Some(CertRowAction::Remove(fp.clone()));
                    }
                });
            });
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    if widgets::pill_button(ui, tl!("Import certificates…"), false).clicked() {
        app.pick_trusted_certificates();
    }
    match action {
        Some(CertRowAction::Set(fp, trust)) => {
            if let Err(e) = app.session.set_certificate_trust(&fp, trust) {
                app.notify(e);
            }
        }
        Some(CertRowAction::Remove(fp)) => {
            app.session.remove_trusted_certificate(&fp);
        }
        Some(CertRowAction::View(i)) => {
            if let Some(c) = app.session.trusted_certificates().get(i).cloned() {
                app.cert_viewer = Some(crate::sign_ui::CertViewer { chain: vec![c], selected: 0, tab: crate::sign_ui::CertTab::Trust });
                app.dialog = Some(crate::Dialog::CertificateViewer);
            }
        }
        None => {}
    }

    ui.add_space(10.0);
    widgets::section_title(ui, tl!("Timestamp servers"));
    crate::timestamp_ui::servers(ui, app);

    ui.add_space(10.0);
    widgets::section_title(ui, tl!("Signature appearances"));
    ui.label(
        egui::RichText::new(tl!("Save an appearance from the signing dialog: its text, a picture or drawn signature, and a logo."))
            .small()
            .color(t.text_muted),
    );
    let mut delete = None;
    if app.session.signature_appearances().is_empty() {
        ui.label(egui::RichText::new(tl!("No saved appearances yet.")).color(t.text_muted));
    }
    for (i, a) in app.session.signature_appearances().iter().enumerate() {
        ui.push_id(("appearance", i), |ui| {
            card(ui, t, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&a.name).font(theme::semibold(13.0)));
                    let what = match &a.graphic {
                        Some(AppearanceGraphic::Image(_)) => tl!("Picture"),
                        Some(AppearanceGraphic::Drawn(_)) => tl!("Drawn signature"),
                        None => tl!("Text only"),
                    };
                    ui.label(egui::RichText::new(what).small().color(t.text_muted));
                    if a.logo.is_some() {
                        ui.label(egui::RichText::new(tl!("Logo")).small().color(t.text_muted));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(tl!("Delete")).clicked() {
                            delete = Some(a.name.clone());
                        }
                    });
                });
            });
        });
    }
    if let Some(name) = delete {
        app.session.delete_signature_appearance(&name);
    }
    if let Some(e) = &app.signature_workflow.error {
        ui.colored_label(Color32::from_rgb(0xD7, 0x37, 0x3F), e);
    }
}

impl PdfCraftApp {
    /// Trust the certificates in the file at `path` (.cer/.crt/.pem/.der); revalidates.
    pub fn import_trusted_certificates(&mut self, path: &std::path::Path) -> Result<usize, String> {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(tl!("A certificate file is limited to 4 MiB.").to_string());
        }
        let found = pdfcraft_engine::sign::x509::load_certificates(&bytes).map_err(|e| e.to_string())?;
        let mut certs = self.session.trusted_certificates().to_vec();
        let mut added = 0;
        for c in found.into_iter().take(256) {
            if !certs.iter().any(|t| t.raw == c.raw) {
                certs.push(c);
                added += 1;
            }
        }
        self.session.set_trusted_certificates(certs);
        Ok(added)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn pick_trusted_certificates(&mut self) {
        let dialog = rfd::AsyncFileDialog::new().add_filter(tl!("Certificates"), &["cer", "crt", "pem", "der"]);
        let _ = self.ask_one(crate::pickers::Ask::File(dialog), None, |app, path| match app.import_trusted_certificates(&path) {
            Ok(n) => app.notify(crate::i18n::fmt(tl!("{n} certificates added to your trusted certificates."), &[("n", &n.to_string())])),
            Err(e) => app.notify_error(e),
        });
    }
}

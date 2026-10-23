//! Protect → Encrypt with certificate: the document opens only with a recipient's private key,
//! and each recipient gets their own permissions (printing, changes, copying, screen readers, or
//! changing the security). Recipients come from certificate files (.cer, .crt, .pem, .der) or
//! digital ID files (.p12, .pfx, with their password). ISO 32000-2 §7.6.5.

use egui::{Align, Layout};
use pdfcraft_engine::sign::{self, Certificate};
use pdfcraft_engine::{Algorithm, CertificateProtection, CertificateRecipient, Changes, Edit, MAX_CERTIFICATE_RECIPIENTS, Printing, RecipientAccess};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// The most bytes read from one certificate or digital ID file.
pub const MAX_CERTIFICATE_FILE: usize = 1 << 20;

/// The dialog's state while it is open.
#[derive(Clone, Debug, PartialEq)]
pub struct CertDraft {
    pub recipients: Vec<CertificateRecipient>,
    /// The recipient whose permissions are shown.
    pub selected: usize,
    pub algorithm: Algorithm,
    pub encrypt_metadata: bool,
    /// The password for the next digital ID file added.
    pub id_password: String,
    /// The last file that couldn't be added, and why.
    pub error: Option<String>,
}

impl Default for CertDraft {
    fn default() -> Self {
        Self { recipients: Vec::new(), selected: 0, algorithm: Algorithm::Aes256, encrypt_metadata: true, id_password: String::new(), error: None }
    }
}

/// The recipient certificate in a file: a certificate file (PEM or DER, one certificate), or
/// the certificate of a digital ID file opened with `password`.
pub fn certificate_from_bytes(bytes: &[u8], password: &str) -> Result<Certificate, String> {
    if bytes.len() > MAX_CERTIFICATE_FILE {
        return Err(crate::i18n::fmt(tl!("The file is larger than {n} MB."), &[("n", &(MAX_CERTIFICATE_FILE >> 20).to_string())]));
    }
    if let Ok(mut certs) = sign::x509::load_certificates(bytes)
        && !certs.is_empty()
    {
        if certs.len() > 1 {
            return Err(crate::i18n::fmt(
                tl!("The file holds {n} certificates; add each recipient from its own file."),
                &[("n", &certs.len().to_string())],
            ));
        }
        return Ok(certs.remove(0));
    }
    match sign::pkcs12::open(bytes, password) {
        Ok(id) => Ok(id.certificate),
        Err(sign::SignError::WrongPassword) => Err(tl!("Type the digital ID's password first (or choose a certificate file).").to_string()),
        Err(_) => Err(tl!("This isn't a certificate (.cer, .crt, .pem, .der) or digital ID (.p12, .pfx).").to_string()),
    }
}

impl CertDraft {
    /// Add the certificate in `bytes`; `Err` says why it wasn't.
    pub fn add(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.recipients.len() >= MAX_CERTIFICATE_RECIPIENTS {
            return Err(crate::i18n::fmt(tl!("At most {n} recipients."), &[("n", &MAX_CERTIFICATE_RECIPIENTS.to_string())]));
        }
        let certificate = certificate_from_bytes(bytes, &self.id_password)?;
        if let Some(i) = self.recipients.iter().position(|r| r.certificate == certificate) {
            self.selected = i;
            return Err(tl!("That recipient is already on the list.").to_string());
        }
        self.recipients.push(CertificateRecipient { certificate, access: RecipientAccess::default() });
        self.selected = self.recipients.len() - 1;
        self.id_password.clear();
        Ok(())
    }

    /// Why Apply is disabled, if it is.
    pub fn problem(&self) -> Option<&'static str> {
        if self.recipients.is_empty() {
            Some("Add at least one recipient certificate.")
        } else if !self.recipients.iter().any(|r| r.access.change_security) {
            Some("Nobody can change or remove this security later unless a recipient may change the security.")
        } else {
            None
        }
    }

    /// The edit Apply makes.
    pub fn edit(&self) -> Edit {
        Edit::ProtectCertificates(CertificateProtection {
            recipients: self.recipients.clone(),
            algorithm: self.algorithm,
            encrypt_metadata: self.encrypt_metadata,
        })
    }
}

impl PdfCraftApp {
    /// Add a recipient from a file's bytes (the picker, or a test), telling the user if it can't be.
    pub fn add_certificate_recipient(&mut self, bytes: &[u8]) {
        let r = self.cert_draft.add(bytes);
        self.cert_draft.error = r.err();
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn pick_certificate_recipient(&mut self) {
        let dialog = rfd::AsyncFileDialog::new()
            .add_filter(tl!("Certificate or digital ID"), &["cer", "crt", "pem", "der", "p7b", "p12", "pfx"])
            .set_title(tl!("Add recipient"));
        self.ask_one(crate::pickers::Ask::File(dialog), None, |app, path| {
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(u64::MAX);
            if size > MAX_CERTIFICATE_FILE as u64 {
                app.cert_draft.error =
                    Some(crate::i18n::fmt(tl!("The file is larger than {n} MB."), &[("n", &(MAX_CERTIFICATE_FILE >> 20).to_string())]));
                return;
            }
            match std::fs::read(&path) {
                Ok(bytes) => app.add_certificate_recipient(&bytes),
                Err(e) => app.cert_draft.error = Some(e.to_string()),
            }
        });
    }
}

fn recipient_name(c: &Certificate) -> String {
    let name = c.display_name();
    match c.subject.email() {
        Some(e) if !name.contains(e) => format!("{name} <{e}>"),
        _ => name,
    }
}

/// Draw the dialog; returns `true` to close.
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    ui.label(egui::RichText::new(tl!("Encrypt with certificate")).font(theme::semibold(18.0)));
    ui.label(
        egui::RichText::new(tl!("Only the recipients can open the document, each with the permissions you give them. It's applied when you save."))
            .color(t.text_muted),
    );
    ui.add_space(6.0);
    ui.separator();
    let mut add = false;
    let mut remove = None;
    {
        let d = &mut app.cert_draft;
        ui.label(egui::RichText::new(tl!("Recipients")).font(theme::medium(13.0)));
        egui::ScrollArea::vertical().id_salt("cert-recipients").max_height(140.0).auto_shrink([false, true]).show(ui, |ui| {
            if d.recipients.is_empty() {
                ui.label(
                    egui::RichText::new(tl!("No recipients yet. Add a certificate for each person who may open the document.")).color(t.text_faint),
                );
            }
            for (i, r) in d.recipients.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.selectable_label(d.selected == i, recipient_name(&r.certificate)).clicked() {
                        d.selected = i;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button(tl!("Remove")).clicked() {
                            remove = Some(i);
                        }
                    });
                });
            }
        });
        ui.horizontal_wrapped(|ui| {
            if widgets::pill_button(ui, tl!("Add recipient…"), false).clicked() {
                add = true;
            }
            ui.label(egui::RichText::new(tl!("Digital ID password")).color(t.text_muted));
            ui.add(egui::TextEdit::singleline(&mut d.id_password).password(true).desired_width(150.0).char_limit(256).id_salt("cert-id-password"));
        });
        if let Some(e) = &d.error {
            ui.label(egui::RichText::new(e).color(egui::Color32::from_rgb(0xD3, 0x2F, 0x2F)));
        }
    }
    if let Some(i) = remove {
        let d = &mut app.cert_draft;
        if i < d.recipients.len() {
            d.recipients.remove(i);
        }
        d.selected = d.selected.min(d.recipients.len().saturating_sub(1));
    }
    if add {
        #[cfg(not(target_arch = "wasm32"))]
        app.pick_certificate_recipient();
    }
    ui.add_space(6.0);
    let d = &mut app.cert_draft;
    let sel = d.selected;
    if let Some(r) = d.recipients.get_mut(sel) {
        widgets::section_title(ui, &crate::i18n::fmt(tl!("Permissions for {name}"), &[("name", &r.certificate.display_name())]));
        let a = &mut r.access;
        egui::Grid::new("cert-permissions").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.add_enabled_ui(!a.change_security, |ui| ui.label(tl!("Printing allowed")));
            ui.add_enabled_ui(!a.change_security, |ui| {
                let opts =
                    [(Printing::None, tl!("None")), (Printing::Low, tl!("Low Resolution (150 dpi)")), (Printing::High, tl!("High Resolution"))];
                let cur = opts.iter().find(|(o, _)| *o == a.printing).map_or(tl!("High Resolution"), |(_, l)| *l);
                egui::ComboBox::from_id_salt("cert-print").width(300.0).selected_text(cur).show_ui(ui, |ui| {
                    for (o, l) in opts {
                        ui.selectable_value(&mut a.printing, o, l);
                    }
                });
            });
            ui.end_row();
            ui.add_enabled_ui(!a.change_security, |ui| ui.label(tl!("Changes allowed")));
            ui.add_enabled_ui(!a.change_security, |ui| {
                let opts = [
                    (Changes::None, tl!("None")),
                    (Changes::Pages, tl!("Inserting, deleting, and rotating pages")),
                    (Changes::FillSign, tl!("Filling in form fields and signing existing signature fields")),
                    (Changes::CommentFillSign, tl!("Commenting, filling in form fields, and signing existing signature fields")),
                    (Changes::AnyExceptExtract, tl!("Any except extracting pages")),
                ];
                let cur = opts.iter().find(|(o, _)| *o == a.changes).map_or(tl!("None"), |(_, l)| *l);
                egui::ComboBox::from_id_salt("cert-changes").width(300.0).selected_text(cur).show_ui(ui, |ui| {
                    for (o, l) in opts {
                        ui.selectable_value(&mut a.changes, o, l);
                    }
                });
            });
            ui.end_row();
            ui.label("");
            ui.vertical(|ui| {
                ui.add_enabled_ui(!a.change_security, |ui| {
                    ui.checkbox(&mut a.copy, tl!("Enable copying of text, images, and other content"));
                    ui.checkbox(&mut a.accessibility, tl!("Enable text access for screen reader devices for the visually impaired"));
                });
                ui.checkbox(&mut a.change_security, tl!("Allow changing the security settings (and everything else)"));
            });
            ui.end_row();
        });
    }
    ui.add_space(6.0);
    egui::Grid::new("cert-options").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label(tl!("Compatibility"));
        let algos = [(Algorithm::Aes256, tl!("Acrobat X and later (256-bit AES)")), (Algorithm::Aes128, tl!("Acrobat 7.0 and later (128-bit AES)"))];
        let cur = algos.iter().find(|(a, _)| *a == d.algorithm).map_or(algos[0].1, |(_, l)| *l);
        egui::ComboBox::from_id_salt("cert-algo").width(300.0).selected_text(cur).show_ui(ui, |ui| {
            for (a, l) in algos {
                ui.selectable_value(&mut d.algorithm, a, l);
            }
        });
        ui.end_row();
        ui.label(tl!("Encrypt"));
        ui.vertical(|ui| {
            ui.radio_value(&mut d.encrypt_metadata, true, tl!("All document contents"));
            ui.radio_value(&mut d.encrypt_metadata, false, tl!("All document contents except metadata"));
        });
        ui.end_row();
    });
    ui.add_space(8.0);
    let problem = d.problem();
    if let Some(p) = problem {
        ui.label(egui::RichText::new(tl!(p)).color(t.text_muted).font(theme::regular(12.0)));
    }
    ui.add_space(8.0);
    let (mut apply, mut cancel) = (false, false);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        // Nobody able to change the security is a warning, not a refusal (Acrobat allows it).
        if ui.add_enabled_ui(!d.recipients.is_empty(), |ui| widgets::pill_button(ui, tl!("Apply"), true)).inner.clicked() {
            apply = true;
        }
        if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
            cancel = true;
        }
    });
    if apply {
        let edit = d.edit();
        if app.apply_edit(edit) {
            app.notify_tr("Certificate security will be applied when you save");
            app.cert_draft = CertDraft::default();
            return true;
        }
        return false;
    }
    if cancel {
        app.cert_draft = CertDraft::default();
    }
    cancel
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_files_are_refused_without_panicking() {
        let mut d = CertDraft::default();
        for bytes in [&b""[..], b"-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----", &[0x30, 0x82, 0xff, 0xff], &[0u8; 64]] {
            assert!(d.add(bytes).is_err());
        }
        assert!(d.add(&vec![0u8; MAX_CERTIFICATE_FILE + 1]).is_err());
        assert!(d.recipients.is_empty());
        assert_eq!(d.problem(), Some("Add at least one recipient certificate."));
    }
}

//! Submit Form: a button's submit-form action (or `this.submitForm`) asks first. The dialog
//! names the address and what would be sent; Send posts it, Save Data As writes it to a file
//! instead, and Cancel sends nothing.

use std::sync::{Arc, Mutex};

use egui::{Align, Color32, Layout};
use pdfcraft_engine::DocId;
use pdfcraft_engine::submit::{Reply, Submission};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// Where a background send leaves its result.
type SendSlot = Arc<Mutex<Option<Result<Reply, String>>>>;

/// A pending Submit Form request.
pub struct SubmitDraft {
    pub doc: DocId,
    /// The button that asked, or empty for a script.
    pub button: String,
    pub submission: Submission,
    /// Why it can't be sent (no web address, …); Save Data As still works.
    pub problem: Option<String>,
    /// Fields that would be sent, by name.
    pub fields: Vec<String>,
    /// A send in progress: its result, once it arrives.
    pub sending: Option<SendSlot>,
    pub error: Option<String>,
}

impl PdfCraftApp {
    /// Ask whether to submit document `doc`'s form as `submission` (from `button`, or a script).
    pub fn start_submit(&mut self, doc: DocId, button: &str, submission: Submission) {
        let problem = pdfcraft_engine::submit::destination(&submission.url).err().or_else(|| {
            // Internet access settings that block the host: Save Data As still works.
            let access = self.session.check_url(&submission.url);
            (access.decision == pdfcraft_engine::url_access::UrlDecision::Block).then(|| format!("Not sent: {}.", access.reason))
        });
        #[cfg(target_arch = "wasm32")]
        let problem = problem.or_else(|| Some("Sending forms isn't available in the browser version. Save the data instead.".to_string()));
        let fields =
            self.session.get(doc).map(|d| d.form.iter().filter(|f| submission.sends(f)).map(|f| f.name.clone()).collect()).unwrap_or_default();
        self.submit_draft = Some(SubmitDraft { doc, button: button.to_string(), submission, problem, fields, sending: None, error: None });
        self.dialog = Some(crate::Dialog::SubmitForm);
    }

    /// Send the pending submission (Send in the dialog): in the background, unless `run_inline`.
    pub fn send_submission(&mut self) {
        let Some(d) = self.submit_draft.as_ref() else { return };
        if d.sending.is_some() || d.problem.is_some() {
            return;
        }
        let payload = match self.session.submission(d.doc, &d.submission) {
            Ok(p) => p,
            Err(e) => {
                if let Some(d) = self.submit_draft.as_mut() {
                    d.error = Some(e.to_string());
                }
                return;
            }
        };
        let slot: SendSlot = Arc::new(Mutex::new(None));
        let result = slot.clone();
        let ctx = self.ctx.clone();
        let task = move || {
            let done = pdfcraft_engine::guard(|| pdfcraft_engine::submit::send(&payload)).and_then(|r| r);
            if let Ok(mut slot) = result.lock() {
                *slot = Some(done);
            }
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        if self.run_inline {
            task();
        } else if let Err(e) = std::thread::Builder::new().name("pdfcraft-submit".into()).spawn(task) {
            if let Some(d) = self.submit_draft.as_mut() {
                d.error = Some(e.to_string());
            }
            return;
        }
        #[cfg(target_arch = "wasm32")]
        task();
        if let Some(d) = self.submit_draft.as_mut() {
            d.sending = Some(slot);
            d.error = None;
        }
        self.poll_submit();
    }

    /// Finish a send once its result is in: report it and close the dialog, or show the error.
    pub(crate) fn poll_submit(&mut self) {
        let Some(slot) = self.submit_draft.as_ref().and_then(|d| d.sending.clone()) else { return };
        let done = match slot.lock() {
            Ok(mut r) => r.take(),
            Err(_) => Some(Err("The submission's state is unavailable.".into())),
        };
        let Some(done) = done else {
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            return;
        };
        let host = self
            .submit_draft
            .as_ref()
            .and_then(|d| pdfcraft_engine::submit::destination(&d.submission.url).ok())
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default();
        match done {
            Ok(reply) if (200..300).contains(&reply.status) => {
                self.submit_draft = None;
                if self.dialog == Some(crate::Dialog::SubmitForm) {
                    self.dialog = None;
                }
                self.notify_fmt("The form was sent to {host}.", &[("host", &host)]);
            }
            Ok(reply) => {
                if let Some(d) = self.submit_draft.as_mut() {
                    d.sending = None;
                    d.error = Some(crate::i18n::fmt(
                        tl!("{host} answered with HTTP {status}; the form may not have been received."),
                        &[("host", &host), ("status", &reply.status.to_string())],
                    ));
                }
            }
            Err(e) => {
                if let Some(d) = self.submit_draft.as_mut() {
                    d.sending = None;
                    d.error = Some(e);
                }
            }
        }
    }

    /// Save Data As: write what would be sent to a file instead.
    pub fn save_submission(&mut self) {
        let Some(d) = self.submit_draft.as_ref() else { return };
        let stem = self.session.get(d.doc).map(|doc| doc.name.trim_end_matches(".pdf").to_string()).unwrap_or_else(|| "form".into());
        match self.session.submission(d.doc, &d.submission) {
            Ok(p) => {
                let name = format!("{stem} data.{}", d.submission.format.extension());
                self.submit_draft = None;
                self.dialog = None;
                self.write_files(&[(name, Arc::new(p.body))], "Save form data");
            }
            Err(e) => {
                if let Some(d) = self.submit_draft.as_mut() {
                    d.error = Some(e.to_string());
                }
            }
        }
    }
}

/// The Submit Form dialog; returns `true` when it should close.
pub(crate) fn dialog(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    let Some(d) = app.submit_draft.as_ref() else { return true };
    ui.set_width(520.0);
    ui.label(egui::RichText::new(tl!("Submit Form")).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    let url = d.submission.url.trim();
    let host = pdfcraft_engine::submit::destination(url).ok().and_then(|u| u.host_str().map(str::to_string));
    match &host {
        Some(h) => ui.label(crate::i18n::fmt(tl!("This form wants to send its data to {host}:"), &[("host", h)])),
        None => ui.label(tl!("This form wants to send its data to:")),
    };
    ui.add(egui::Label::new(egui::RichText::new(url).font(theme::regular(12.0)).color(t.text_muted)).wrap());
    ui.add_space(6.0);
    let lang = crate::i18n::current();
    let count = crate::i18n::trn(lang, d.fields.len() as u64, "{n} field", "{n} fields");
    let what = crate::i18n::fmt(tl!("{fields} as {format}"), &[("fields", &count), ("format", d.submission.format.label())]);
    let what = if d.submission.format == pdfcraft_engine::submit::SubmitFormat::Pdf { tl!("The whole document (PDF)").to_string() } else { what };
    ui.label(what);
    if !d.fields.is_empty() && d.submission.format != pdfcraft_engine::submit::SubmitFormat::Pdf {
        let mut names = d.fields.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
        if d.fields.len() > 8 {
            names.push_str(", …");
        }
        ui.add(egui::Label::new(egui::RichText::new(names).font(theme::regular(11.5)).color(t.text_muted)).wrap());
    }
    ui.add_space(6.0);
    ui.add(
        egui::Label::new(
            egui::RichText::new(tl!("Only send form data to sites you trust. Nothing is sent unless you choose Send."))
                .font(theme::regular(11.5))
                .color(t.text_muted),
        )
        .wrap(),
    );
    let danger = Color32::from_rgb(0xD7, 0x37, 0x3F);
    for message in [&d.problem, &d.error].into_iter().flatten() {
        ui.add_space(4.0);
        ui.add(egui::Label::new(egui::RichText::new(message.as_str()).color(danger)).wrap());
    }
    let (can_send, sending) = (d.problem.is_none(), d.sending.is_some());
    let (mut send, mut save, mut close) = (false, false, false);
    ui.add_space(12.0);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if ui.add_enabled_ui(can_send && !sending, |ui| widgets::pill_button(ui, tl!("Send"), true)).inner.clicked() {
            send = true;
        }
        if ui.add_enabled_ui(!sending, |ui| widgets::pill_button(ui, tl!("Save Data As…"), false)).inner.clicked() {
            save = true;
        }
        if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
            close = true;
        }
        if sending {
            ui.spinner();
            ui.label(tl!("Sending…"));
        }
    });
    if send {
        app.send_submission();
    }
    if save {
        app.save_submission();
        return app.submit_draft.is_none();
    }
    if close {
        app.submit_draft = None;
    }
    close || app.submit_draft.is_none()
}

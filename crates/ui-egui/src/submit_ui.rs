//! Submit Form: a button's submit-form action (or `this.submitForm`) asks first. The dialog
//! names the address and what would be sent; Send posts it, Save Data As writes it to a file
//! instead, and Cancel sends nothing. Sending runs on a background thread with a time limit and
//! a capped reply; an FDF or XFDF reply (a server filling fields back) is imported. A `mailto:`
//! submission is handed to the email app with the data saved to attach (PdfCraft sends no
//! email itself). The internet access settings decide which sites may be sent to.

use std::sync::{Arc, Mutex};

use egui::{Align, Color32, Layout};
use pdfcraft_engine::DocId;
use pdfcraft_engine::submit::{Email, Payload, Reply, SendOptions, Submission, Target};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// Where a background send leaves its result.
type SendSlot = Arc<Mutex<Option<Result<Reply, String>>>>;

/// Sends a payload and returns the server's reply ([`pdfcraft_engine::submit::send_with`] unless
/// a test sets [`PdfCraftApp::submit_sender`]).
pub type Sender = Arc<dyn Fn(&Payload) -> Result<Reply, String> + Send + Sync>;

/// The largest reply read, and the longest wait for it.
pub const SEND_OPTIONS: SendOptions = SendOptions { timeout_seconds: pdfcraft_engine::submit::TIMEOUT_SECONDS, max_response: 1 << 20 };

/// Whether a reply is form data to import: its type, or its first bytes.
pub fn reply_data_name(reply: &Reply) -> Option<&'static str> {
    let ct = reply.content_type.to_ascii_lowercase();
    let head: Vec<u8> = reply.body.iter().copied().skip_while(u8::is_ascii_whitespace).take(256).collect();
    let text = String::from_utf8_lossy(&head).to_ascii_lowercase();
    if ct.contains("vnd.fdf") || head.starts_with(b"%FDF") {
        Some("reply.fdf")
    } else if ct.contains("vnd.adobe.xfdf") || ((text.starts_with("<?xml") || text.starts_with("<xfdf")) && text.contains("<xfdf")) {
        Some("reply.xfdf")
    } else {
        None
    }
}

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
    /// A `mailto:` submission: the email to hand to the email app.
    pub email: Option<Email>,
}

impl PdfCraftApp {
    /// Ask whether to submit document `doc`'s form as `submission` (from `button`, or a script).
    pub fn start_submit(&mut self, doc: DocId, button: &str, submission: Submission) {
        let (problem, email) = match pdfcraft_engine::submit::target(&submission.url) {
            Ok(Target::Email(e)) => (None, Some(e)),
            Ok(Target::Web(_)) => {
                // Internet access settings that block the host: Save Data As still works.
                let access = self.session.check_url(&submission.url);
                let blocked = (access.decision == pdfcraft_engine::url_access::UrlDecision::Block).then(|| format!("Not sent: {}.", access.reason));
                #[cfg(target_arch = "wasm32")]
                let blocked = blocked.or_else(|| Some("Sending forms isn't available in the browser version. Save the data instead.".to_string()));
                (blocked, None)
            }
            Err(e) => (Some(e), None),
        };
        let fields =
            self.session.get(doc).map(|d| d.form.iter().filter(|f| submission.sends(f)).map(|f| f.name.clone()).collect()).unwrap_or_default();
        self.submit_draft = Some(SubmitDraft { doc, button: button.to_string(), submission, problem, fields, sending: None, error: None, email });
        self.dialog = Some(crate::Dialog::SubmitForm);
    }

    /// Send the pending submission (Send in the dialog): in the background, unless `run_inline`.
    pub fn send_submission(&mut self) {
        let Some(d) = self.submit_draft.as_ref() else { return };
        if d.sending.is_some() || d.problem.is_some() || d.email.is_some() {
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
        let sender = self.submit_sender.clone();
        let task = move || {
            let done = pdfcraft_engine::guard(|| match &sender {
                Some(send) => send(&payload),
                None => pdfcraft_engine::submit::send_with(&payload, &SEND_OPTIONS),
            })
            .and_then(|r| r);
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
                let doc = self.submit_draft.take().map(|d| d.doc);
                if self.dialog == Some(crate::Dialog::SubmitForm) {
                    self.dialog = None;
                }
                match (doc, reply_data_name(&reply)) {
                    (Some(doc), Some(name)) if !reply.truncated => {
                        let edit = pdfcraft_engine::Edit::ImportData { name: name.to_string(), bytes: Arc::new(reply.body) };
                        match self.session.apply(doc, edit) {
                            Ok(()) => {
                                if let (Some(i), Some(d)) = (self.views.iter().position(|v| v.id == doc), self.session.get(doc)) {
                                    let info = d.info.clone();
                                    if let Some(v) = self.views.get_mut(i) {
                                        v.document_changed(&info);
                                    }
                                }
                                self.notify_fmt("The form was sent to {host}, which filled in its answer.", &[("host", &host)]);
                            }
                            Err(e) => self.notify_fmt(
                                "The form was sent to {host}; its answer couldn't be imported: {e}",
                                &[("host", &host), ("e", &e.to_string())],
                            ),
                        }
                    }
                    (_, Some(_)) => self.notify_fmt("The form was sent to {host}; its answer was too large to import.", &[("host", &host)]),
                    _ => self.notify_fmt("The form was sent to {host}.", &[("host", &host)]),
                }
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

    /// A `mailto:` submission: open the email app with the address and subject, and save the
    /// data to attach (Acrobat attaches it itself; a `mailto:` link can't carry a file).
    pub fn email_submission(&mut self) {
        let Some(url) = self.submit_draft.as_ref().map(|d| d.submission.url.clone()) else { return };
        match pdfcraft_engine::links::document_url(&url) {
            Ok(url) => {
                self.open_url(&url);
                self.save_submission();
                self.notify_tr("Attach the saved form data to the email");
            }
            Err(e) => {
                if let Some(d) = self.submit_draft.as_mut() {
                    d.error = Some(e.to_string());
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
    if let Some(e) = &d.email {
        ui.label(tl!("This form wants to email its data:"));
        egui::Grid::new("submit-email").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
            for (label, list) in [(tl!("To"), &e.to), (tl!("Cc"), &e.cc), (tl!("Bcc"), &e.bcc)] {
                if !list.is_empty() {
                    ui.label(egui::RichText::new(label).color(t.text_muted));
                    ui.add(egui::Label::new(list.join(", ")).wrap());
                    ui.end_row();
                }
            }
            if let Some(subject) = &e.subject {
                ui.label(egui::RichText::new(tl!("Subject")).color(t.text_muted));
                ui.add(egui::Label::new(subject.as_str()).wrap());
                ui.end_row();
            }
        });
    } else {
        let host = pdfcraft_engine::submit::destination(url).ok().and_then(|u| u.host_str().map(str::to_string));
        match &host {
            Some(h) => ui.label(crate::i18n::fmt(tl!("This form wants to send its data to {host}:"), &[("host", h)])),
            None => ui.label(tl!("This form wants to send its data to:")),
        };
        ui.add(egui::Label::new(egui::RichText::new(url).font(theme::regular(12.0)).color(t.text_muted)).wrap());
    }
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
    if d.email.is_some() {
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                egui::RichText::new(tl!("PdfCraft doesn't send email itself: it opens your email app and saves the data for you to attach."))
                    .font(theme::regular(11.5))
                    .color(t.text_muted),
            )
            .wrap(),
        );
    }
    let (can_send, sending, email) = (d.problem.is_none(), d.sending.is_some(), d.email.is_some());
    let (mut send, mut save, mut close, mut mail) = (false, false, false, false);
    ui.add_space(12.0);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if email {
            mail = widgets::pill_button(ui, tl!("Open email app"), true).clicked();
        } else if ui.add_enabled_ui(can_send && !sending, |ui| widgets::pill_button(ui, tl!("Send"), true)).inner.clicked() {
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
    if mail {
        app.email_submission();
        return app.submit_draft.is_none();
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

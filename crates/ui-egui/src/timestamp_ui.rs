//! Explicit timestamp configuration, selected-time verification and local DSS import.
//! Background results are revision-bound and saved only after the cancellation/staleness gate.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use pdfcraft_engine::{
    DocId, SignatureInfo,
    sign::{self, VerificationTime},
    signature_workflow::{HttpTimestampAuthority, SignatureCancellation, SignatureResult, TimestampServer, verification_time_utc},
};

use crate::{Dialog, PdfCraftApp, RightPanel};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SignatureWorkflowMode {
    #[default]
    Servers,
    Timestamp,
    Evidence,
    Verify,
}

#[derive(Default)]
pub struct SignatureWorkflowState {
    pub mode: SignatureWorkflowMode,
    pub selected: Option<usize>,
    pub server_draft: Option<TimestampServer>,
    pub allow_network: bool,
    pub certificates: String,
    pub crls: String,
    pub ocsps: String,
    pub verification_time: String,
    pub results: Vec<SignatureInfo>,
    pub error: Option<String>,
}

type Outcome = Result<(SignatureResult, Vec<String>), String>;
pub struct SignatureRun {
    pub doc: DocId,
    path: PathBuf,
    pub cancellation: SignatureCancellation,
    result: Arc<Mutex<Option<Outcome>>>,
}

fn profile() -> TimestampServer {
    TimestampServer { name: "Timestamp authority".into(), url: String::new(), policy: None, timeout_seconds: 15, require_trusted_authority: true }
}

pub(crate) fn server_choice(ui: &mut egui::Ui, servers: &[TimestampServer], selected: &mut Option<usize>, optional: bool) {
    let name = selected.and_then(|i| servers.get(i)).map_or(if optional { "No timestamp" } else { "Choose a server" }, |s| s.name.as_str());
    egui::ComboBox::from_id_salt("timestamp-server-choice").selected_text(name).show_ui(ui, |ui| {
        if optional {
            ui.selectable_value(selected, None, "No timestamp");
        }
        for (index, server) in servers.iter().enumerate() {
            ui.selectable_value(selected, Some(index), &server.name);
        }
    });
}

pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp) -> bool {
    let mode = app.signature_workflow.mode;
    let title = match mode {
        SignatureWorkflowMode::Servers => "Timestamp servers",
        SignatureWorkflowMode::Timestamp => "Timestamp document",
        SignatureWorkflowMode::Evidence => "Embed validation evidence",
        SignatureWorkflowMode::Verify => "Verification time",
    };
    ui.heading(title);
    let mut execute = false;
    if mode == SignatureWorkflowMode::Servers {
        servers(ui, app);
    } else {
        let d = &mut app.signature_workflow;
        if mode != SignatureWorkflowMode::Verify {
            server_choice(ui, &app.timestamp_servers, &mut d.selected, mode == SignatureWorkflowMode::Evidence);
            ui.checkbox(&mut d.allow_network, "Allow a timestamp request to the selected server");
            ui.label("The request contains a digest, not the PDF. No request is made when this dialog opens.");
        }
        if mode == SignatureWorkflowMode::Evidence {
            ui.label("Local DER files, one path per line. Certificates are not automatically trusted.");
            for (label, value) in [("Certificates", &mut d.certificates), ("CRLs", &mut d.crls), ("OCSP responses", &mut d.ocsps)] {
                let label = ui.label(label);
                ui.add(egui::TextEdit::multiline(value).desired_rows(2).desired_width(f32::INFINITY)).labelled_by(label.id);
            }
            ui.label("Embedding evidence does not itself establish PAdES B-LT/B-LTA compliance. Incomplete coverage is reported after validation.");
        }
        if mode != SignatureWorkflowMode::Timestamp {
            if d.verification_time.is_empty() {
                d.verification_time = "best".into();
            }
            ui.label("Certificate and revocation checks at:");
            for (value, label) in
                [("best", "Trusted timestamp, else claimed signing time"), ("claimed", "Claimed signing time"), ("now", "Current time")]
            {
                ui.radio_value(&mut d.verification_time, value.to_string(), label);
            }
            let label = ui.label("Or UTC date (YYYYMMDDHHmmSSZ)");
            ui.text_edit_singleline(&mut d.verification_time).labelled_by(label.id);
            ui.label("A chosen or claimed date is not cryptographic proof of time.");
        }
        if mode == SignatureWorkflowMode::Verify {
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                for result in &d.results {
                    ui.collapsing(format!("{}: {}", result.field, result.summary()), |ui| {
                        for detail in &result.details {
                            ui.label(detail);
                        }
                    });
                }
            });
        }
        execute = ui
            .add_enabled(
                app.signature_run.is_none(),
                egui::Button::new(if mode == SignatureWorkflowMode::Verify { "Verify" } else { "Save result…" }),
            )
            .clicked();
    }
    if execute {
        let result = match mode {
            SignatureWorkflowMode::Verify => app.verify_signature_workflow(),
            _ => app.save_signature_workflow(),
        };
        if let Err(error) = result {
            app.signature_workflow.error = Some(error);
        }
    }
    if let Some(error) = &app.signature_workflow.error {
        ui.colored_label(egui::Color32::DARK_RED, error);
    }
    ui.button("Close").clicked()
}

fn servers(ui: &mut egui::Ui, app: &mut PdfCraftApp) {
    let d = &mut app.signature_workflow;
    let before = d.selected;
    server_choice(ui, &app.timestamp_servers, &mut d.selected, true);
    if before != d.selected {
        d.server_draft = d.selected.and_then(|i| app.timestamp_servers.get(i)).cloned();
    }
    if ui.button("New server").clicked() {
        d.selected = None;
        d.server_draft = Some(profile());
    }
    let Some(server) = &mut d.server_draft else {
        ui.label("Choose a server or add one. No server is enabled by default.");
        return;
    };
    for (label, value) in [("Name", &mut server.name), ("HTTPS URL", &mut server.url)] {
        let l = ui.label(label);
        ui.text_edit_singleline(value).labelled_by(l.id);
    }
    let mut policy = server.policy.clone().unwrap_or_default();
    let l = ui.label("Optional policy OID");
    if ui.text_edit_singleline(&mut policy).labelled_by(l.id).changed() {
        server.policy = (!policy.trim().is_empty()).then(|| policy.trim().to_string());
    }
    ui.horizontal(|ui| {
        ui.label("Timeout (seconds)");
        ui.add(egui::DragValue::new(&mut server.timeout_seconds).range(1..=30));
    });
    ui.checkbox(&mut server.require_trusted_authority, "Require an explicitly trusted timestamp authority");
    ui.label("Add a TSA certificate to Trusted Certificates only after independently verifying its identity and fingerprint. Disabling this check saves an untrusted token; it does not establish trusted time.");
    if ui.button("Save server").clicked() {
        match server.validate() {
            Ok(()) => {
                if let Some(entry) = d.selected.and_then(|i| app.timestamp_servers.get_mut(i)) {
                    *entry = server.clone();
                } else if app.timestamp_servers.len() < 32 {
                    app.timestamp_servers.push(server.clone());
                    d.selected = Some(app.timestamp_servers.len() - 1);
                } else {
                    d.error = Some("At most 32 timestamp servers may be configured.".into());
                    return;
                }
                d.error = None;
            }
            Err(error) => d.error = Some(error),
        }
    }
    if d.selected.is_some() && ui.button("Remove server").clicked() {
        if let Some(index) = d.selected.filter(|i| *i < app.timestamp_servers.len()) {
            app.timestamp_servers.remove(index);
        }
        d.selected = None;
        d.server_draft = None;
    }
}

impl PdfCraftApp {
    pub(crate) fn set_signature_option(&mut self, key: &str, value: &str) -> Option<Result<(), String>> {
        let result = match key {
            // Signature Properties for a signature field (by its full name), and its tab.
            "signature-properties" => self.open_signature_properties(value),
            "signature-properties-tab" => match (crate::sign_ui::SigPropsTab::parse(value), self.sig_props.as_mut()) {
                (Some(tab), Some(p)) => {
                    p.tab = tab;
                    Ok(())
                }
                (None, _) => Err("signature-properties-tab must be summary, document, signer, date-time or legal".into()),
                (_, None) => Err("Signature Properties is not open.".into()),
            },
            "timestamp-servers" => serde_json::from_str::<Vec<TimestampServer>>(value).map_err(|e| e.to_string()).and_then(|servers| {
                if servers.len() > 32 {
                    return Err("At most 32 timestamp servers may be configured.".into());
                }
                for server in &servers {
                    server.validate()?;
                }
                self.timestamp_servers = servers;
                Ok(())
            }),
            "timestamp-server" => {
                if value == "none" {
                    self.signature_workflow.selected = None;
                    Ok(())
                } else {
                    value.parse::<usize>().map_err(|_| "timestamp-server must be a zero-based index or none".to_string()).and_then(|index| {
                        if index >= self.timestamp_servers.len() {
                            return Err("Timestamp server does not exist.".into());
                        }
                        self.signature_workflow.selected = Some(index);
                        Ok(())
                    })
                }
            }
            "timestamp-network" => match value {
                "true" => {
                    self.signature_workflow.allow_network = true;
                    Ok(())
                }
                "false" => {
                    self.signature_workflow.allow_network = false;
                    Ok(())
                }
                _ => Err("timestamp-network must be true or false".into()),
            },
            "signature-certificates" => {
                self.signature_workflow.certificates = value.to_string();
                Ok(())
            }
            "signature-crls" => {
                self.signature_workflow.crls = value.to_string();
                Ok(())
            }
            "signature-ocsps" => {
                self.signature_workflow.ocsps = value.to_string();
                Ok(())
            }
            "signature-verification-time" => {
                if !matches!(value, "best" | "claimed" | "now")
                    && let Err(e) = verification_time_utc(value)
                {
                    return Some(Err(e));
                }
                self.signature_workflow.verification_time = value.to_string();
                Ok(())
            }
            _ => return None,
        };
        Some(result)
    }

    pub fn open_signature_workflow(&mut self, mode: SignatureWorkflowMode) {
        self.signature_workflow.mode = mode;
        self.signature_workflow.allow_network = false;
        self.signature_workflow.error = None;
        self.signature_workflow.results.clear();
        self.dialog = Some(Dialog::SignatureWorkflow);
    }

    fn signature_verification_time(&self) -> Result<VerificationTime, String> {
        match self.signature_workflow.verification_time.as_str() {
            "" | "best" => Ok(VerificationTime::BestAvailable),
            "claimed" => Ok(VerificationTime::ClaimedSigningTime),
            "now" => Ok(VerificationTime::At(sign::Time::from_unix(self.session.now_secs()))),
            value => verification_time_utc(value).map(VerificationTime::At),
        }
    }

    pub fn verify_signature_workflow(&mut self) -> Result<(), String> {
        let (_, doc) = self.active_ids().ok_or("No document is open.")?;
        self.signature_workflow.results = self.session.verify_signatures_at(doc, self.signature_verification_time()?).map_err(|e| e.to_string())?;
        self.signature_workflow.error = None;
        Ok(())
    }

    pub(crate) fn signature_authority(
        &self,
        selected: Option<usize>,
        consent: bool,
        cancel: SignatureCancellation,
    ) -> Result<Option<HttpTimestampAuthority>, String> {
        let Some(index) = selected else { return Ok(None) };
        if !consent {
            return Err("Allow the timestamp request before saving.".into());
        }
        let server = self.timestamp_servers.get(index).cloned().ok_or("Choose a configured timestamp server.")?;
        HttpTimestampAuthority::new(server, self.session.trusted_certificates().to_vec(), cancel).map(Some)
    }

    pub fn save_signature_workflow(&mut self) -> Result<(), String> {
        if self.signature_run.is_some() {
            return Err("A signature operation is already running.".into());
        }
        if !self.commit_form_typing() {
            return Err("The form value must be accepted before signing.".into());
        }
        let (_, doc) = self.active_ids().ok_or("No document is open.")?;
        let cancel = SignatureCancellation::default();
        let authority = self.signature_authority(self.signature_workflow.selected, self.signature_workflow.allow_network, cancel.clone())?;
        let mode = self.signature_workflow.mode;
        if mode == SignatureWorkflowMode::Timestamp && authority.is_none() {
            return Err("Choose a configured timestamp server.".into());
        }
        let evidence = if mode == SignatureWorkflowMode::Evidence { Some(self.read_signature_evidence()?) } else { None };
        let when = self.signature_verification_time()?;
        let go = move |app: &mut Self, path: PathBuf| -> Result<(), String> {
            if !app.commit_typing_in(doc) {
                return Err("The form value must be accepted before signing.".into());
            }
            let job = app.session.signature_job(doc).map_err(|e| e.to_string())?;
            app.spawn_signature_job(doc, path, cancel, move || {
                let authority = authority.as_ref().map(|a| a as &dyn sign::TimestampAuthority);
                match evidence {
                    Some(evidence) => job.embed_evidence(&evidence, when, authority).map(|(r, report)| (r, report.notes)).map_err(|e| e.to_string()),
                    None => job.timestamp(authority.ok_or("Choose a timestamp server.")?).map(|r| (r, Vec::new())).map_err(|e| e.to_string()),
                }
            })
        };
        if let Some(path) = self.save_override.clone() {
            return go(self, path.into());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let dialog = rfd::AsyncFileDialog::new().add_filter("PDF", &["pdf"]).set_file_name("timestamped.pdf");
            if self.ask_one(crate::pickers::Ask::Save(dialog), Some(doc), move |app, path| {
                if let Err(e) = go(app, path) {
                    app.notify_error(e);
                }
            }) {
                Ok(())
            } else {
                Err("A file picker is already open.".into())
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = go;
            Err("Saving this workflow is available in native PdfCraft.".into())
        }
    }

    fn read_signature_evidence(&self) -> Result<sign::dss::Evidence, String> {
        use std::io::Read;
        let mut evidence = sign::dss::Evidence::default();
        let mut count = 0usize;
        let mut size = 0usize;
        for (paths, target) in [
            (&self.signature_workflow.certificates, &mut evidence.certs),
            (&self.signature_workflow.crls, &mut evidence.crls),
            (&self.signature_workflow.ocsps, &mut evidence.ocsps),
        ] {
            for path in paths.lines().map(str::trim).filter(|s| !s.is_empty()) {
                count += 1;
                if count > 64 {
                    return Err("At most 64 evidence files may be supplied.".into());
                }
                let mut bytes = Vec::new();
                std::fs::File::open(path)
                    .map_err(|e| format!("{path}: {e}"))?
                    .take(2 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
                size += bytes.len();
                if bytes.len() > 2 * 1024 * 1024 || size > 32 * 1024 * 1024 {
                    return Err("Evidence is limited to 2 MiB per file and 32 MiB in total.".into());
                }
                target.push(bytes);
            }
        }
        Ok(evidence)
    }

    pub(crate) fn spawn_signature_job(
        &mut self,
        doc: DocId,
        path: PathBuf,
        cancellation: SignatureCancellation,
        work: impl FnOnce() -> Outcome + Send + 'static,
    ) -> Result<(), String> {
        if self.signature_run.is_some() {
            return Err("A signature operation is already running.".into());
        }
        let result = Arc::new(Mutex::new(None));
        let slot = result.clone();
        let ctx = self.ctx.clone();
        let task = move || {
            let done = pdfcraft_engine::guard(work).and_then(|result| result);
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(done);
            }
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        if self.run_inline {
            task();
        } else {
            std::thread::Builder::new().name("pdfcraft-signature".into()).spawn(task).map_err(|e| e.to_string())?;
        }
        #[cfg(target_arch = "wasm32")]
        task();
        self.signature_run = Some(SignatureRun { doc, path, cancellation, result });
        self.signature_workflow.error = None;
        Ok(())
    }

    pub fn cancel_signature_workflow(&mut self) {
        if let Some(run) = &self.signature_run {
            run.cancellation.cancel();
        }
    }

    pub(crate) fn poll_signature_workflow(&mut self) {
        let Some(run) = &self.signature_run else { return };
        let done = match run.result.lock() {
            Ok(mut result) => result.take(),
            Err(_) => Some(Err("Signature worker state is unavailable.".into())),
        };
        let Some(done) = done else {
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            return;
        };
        let Some(run) = self.signature_run.take() else { return };
        if run.cancellation.is_cancelled() {
            self.notify("Signature operation cancelled. No result was saved.");
            return;
        }
        let apply = || -> Result<Vec<String>, String> {
            let (result, notes) = done?;
            self.session.check_signature_result(&result).map_err(|e| e.to_string())?;
            crate::editing::write_atomically(&run.path.to_string_lossy(), &result.bytes).map_err(|e| format!("Could not save: {e}"))?;
            self.session.mark_signed(result.doc, result.bytes, Some(run.path.to_string_lossy().into_owned())).map_err(|e| e.to_string())?;
            if let Some(view) = self.views.iter_mut().find(|v| v.id == result.doc) {
                view.invalidate_content();
            }
            Ok(notes)
        };
        match apply() {
            Ok(notes) => {
                self.right = Some(RightPanel::Signatures);
                self.notify(format!("Saved to {}. {}", run.path.display(), notes.join(" ")));
            }
            Err(error) => {
                self.signature_workflow.error = Some(error.clone());
                self.notify_error(error);
            }
        }
    }

    pub(crate) fn signature_progress(&mut self, ctx: &egui::Context) {
        if self.signature_run.is_none() {
            return;
        }
        egui::Window::new("Signature operation").collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label("Validating and saving…");
            if ui.button("Cancel operation").clicked() {
                self.cancel_signature_workflow();
            }
            ui.label("A cancelled response is discarded. A blocked request stops at the configured deadline (at most 30 seconds).");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Host;

    #[test]
    fn signature_state_is_inspectable_without_sending_a_request() {
        let mut app = PdfCraftApp::new();
        app.open_signature_workflow(SignatureWorkflowMode::Timestamp);
        app.set_option(
            "timestamp-servers",
            r#"[{"name":"Synthetic","url":"http://127.0.0.1:1/unused","timeout_seconds":1,"require_trusted_authority":true}]"#,
        )
        .unwrap();
        app.set_option("timestamp-server", "0").unwrap();
        app.set_option("timestamp-network", "true").unwrap();
        let state = app.state();
        assert_eq!(state["timestamp_servers"][0]["name"], "Synthetic");
        assert_eq!(state["signature_workflow"]["selected"], 0);
        assert_eq!(state["signature_workflow"]["allow_network"], true);
        assert!(state["signature_workflow"]["running"].is_null());
        assert!(app.signature_run.is_none());
    }
}

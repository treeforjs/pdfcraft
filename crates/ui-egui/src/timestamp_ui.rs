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
    Timestamp,
    Evidence,
    Verify,
    /// Add verification information (LTV): chains and revocation evidence, fetched online
    /// where allowed.
    Ltv,
}

#[derive(Default)]
pub struct SignatureWorkflowState {
    pub mode: SignatureWorkflowMode,
    pub selected: Option<usize>,
    /// Preferences → Signatures → Timestamp servers: the server being edited (its index in
    /// the list, or `None` for a new one) and the draft.
    pub editing: Option<usize>,
    pub server_draft: Option<TimestampServer>,
    /// A password typed for the selected server (held for the session once used).
    pub password: String,
    pub allow_network: bool,
    /// Add verification information: fetch revocation evidence online, and also from sites
    /// Internet access would ask about.
    pub fetch_online: bool,
    pub allow_asked_sites: bool,
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
    TimestampServer {
        name: tl!("Timestamp authority").into(),
        url: String::new(),
        policy: None,
        timeout_seconds: 15,
        require_trusted_authority: true,
        username: None,
    }
}

pub(crate) fn server_choice(ui: &mut egui::Ui, servers: &[TimestampServer], selected: &mut Option<usize>, optional: bool) {
    let name = selected.and_then(|i| servers.get(i)).map_or(if optional { tl!("No timestamp") } else { tl!("Choose a server") }, |s| s.name.as_str());
    egui::ComboBox::from_id_salt("timestamp-server-choice").selected_text(name).show_ui(ui, |ui| {
        if optional {
            ui.selectable_value(selected, None, tl!("No timestamp"));
        }
        for (index, server) in servers.iter().enumerate() {
            ui.selectable_value(selected, Some(index), &server.name);
        }
    });
}

pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp) -> bool {
    let mode = app.signature_workflow.mode;
    let title = match mode {
        SignatureWorkflowMode::Timestamp => tl!("Timestamp document"),
        SignatureWorkflowMode::Evidence => tl!("Embed validation evidence"),
        SignatureWorkflowMode::Verify => tl!("Verification time"),
        SignatureWorkflowMode::Ltv => tl!("Add verification information"),
    };
    ui.heading(title);
    let servers = app.session.timestamp_servers().to_vec();
    let d = &mut app.signature_workflow;
    if mode == SignatureWorkflowMode::Ltv {
        ui.label(tl!(
            "Adds the signers' certificate chains and their revocation status to the document, so the signatures can still be checked after the certificates expire."
        ));
        ui.checkbox(&mut d.fetch_online, tl!("Fetch revocation information online"));
        if d.fetch_online {
            ui.checkbox(&mut d.allow_asked_sites, tl!("Also from sites that Internet access asks about"));
            ui.label(
                egui::RichText::new(tl!("Blocked sites stay blocked. Only the certificates' OCSP responders and CRL addresses are contacted."))
                    .small(),
            );
        }
    }
    if mode != SignatureWorkflowMode::Verify {
        let optional = mode != SignatureWorkflowMode::Timestamp;
        if mode == SignatureWorkflowMode::Ltv {
            ui.label(tl!("Then add a document timestamp (optional):"));
        }
        server_choice(ui, &servers, &mut d.selected, optional);
        if d.selected.is_some() {
            ui.checkbox(&mut d.allow_network, tl!("Allow a timestamp request to the selected server"));
            ui.label(egui::RichText::new(tl!("The request contains a digest, not the PDF. No request is made when this dialog opens.")).small());
        }
        if let Some(server) = d.selected.and_then(|i| servers.get(i)).filter(|s| s.username.is_some())
            && app.session.timestamp_password(&server.name).is_none()
        {
            let l = ui.label(tl!("Password"));
            ui.add(egui::TextEdit::singleline(&mut d.password).password(true)).labelled_by(l.id);
        }
    }
    if matches!(mode, SignatureWorkflowMode::Evidence | SignatureWorkflowMode::Ltv) {
        ui.label(if mode == SignatureWorkflowMode::Ltv {
            tl!("Local DER files to add too (optional), one path per line. Certificates are not automatically trusted.")
        } else {
            tl!("Local DER files, one path per line. Certificates are not automatically trusted.")
        });
        for (label, value) in [(tl!("Certificates"), &mut d.certificates), (tl!("CRLs"), &mut d.crls), (tl!("OCSP responses"), &mut d.ocsps)] {
            let label = ui.label(label);
            ui.add(egui::TextEdit::multiline(value).desired_rows(2).desired_width(f32::INFINITY)).labelled_by(label.id);
        }
        ui.label(
            egui::RichText::new(tl!(
                "Embedding evidence does not itself establish PAdES B-LT/B-LTA compliance. Incomplete coverage is reported after validation."
            ))
            .small(),
        );
    }
    if matches!(mode, SignatureWorkflowMode::Evidence | SignatureWorkflowMode::Verify) {
        if d.verification_time.is_empty() {
            d.verification_time = "best".into();
        }
        ui.label(tl!("Certificate and revocation checks at:"));
        for (value, label) in
            [("best", tl!("Trusted timestamp, else claimed signing time")), ("claimed", tl!("Claimed signing time")), ("now", tl!("Current time"))]
        {
            ui.radio_value(&mut d.verification_time, value.to_string(), label);
        }
        let label = ui.label(tl!("Or UTC date (YYYYMMDDHHmmSSZ)"));
        ui.text_edit_singleline(&mut d.verification_time).labelled_by(label.id);
        ui.label(egui::RichText::new(tl!("A chosen or claimed date is not cryptographic proof of time.")).small());
    }
    if mode == SignatureWorkflowMode::Verify {
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            for result in &d.results {
                ui.collapsing(format!("{}: {}", result.field, tl!(result.summary())), |ui| {
                    for detail in &result.details {
                        ui.label(detail);
                    }
                });
            }
        });
    }
    let execute = ui
        .add_enabled(
            app.signature_run.is_none(),
            egui::Button::new(if mode == SignatureWorkflowMode::Verify { tl!("Verify") } else { tl!("Save result…") }),
        )
        .clicked();
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
        ui.colored_label(egui::Color32::from_rgb(0xD7, 0x37, 0x3F), error);
    }
    ui.button(tl!("Close")).clicked()
}

/// Preferences → Signatures → Timestamp servers: the list, and the server being edited.
pub(crate) fn servers(ui: &mut egui::Ui, app: &mut PdfCraftApp) {
    let list = app.session.timestamp_servers().to_vec();
    if list.is_empty() {
        ui.label(egui::RichText::new(tl!("No server is configured. PdfCraft never contacts one unless you add it here.")).small());
    }
    let mut edit = None;
    let mut remove = None;
    for (i, server) in list.iter().enumerate() {
        ui.push_id(("timestamp-server", i), |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&server.name).strong());
                ui.label(egui::RichText::new(&server.url).small());
                if ui.button(tl!("Edit")).clicked() {
                    edit = Some(i);
                }
                if ui.button(tl!("Remove")).clicked() {
                    remove = Some(i);
                }
            });
        });
    }
    let d = &mut app.signature_workflow;
    if let Some(i) = edit {
        d.editing = Some(i);
        d.server_draft = list.get(i).cloned();
        d.password.clear();
    }
    if let Some(i) = remove {
        let mut next = list.clone();
        if i < next.len() {
            next.remove(i);
        }
        if let Err(e) = app.session.set_timestamp_servers(next) {
            app.signature_workflow.error = Some(e);
        }
        app.signature_workflow.server_draft = None;
        app.signature_workflow.editing = None;
        app.signature_workflow.selected = None;
        return;
    }
    if d.server_draft.is_none() && ui.button(tl!("New server")).clicked() {
        d.editing = None;
        d.server_draft = Some(profile());
        d.password.clear();
    }
    let Some(server) = &mut d.server_draft else { return };
    for (label, value) in [(tl!("Name"), &mut server.name), (tl!("HTTPS URL"), &mut server.url)] {
        let l = ui.label(label);
        ui.text_edit_singleline(value).labelled_by(l.id);
    }
    let mut policy = server.policy.clone().unwrap_or_default();
    let l = ui.label(tl!("Optional policy OID"));
    if ui.text_edit_singleline(&mut policy).labelled_by(l.id).changed() {
        server.policy = (!policy.trim().is_empty()).then(|| policy.trim().to_string());
    }
    let mut user = server.username.clone().unwrap_or_default();
    let l = ui.label(tl!("Account name (optional)"));
    if ui.text_edit_singleline(&mut user).labelled_by(l.id).changed() {
        server.username = (!user.trim().is_empty()).then(|| user.trim().to_string());
    }
    if server.username.is_some() {
        let l = ui.label(tl!("Password"));
        ui.add(egui::TextEdit::singleline(&mut d.password).password(true)).labelled_by(l.id);
        ui.label(egui::RichText::new(tl!("The password is kept only until PdfCraft quits, never saved.")).small());
    }
    ui.horizontal(|ui| {
        ui.label(tl!("Timeout (seconds)"));
        ui.add(egui::DragValue::new(&mut server.timeout_seconds).range(1..=30));
    });
    ui.checkbox(&mut server.require_trusted_authority, tl!("Require an explicitly trusted timestamp authority"));
    ui.label(egui::RichText::new(tl!("Add a TSA certificate to Trusted Certificates only after independently verifying its identity and fingerprint. Disabling this check saves an untrusted token; it does not establish trusted time.")).small());
    let mut save = false;
    let mut cancel = false;
    ui.horizontal(|ui| {
        save = ui.button(tl!("Save server")).clicked();
        cancel = ui.button(tl!("Cancel")).clicked();
    });
    if cancel {
        d.server_draft = None;
        d.editing = None;
        d.password.clear();
        return;
    }
    if save {
        let server = server.clone();
        let mut next = list;
        match d.editing.filter(|i| *i < next.len()) {
            Some(i) => next[i] = server.clone(),
            None => next.push(server.clone()),
        }
        let password = std::mem::take(&mut d.password);
        match app.session.set_timestamp_servers(next) {
            Ok(()) => {
                if server.username.is_some() && !password.is_empty() {
                    let _ = app.session.set_timestamp_password(&server.name, Some(password));
                }
                app.signature_workflow.server_draft = None;
                app.signature_workflow.editing = None;
                app.signature_workflow.error = None;
            }
            Err(e) => app.signature_workflow.error = Some(e),
        }
    }
}

/// The notice after adding verification information: how much was covered, and why not all.
fn ltv_notes(report: &pdfcraft_engine::signature_workflow::LtvReport) -> Vec<String> {
    let f = &report.fetched;
    let mut notes = vec![crate::i18n::fmt(
        tl!("Revocation information for {covered} of {needed} certificates was fetched."),
        &[("covered", &f.covered.to_string()), ("needed", &f.needed.to_string())],
    )];
    let refused = f.attempts.iter().filter(|a| matches!(a.outcome, "blocked" | "asks")).count();
    if refused > 0 {
        notes.push(crate::i18n::fmt(tl!("{n} addresses were not contacted because of Internet access settings."), &[("n", &refused.to_string())]));
    }
    if report.evidence.has_complete_coverage() {
        notes.push(tl!("Every certificate's revocation status is now in the document.").to_string());
    } else {
        notes.extend(report.evidence.notes.iter().cloned());
    }
    notes
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
            "timestamp-servers" => serde_json::from_str::<Vec<TimestampServer>>(value)
                .map_err(|e| e.to_string())
                .and_then(|servers| self.session.set_timestamp_servers(servers)),
            "timestamp-server" => {
                if value == "none" {
                    self.signature_workflow.selected = None;
                    Ok(())
                } else {
                    value.parse::<usize>().map_err(|_| "timestamp-server must be a zero-based index or none".to_string()).and_then(|index| {
                        if index >= self.session.timestamp_servers().len() {
                            return Err("Timestamp server does not exist.".into());
                        }
                        self.signature_workflow.selected = Some(index);
                        Ok(())
                    })
                }
            }
            "signature-fetch-online" | "signature-asked-sites" => match value {
                "true" | "false" => {
                    let on = value == "true";
                    if key == "signature-fetch-online" {
                        self.signature_workflow.fetch_online = on;
                    } else {
                        self.signature_workflow.allow_asked_sites = on;
                    }
                    Ok(())
                }
                _ => Err(format!("{key} must be true or false")),
            },
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
            // The Sign As dialog: lock after signing, the graphic, a saved appearance.
            "sign-lock-document" | "sign-graphic" | "sign-appearance" => {
                let Some(d) = self.sign_draft.as_mut() else { return Some(Err("No signing dialog is open.".into())) };
                match key {
                    "sign-lock-document" => match value {
                        "true" | "false" => {
                            d.lock_document = value == "true";
                            Ok(())
                        }
                        _ => Err("sign-lock-document must be true or false".into()),
                    },
                    "sign-graphic" => {
                        use crate::sign_ui::GraphicChoice;
                        match value {
                            "name" => d.graphic = GraphicChoice::Name,
                            "drawn" => d.graphic = GraphicChoice::Drawn,
                            "picture" => d.graphic = GraphicChoice::Picture,
                            _ => return Some(Err("sign-graphic must be name, drawn or picture".into())),
                        }
                        Ok(())
                    }
                    _ => match self.session.signature_appearance(value).cloned() {
                        Some(a) => {
                            d.apply(&a);
                            Ok(())
                        }
                        None => Err(format!("No saved appearance is named {value:?}.")),
                    },
                }
            }
            _ => return None,
        };
        Some(result)
    }

    pub fn open_signature_workflow(&mut self, mode: SignatureWorkflowMode) {
        self.signature_workflow.mode = mode;
        // Every request needs consent again, each time the dialog opens.
        self.signature_workflow.allow_network = false;
        self.signature_workflow.fetch_online = false;
        self.signature_workflow.allow_asked_sites = false;
        self.signature_workflow.password.clear();
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
        let server = self.session.timestamp_servers().get(index).cloned().ok_or("Choose a configured timestamp server.")?;
        let password = self.session.timestamp_password(&server.name).map(str::to_string);
        HttpTimestampAuthority::with_trust(server, self.session.trust_store().clone(), password, cancel).map(Some)
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
        // A password typed in the dialog is held for the session from now on.
        if let Some(server) = self.signature_workflow.selected.and_then(|i| self.session.timestamp_servers().get(i)).cloned()
            && server.username.is_some()
            && !self.signature_workflow.password.is_empty()
        {
            let password = std::mem::take(&mut self.signature_workflow.password);
            self.session.set_timestamp_password(&server.name, Some(password))?;
        }
        let authority = self.signature_authority(self.signature_workflow.selected, self.signature_workflow.allow_network, cancel.clone())?;
        let mode = self.signature_workflow.mode;
        if mode == SignatureWorkflowMode::Timestamp && authority.is_none() {
            return Err(tl!("Choose a configured timestamp server.").into());
        }
        let evidence = match mode {
            SignatureWorkflowMode::Evidence => Some(self.read_signature_evidence()?),
            SignatureWorkflowMode::Ltv => Some(self.read_signature_evidence_optional()?),
            _ => None,
        };
        let ltv = (mode == SignatureWorkflowMode::Ltv).then(|| {
            let d = &self.signature_workflow;
            self.session.revocation_fetch_policy(d.fetch_online, d.allow_asked_sites)
        });
        let transport = pdfcraft_engine::revocation_fetch::HttpTransport { cancellation: cancel.clone() };
        let when = self.signature_verification_time()?;
        let go = move |app: &mut Self, path: PathBuf| -> Result<(), String> {
            if !app.commit_typing_in(doc) {
                return Err("The form value must be accepted before signing.".into());
            }
            let job = app.session.signature_job(doc).map_err(|e| e.to_string())?;
            app.spawn_signature_job(doc, path, cancel, move || {
                let authority = authority.as_ref().map(|a| a as &dyn sign::TimestampAuthority);
                match (evidence, ltv) {
                    (Some(local), Some(policy)) => job
                        .add_verification_information(&local, &policy, &transport, authority)
                        .map(|(r, report)| (r, ltv_notes(&report)))
                        .map_err(|e| e.to_string()),
                    (Some(evidence), None) => {
                        job.embed_evidence(&evidence, when, authority).map(|(r, report)| (r, report.notes)).map_err(|e| e.to_string())
                    }
                    _ => job.timestamp(authority.ok_or("Choose a timestamp server.")?).map(|r| (r, Vec::new())).map_err(|e| e.to_string()),
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

    /// Add verification information's local files: none is fine.
    fn read_signature_evidence_optional(&self) -> Result<sign::dss::Evidence, String> {
        let d = &self.signature_workflow;
        if [&d.certificates, &d.crls, &d.ocsps].iter().all(|s| s.trim().is_empty()) {
            return Ok(sign::dss::Evidence::default());
        }
        self.read_signature_evidence()
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
        egui::Window::new(tl!("Signature operation")).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label(tl!("Validating and saving…"));
            if ui.button(tl!("Cancel operation")).clicked() {
                self.cancel_signature_workflow();
            }
            ui.label(tl!("A cancelled response is discarded. A blocked request stops at the configured deadline (at most 30 seconds)."));
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

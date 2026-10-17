//! Submitting a form (a Submit Form button or `this.submitForm`): the data a submit-form
//! action asks for, and sending it. Nothing is sent unless the front end calls [`send`],
//! which it does only after the user confirms the destination.

pub use pdfcraft_forms::submit::{Submission, SubmitFormat};

use crate::{DocId, EditError, Session, guard};

/// What a submission sends, and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payload {
    /// The address requested: the action's URL, with the data appended as a query for GET.
    pub url: String,
    pub get: bool,
    pub format: SubmitFormat,
    pub content_type: &'static str,
    /// The request body (empty for GET).
    pub body: Vec<u8>,
}

/// The server's answer to a sent form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub content_type: String,
}

/// At most this long a submission waits for the server.
pub const TIMEOUT_SECONDS: u64 = 30;

/// The address a submission may be sent to: an absolute `http:` or `https:` URL without
/// credentials. Anything else (`mailto:`, a relative path, `file:`) is explained instead.
pub fn destination(url: &str) -> Result<url::Url, String> {
    let parsed = url::Url::parse(url.trim()).map_err(|_| {
        if url.trim().is_empty() {
            "The form doesn't say where to send its data.".to_string()
        } else {
            format!("{url} isn't a web address PdfCraft can send to.")
        }
    })?;
    match parsed.scheme() {
        "http" | "https" => {}
        "mailto" => {
            return Err(format!("The form asks to email its data to {}; PdfCraft doesn't send email. Save the data and attach it.", parsed.path()));
        }
        _ => return Err(format!("PdfCraft sends form data only to web (http or https) addresses, not to {url}.")),
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("The form's address contains a user name or password; PdfCraft doesn't send to it.".into());
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(format!("{url} has no host."));
    }
    Ok(parsed)
}

impl Session {
    /// The data `submission` sends from document `id`, in its format and for its fields.
    /// The PDF format sends the document as Save would write it.
    pub fn submission(&self, id: DocId, submission: &Submission) -> Result<Payload, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let format = submission.format;
        let body = if format == SubmitFormat::Pdf {
            self.save_bytes(id)?.to_vec()
        } else {
            let cos = match doc.editor.as_ref() {
                Some(e) => e.cos.clone(),
                None => pdfcraft_cos::Document::open_with_password(doc.bytes.clone(), doc.password.as_deref())
                    .map_err(|e| EditError::Write(e.to_string()))?,
            };
            let keep = |f: &pdfcraft_forms::Field| submission.sends(f);
            guard(|| match format {
                SubmitFormat::Xfdf => pdfcraft_xfdf::export_xfdf_with(&cos, false, true, &doc.name, &keep).into_bytes(),
                SubmitFormat::Html => pdfcraft_xfdf::export_html_form(&cos, &keep).into_bytes(),
                _ => pdfcraft_xfdf::export_fdf_with(&cos, false, true, &doc.name, &keep),
            })
            .map_err(EditError::Write)?
        };
        let mut url = submission.url.trim().to_string();
        let get = submission.get && format == SubmitFormat::Html;
        if get {
            let query = String::from_utf8_lossy(&body).into_owned();
            if !query.is_empty() {
                url.push(if url.contains('?') { '&' } else { '?' });
                url.push_str(&query);
            }
        }
        Ok(Payload { url, get, format, content_type: format.content_type(), body: if get { Vec::new() } else { body } })
    }
}

/// Send `payload` to its address and return the server's status. Redirects are not followed
/// (the user confirmed one address); HTTPS uses the system's trusted roots.
#[cfg(not(target_arch = "wasm32"))]
pub fn send(payload: &Payload) -> Result<Reply, String> {
    use std::time::Duration;
    let target = destination(&payload.url)?;
    let roots = rustls_native_certs::load_native_certs();
    let roots: Vec<_> = roots.certs.iter().map(|cert| ureq::tls::Certificate::from_der(cert.as_ref()).to_owned()).collect();
    if roots.is_empty() && target.scheme() == "https" {
        return Err("No system TLS trust roots are available.".into());
    }
    let mut config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(TIMEOUT_SECONDS)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .max_redirects(0)
        .http_status_as_error(false)
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::new_with_certs(&roots)).build());
    // A literal loopback address is local by construction: not through an environment proxy.
    if matches!(target.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
        || matches!(target.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
    {
        config = config.proxy(None);
    }
    let agent = config.build().new_agent();
    let agent_name = concat!("PdfCraft/", env!("CARGO_PKG_VERSION"));
    let response = if payload.get {
        agent.get(target.as_str()).header("User-Agent", agent_name).call()
    } else {
        agent.post(target.as_str()).header("Content-Type", payload.content_type).header("User-Agent", agent_name).send(payload.body.as_slice())
    }
    .map_err(|e| format!("The form couldn't be sent: {e}"))?;
    let content_type = response.headers().get("Content-Type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    Ok(Reply { status: response.status().as_u16(), content_type })
}

/// The browser build doesn't send forms.
#[cfg(target_arch = "wasm32")]
pub fn send(_: &Payload) -> Result<Reply, String> {
    Err("Sending forms isn't available in the browser version. Save the data instead.".into())
}

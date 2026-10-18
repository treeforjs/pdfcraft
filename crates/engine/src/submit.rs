//! Submitting a form (a Submit Form button or `this.submitForm`): the data a submit-form
//! action asks for, and sending it. Nothing is sent unless the front end calls [`send`],
//! which it does only after the user confirms the destination.

pub use pdfcraft_forms::submit::{Submission, SubmitFormat, flag};

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
    /// A file name for the data (saving it, or attaching it to an email): the document's
    /// name with the format's extension.
    pub filename: String,
    /// The fields sent, by full name.
    pub fields: Vec<String>,
    /// How many markup annotations were sent (IncludeAnnotations).
    pub annotations: usize,
    /// What the action asked for that couldn't be done as asked, in words.
    pub notes: Vec<String>,
}

/// What the viewer knows when a submission starts that the action doesn't say.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    /// The push button whose action this is (SubmitCoordinates names the click after it).
    pub button: Option<String>,
    /// Where the button was clicked, in points from its widget's top-left corner.
    pub click: Option<[f64; 2]>,
    /// The current user's name, matched against annotation authors (`/T`) for
    /// ExclNonUserAnnots.
    pub user: Option<String>,
}

/// The server's answer to a sent form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub content_type: String,
    /// The response body, at most [`SendOptions::max_response`] bytes of it.
    pub body: Vec<u8>,
    /// The server sent more than `max_response` bytes; `body` holds the first ones.
    pub truncated: bool,
    /// A redirect's `Location` (redirects are reported, never followed).
    pub location: Option<String>,
}

/// How a submission is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SendOptions {
    /// The whole exchange's time limit, in seconds (at most [`MAX_TIMEOUT_SECONDS`]).
    pub timeout_seconds: u64,
    /// At most this many bytes of the response are read (at most [`MAX_RESPONSE`]).
    pub max_response: usize,
}

impl Default for SendOptions {
    fn default() -> Self {
        Self { timeout_seconds: TIMEOUT_SECONDS, max_response: DEFAULT_RESPONSE }
    }
}

/// At most this long a submission waits for the server, by default.
pub const TIMEOUT_SECONDS: u64 = 30;
/// The longest time limit a caller may ask for.
pub const MAX_TIMEOUT_SECONDS: u64 = 300;
/// The response bytes read by default.
pub const DEFAULT_RESPONSE: usize = 1 << 20;
/// The most response bytes a caller may ask to read.
pub const MAX_RESPONSE: usize = 16 << 20;

/// An email a `mailto:` submission asks for. PdfCraft doesn't send email: the front end hands
/// this, with the payload as the attachment, to the user's mail program or to the agent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Email {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: Option<String>,
    pub body: Option<String>,
}

/// Where a submission goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// An `http:` or `https:` address (see [`destination`]).
    Web(url::Url),
    /// A `mailto:` address (RFC 6068).
    Email(Email),
}

/// At most this many addresses are read from a `mailto:` URL.
const MAX_ADDRESSES: usize = 100;
/// At most this many bytes of a `mailto:` URL are read.
const MAX_MAILTO: usize = 64 << 10;

/// Percent-decode `s` (RFC 3986; `+` stays a plus, as in `mailto:`), as UTF-8 with
/// replacement characters for invalid sequences.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        let hex = |j: usize| b.get(j).and_then(|h| char::from(*h).to_digit(16));
        if c == b'%'
            && let (Some(h), Some(l)) = (hex(i + 1), hex(i + 2))
        {
            out.push(((h << 4) | l) as u8);
            i += 3;
        } else {
            out.push(c);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Read a `mailto:` URL: its addresses and the `to`, `cc`, `bcc`, `subject` and `body` fields.
pub fn parse_mailto(url: &str) -> Result<Email, String> {
    let url = url.trim();
    if url.len() > MAX_MAILTO {
        return Err("The form's email address is too long.".into());
    }
    let rest = url
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("mailto:"))
        .and_then(|_| url.get(7..))
        .ok_or_else(|| format!("{url} isn't a mailto: address."))?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut email = Email::default();
    let push = |list: &mut Vec<String>, value: &str| {
        for a in value.split(',').map(str::trim).filter(|a| !a.is_empty()) {
            if list.len() < MAX_ADDRESSES {
                list.push(a.to_string());
            }
        }
    };
    push(&mut email.to, &percent_decode(path));
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = percent_decode(v);
        match percent_decode(k).to_ascii_lowercase().as_str() {
            "to" => push(&mut email.to, &v),
            "cc" => push(&mut email.cc, &v),
            "bcc" => push(&mut email.bcc, &v),
            "subject" => email.subject = Some(v),
            "body" => email.body = Some(v),
            _ => {}
        }
    }
    if email.to.is_empty() && email.cc.is_empty() && email.bcc.is_empty() {
        return Err(format!("{url} names no one to email the form to."));
    }
    Ok(email)
}

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

/// Where `url` sends a submission: a web address ([`destination`]) or an email (`mailto:`).
pub fn target(url: &str) -> Result<Target, String> {
    if url.trim().get(..7).is_some_and(|p| p.eq_ignore_ascii_case("mailto:")) {
        return parse_mailto(url).map(Target::Email);
    }
    destination(url).map(Target::Web)
}

impl Session {
    /// The data `submission` sends from document `id`, in its format and for its fields.
    /// The PDF format sends the document as Save would write it.
    pub fn submission(&self, id: DocId, submission: &Submission) -> Result<Payload, EditError> {
        self.submission_in(id, submission, &Context::default())
    }

    /// [`Session::submission`] with what the viewer knows (the button, the click, the user).
    /// Pure: nothing is sent or changed.
    pub fn submission_in(&self, id: DocId, submission: &Submission, cx: &Context) -> Result<Payload, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let format = submission.format;
        let mut notes = Vec::new();
        let fields: Vec<String> = doc.form.iter().filter(|f| submission.sends(f)).map(|f| f.name.clone()).collect();
        let mut annotations = 0;
        let body = if format == SubmitFormat::Pdf {
            self.save_bytes(id)?.to_vec()
        } else {
            let cos = match doc.editor.as_ref() {
                Some(e) => e.cos.clone(),
                None => pdfcraft_cos::Document::open_with_password(doc.bytes.clone(), doc.password.as_deref())
                    .map_err(|e| EditError::Write(e.to_string()))?,
            };
            // The bytes Save would add to the file as opened (IncludeAppendSaves).
            let differences = if submission.append_saves {
                let saved = self.save_bytes(id)?;
                let base = doc.editor.as_ref().map_or_else(|| doc.bytes.clone(), |e| e.cos.bytes().clone());
                match saved.get(base.len()..).filter(|tail| !tail.is_empty() && saved.starts_with(&base)) {
                    Some(tail) => Some(tail.to_vec()),
                    None => {
                        notes.push("IncludeAppendSaves: the document has no changes since it was opened, so no /Differences were sent.".into());
                        None
                    }
                }
            } else {
                None
            };
            let embedded = if submission.embed_form { Some(self.save_bytes(id)?.to_vec()) } else { None };
            let user = cx.user.as_deref().map(str::trim).filter(|u| !u.is_empty());
            if submission.user_annotations_only && user.is_none() {
                notes.push("ExclNonUserAnnots: no user name was given, so no annotations were sent.".into());
            }
            let by_user = |d: &pdfcraft_cos::Dict| {
                if !submission.user_annotations_only {
                    return true;
                }
                let author = d.get(b"T").and_then(|t| cos.resolve(t).as_string().map(|s| s.to_text()));
                user.is_some_and(|u| author.as_deref().map(str::trim) == Some(u))
            };
            let counted = std::cell::Cell::new(0usize);
            let comment = |d: &pdfcraft_cos::Dict| {
                let keep = by_user(d);
                if keep {
                    counted.set(counted.get().saturating_add(1));
                }
                keep
            };
            let keep = |f: &pdfcraft_forms::Field| submission.sends(f);
            let value = |f: &pdfcraft_forms::Field| submission.values(f);
            let file = if submission.no_file_key { None } else { Some(doc.name.as_str()) };
            let data = pdfcraft_xfdf::SubmitData {
                keep: &keep,
                value: &value,
                comments: submission.annotations.then_some(&comment as &dyn Fn(&pdfcraft_cos::Dict) -> bool),
                file,
                differences: differences.as_deref(),
                embedded_pdf: embedded.as_deref(),
            };
            let mut body = guard(|| match format {
                SubmitFormat::Xfdf => pdfcraft_xfdf::submit_xfdf(&cos, &data).into_bytes(),
                SubmitFormat::Html => pdfcraft_xfdf::submit_html(&cos, &data).into_bytes(),
                _ => pdfcraft_xfdf::submit_fdf(&cos, &data),
            })
            .map_err(EditError::Write)?;
            annotations = counted.get();
            if submission.embed_form && submission.no_file_key {
                notes.push("ExclFKey: the embedded document needs /F, so it was kept.".into());
            }
            if format == SubmitFormat::Html && submission.coordinates {
                match cx.button.as_deref().and_then(|b| doc.form.iter().find(|f| f.name == b)) {
                    Some(button) => {
                        // The mapping name (/TM) when the button has one, else its name.
                        let name = cos
                            .get(button.obj)
                            .as_dict()
                            .and_then(|d| d.get(b"TM").and_then(|t| cos.resolve(t).as_string().map(|s| s.to_text())))
                            .filter(|n| !n.is_empty())
                            .unwrap_or_else(|| button.name.clone());
                        let [x, y] = cx.click.unwrap_or([0.0, 0.0]);
                        let whole = |v: f64| if v.is_finite() { v.round().clamp(-1e9, 1e9) as i64 } else { 0 };
                        let name = pdfcraft_xfdf::form_urlencode(&name);
                        let pair = format!("{name}.x={}&{name}.y={}", whole(x), whole(y));
                        if !body.is_empty() {
                            body.push(b'&');
                        }
                        body.extend_from_slice(pair.as_bytes());
                        if cx.click.is_none() {
                            notes.push("SubmitCoordinates: no click position was given, so 0,0 was sent.".into());
                        }
                    }
                    None => notes.push("SubmitCoordinates: no submitting button was given, so no click position was sent.".into()),
                }
            }
            body
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
        let stem = std::path::Path::new(&doc.name)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "form".into());
        Ok(Payload {
            url,
            get,
            format,
            content_type: format.content_type(),
            body: if get { Vec::new() } else { body },
            filename: format!("{stem}.{}", format.extension()),
            fields: if format == SubmitFormat::Pdf { Vec::new() } else { fields },
            annotations,
            notes,
        })
    }
}

/// Send `payload` to its address and return the server's status. Redirects are not followed
/// (the user confirmed one address); HTTPS uses the system's trusted roots.
#[cfg(not(target_arch = "wasm32"))]
pub fn send(payload: &Payload) -> Result<Reply, String> {
    send_with(payload, &SendOptions::default())
}

/// [`send`] with a time limit and a cap on the response read (both clamped to their maxima).
#[cfg(not(target_arch = "wasm32"))]
pub fn send_with(payload: &Payload, options: &SendOptions) -> Result<Reply, String> {
    use std::io::Read as _;
    use std::time::Duration;
    let target = destination(&payload.url)?;
    let timeout = options.timeout_seconds.clamp(1, MAX_TIMEOUT_SECONDS);
    let cap = options.max_response.min(MAX_RESPONSE);
    let roots = rustls_native_certs::load_native_certs();
    let roots: Vec<_> = roots.certs.iter().map(|cert| ureq::tls::Certificate::from_der(cert.as_ref()).to_owned()).collect();
    if roots.is_empty() && target.scheme() == "https" {
        return Err("No system TLS trust roots are available.".into());
    }
    let mut config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout)))
        .timeout_connect(Some(Duration::from_secs(timeout.min(10))))
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
    let mut response = if payload.get {
        agent.get(target.as_str()).header("User-Agent", agent_name).call()
    } else {
        agent.post(target.as_str()).header("Content-Type", payload.content_type).header("User-Agent", agent_name).send(payload.body.as_slice())
    }
    .map_err(|e| format!("The form couldn't be sent: {e}"))?;
    let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    let content_type = header("Content-Type").unwrap_or_default();
    let location = header("Location");
    let status = response.status().as_u16();
    // One byte past the cap tells a full answer from a cut one.
    let mut body = Vec::new();
    let limit = u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1);
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .reader()
        .take(limit)
        .read_to_end(&mut body)
        .map_err(|e| format!("The form was sent (status {status}), but its answer couldn't be read: {e}"))?;
    let truncated = body.len() > cap;
    body.truncate(cap);
    Ok(Reply { status, content_type, body, truncated, location })
}

/// The browser build doesn't send forms.
#[cfg(target_arch = "wasm32")]
pub fn send(_: &Payload) -> Result<Reply, String> {
    Err("Sending forms isn't available in the browser version. Save the data instead.".into())
}

/// The browser build doesn't send forms.
#[cfg(target_arch = "wasm32")]
pub fn send_with(_: &Payload, _: &SendOptions) -> Result<Reply, String> {
    Err("Sending forms isn't available in the browser version. Save the data instead.".into())
}

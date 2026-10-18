//! Submit Form (ISO 32000-2 §12.7.6.2): `form_submit_prepare` builds exactly what a Submit
//! Form button or action would send (address, method, content type, body) without sending
//! anything; `form_submit_send` sends it over HTTP(S) once the caller confirms, standing in
//! for the viewer's security prompt, or prepares the email a `mailto:` action asks for.

use base64::Engine as _;
use pdfcraft_engine::submit::{self, Context, Email, Payload, SendOptions, Submission, SubmitFormat, Target, flag};
use pdfcraft_engine::url_access::UrlDecision;
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

/// Bodies and attachments up to this size are returned inline (base64); larger ones need a path.
const MAX_INLINE: usize = 8 << 20;
/// At most this many field names are accepted in `fields`.
const MAX_FIELDS: usize = 10_000;
/// Bodies up to this size that are UTF-8 text are also returned as text.
const MAX_TEXT: usize = 1 << 20;

fn method(p: &Payload, email: bool) -> &'static str {
    if email {
        "EMAIL"
    } else if p.get {
        "GET"
    } else {
        "POST"
    }
}

fn flag_names(bits: u32) -> Vec<&'static str> {
    flag::NAMES.iter().filter(|(b, _)| bits & b != 0).map(|(_, n)| *n).collect()
}

fn email_json(e: &Email) -> Value {
    json!({ "to": e.to, "cc": e.cc, "bcc": e.bcc, "subject": e.subject, "body": e.body })
}

/// The body as text when it is short UTF-8 (FDF and PDF bodies are binary).
fn text_of(body: &[u8], format: SubmitFormat) -> Option<&str> {
    (format != SubmitFormat::Pdf && body.len() <= MAX_TEXT).then(|| std::str::from_utf8(body).ok()).flatten()
}

impl Automation {
    /// The submission the arguments describe, and what the viewer would know about the click.
    pub(crate) fn submission_args(&self, a: &Args) -> Result<(Submission, Context)> {
        let doc = self.doc(a)?;
        let button = a.opt_str("field")?;
        let mut submission = match (button, a.opt_str("url")?) {
            (Some(name), url) => {
                let f = doc.form.iter().find(|f| f.name == name).ok_or_else(|| bad(format!("no field {name:?}")))?;
                let mut s = match &f.button {
                    Some(pdfcraft_engine::form_scripts::ButtonAction::Submit(s)) => s.clone(),
                    _ => return Err(bad(format!("{name:?} is not a button that submits the form (give url instead)"))),
                };
                if let Some(u) = url {
                    s.url = u.to_string();
                }
                s
            }
            (None, Some(url)) => Submission::to_url(url),
            (None, None) => return Err(bad("give field (a Submit Form button) or url")),
        };
        // Flags: a /Flags integer and/or flag names; given, they replace the action's flags.
        let mut bits = None;
        if let Some(n) = a.opt_int("flags")? {
            let n = u32::try_from(n).map_err(|_| bad("flags must be a /Flags value from 0 to 4294967295"))?;
            bits = Some(n & flag::ALL);
        }
        if let Some(v) = a.get("options") {
            let list = v.as_array().ok_or_else(|| bad("options must be an array of flag names"))?;
            let mut n = bits.unwrap_or(0);
            for o in list.iter().take(64) {
                let name = o.as_str().ok_or_else(|| bad("options must be an array of flag names"))?;
                let names: Vec<&str> = flag::NAMES.iter().map(|(_, n)| *n).collect();
                n |= flag::by_name(name).ok_or_else(|| bad(format!("unknown flag {name:?} (one of {})", names.join(", "))))?;
            }
            bits = Some(n);
        }
        let fields = match a.get("fields") {
            Some(v) => {
                let arr = v.as_array().ok_or_else(|| bad("fields must be an array of field names"))?;
                if arr.len() > MAX_FIELDS {
                    return Err(bad(format!("fields lists at most {MAX_FIELDS} names")));
                }
                Some(
                    arr.iter()
                        .map(|x| x.as_str().map(str::to_string).ok_or_else(|| bad("fields must be an array of field names")))
                        .collect::<Result<Vec<_>>>()?,
                )
            }
            None => None,
        };
        if bits.is_some() || fields.is_some() {
            let flags = bits.unwrap_or_else(|| submission.flags());
            submission = Submission::with_flags(&submission.url, flags, fields.unwrap_or_else(|| submission.fields.clone()));
        }
        if let Some(format) = a.opt_str("format")? {
            let format = match format {
                "fdf" => SubmitFormat::Fdf,
                "xfdf" => SubmitFormat::Xfdf,
                "html" => SubmitFormat::Html,
                "pdf" => SubmitFormat::Pdf,
                f => return Err(bad(format!("unknown format {f:?} (fdf, xfdf, html, pdf)"))),
            };
            submission = submission.in_format(format);
        }
        let click = match a.get("click") {
            Some(v) => {
                let p =
                    v.as_array().filter(|p| p.len() == 2).ok_or_else(|| bad("click must be [x, y] in points from the button's top-left corner"))?;
                let n = |i: usize| p.get(i).and_then(Value::as_f64).filter(|f| f.is_finite()).ok_or_else(|| bad("click must be two finite numbers"));
                Some([n(0)?, n(1)?])
            }
            None => None,
        };
        let user = a.opt_str("user")?.map(str::to_string);
        Ok((submission, Context { button: button.map(str::to_string), click, user }))
    }

    /// The JSON both tools share: what is sent, where, and how.
    fn payload_json(submission: &Submission, payload: &Payload, email: bool) -> Value {
        let bits = submission.flags();
        json!({
            "url": payload.url, "method": method(payload, email), "format": submission.format.label(),
            "content_type": payload.content_type, "bytes": payload.body.len(), "filename": payload.filename,
            "flags": bits, "flag_names": flag_names(bits), "fields": payload.fields,
            "annotations": payload.annotations, "notes": payload.notes,
        })
    }

    pub(crate) fn form_submit_prepare(&mut self, a: &Args) -> Result<Value> {
        let (submission, cx) = self.submission_args(a)?;
        let id = self.doc(a)?.id;
        let payload = self.session.submission_in(id, &submission, &cx).map_err(failed)?;
        let target = submit::target(&submission.url);
        let email = matches!(target, Ok(Target::Email(_)));
        let mut out = Self::payload_json(&submission, &payload, email);
        match &target {
            Ok(Target::Web(_)) => {
                out["target"] = json!("web");
                out["sendable"] = json!(true);
            }
            Ok(Target::Email(e)) => {
                out["target"] = json!("email");
                out["sendable"] = json!(true);
                out["email"] = email_json(e);
            }
            Err(problem) => {
                out["target"] = Value::Null;
                out["sendable"] = json!(false);
                out["problem"] = json!(problem);
            }
        }
        if !submission.url.trim().is_empty() {
            let check = self.session.check_url(&submission.url);
            out["url_access"] = crate::url_access::check_json(&submission.url, &check);
        }
        if let Some(text) = text_of(&payload.body, payload.format) {
            out["body"] = json!(text);
        }
        if payload.body.len() <= MAX_INLINE {
            out["body_base64"] = json!(base64::engine::general_purpose::STANDARD.encode(&payload.body));
        } else {
            out["body_omitted"] = json!(format!("the body is over {} MiB; save it with form_submit path instead", MAX_INLINE >> 20));
        }
        Ok(out)
    }

    pub(crate) fn form_submit_send(&mut self, a: &Args) -> Result<Value> {
        if a.opt_bool("confirm")? != Some(true) {
            return Err(bad(
                "form_submit_send sends the form's data off this machine: pass confirm: true once the user has agreed to send it there (see form_submit_prepare for what would be sent)",
            ));
        }
        let (submission, cx) = self.submission_args(a)?;
        let target = submit::target(&submission.url).map_err(bad)?;
        let timeout_seconds = match a.opt_int("timeout_seconds")? {
            Some(t) if (1..=submit::MAX_TIMEOUT_SECONDS as i64).contains(&t) => t as u64,
            Some(_) => return Err(bad(format!("timeout_seconds must be from 1 to {}", submit::MAX_TIMEOUT_SECONDS))),
            None => submit::TIMEOUT_SECONDS,
        };
        let max_response = match a.opt_int("max_response_bytes")? {
            Some(n) if (0..=submit::MAX_RESPONSE as i64).contains(&n) => n as usize,
            Some(_) => return Err(bad(format!("max_response_bytes must be from 0 to {}", submit::MAX_RESPONSE))),
            None => submit::DEFAULT_RESPONSE,
        };
        let import_reply = a.opt_bool("import_reply")?.unwrap_or(false);
        let attachment_path = a.opt_str("attachment_path")?;
        if attachment_path.is_some() && !matches!(target, Target::Email(_)) {
            return Err(bad("attachment_path is for mailto: addresses only"));
        }
        let attachment_path = attachment_path.map(|p| self.resolve(p, true)).transpose()?;
        let check = self.session.check_url(&submission.url);
        if check.decision == UrlDecision::Block {
            return Err(failed(format!(
                "blocked: the form wasn't sent to {}: {} (see trust_urls_get; save the data with form_submit path instead)",
                submission.url.trim(),
                check.reason
            )));
        }
        let id = self.doc(a)?.id;
        let payload = self.session.submission_in(id, &submission, &cx).map_err(failed)?;
        let email = matches!(target, Target::Email(_));
        let mut out = Self::payload_json(&submission, &payload, email);
        out["url_access"] = crate::url_access::check_json(&submission.url, &check);
        match target {
            Target::Email(mut e) => {
                // PdfCraft doesn't send email: the agent or the user's mail program does.
                if e.subject.is_none() {
                    e.subject = Some(format!("Form data: {}", self.doc(a)?.name));
                }
                let mut attachment = json!({ "name": payload.filename, "content_type": payload.content_type, "bytes": payload.body.len() });
                if let Some(path) = attachment_path {
                    crate::write_atomic(&path, &payload.body)?;
                    attachment["path"] = json!(path.to_string_lossy());
                } else if payload.body.len() <= MAX_INLINE {
                    attachment["base64"] = json!(base64::engine::general_purpose::STANDARD.encode(&payload.body));
                } else {
                    return Err(bad(format!("the attachment is over {} MiB; give attachment_path to save it", MAX_INLINE >> 20)));
                }
                let mut mail = email_json(&e);
                mail["attachment"] = attachment;
                out["target"] = json!("email");
                out["sent"] = json!(false);
                out["email"] = mail;
            }
            Target::Web(_) => {
                let reply = submit::send_with(&payload, &SendOptions { timeout_seconds, max_response }).map_err(failed)?;
                out["target"] = json!("web");
                out["sent"] = json!(true);
                out["status"] = json!(reply.status);
                out["ok"] = json!((200..300).contains(&reply.status));
                out["reply_content_type"] = json!(reply.content_type);
                out["reply_bytes"] = json!(reply.body.len());
                out["reply_truncated"] = json!(reply.truncated);
                if let Some(l) = reply.location {
                    out["location"] = json!(l);
                }
                let text = (reply.body.len() <= MAX_TEXT).then(|| std::str::from_utf8(&reply.body).ok()).flatten();
                match text {
                    Some(t) => out["reply"] = json!(t),
                    None => out["reply_base64"] = json!(base64::engine::general_purpose::STANDARD.encode(&reply.body)),
                }
                // A server may answer with FDF or XFDF for the viewer to load into the form.
                let ct = reply.content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
                let data_reply = matches!(ct.as_str(), "application/vnd.fdf" | "application/vnd.adobe.xfdf");
                if import_reply && data_reply && (200..300).contains(&reply.status) {
                    if reply.truncated {
                        out["imported"] = json!(false);
                        out["import_problem"] = json!("the answer was cut at max_response_bytes, so it wasn't imported");
                    } else {
                        let name = if ct.ends_with("xfdf") { "reply.xfdf" } else { "reply.fdf" };
                        let edit = pdfcraft_engine::Edit::ImportData { name: name.into(), bytes: std::sync::Arc::new(reply.body) };
                        match self.apply(a, edit) {
                            Ok(_) => out["imported"] = json!(true),
                            Err(e) => {
                                out["imported"] = json!(false);
                                out["import_problem"] = json!(e.to_string());
                            }
                        }
                    }
                } else if data_reply {
                    out["imported"] = json!(false);
                }
            }
        }
        Ok(out)
    }
}

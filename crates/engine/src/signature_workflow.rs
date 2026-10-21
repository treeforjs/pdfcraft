//! User-requested timestamp transport and detached signature jobs. The sign crate remains
//! transport-free; opening or validating a document never fetches anything.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use pdfcraft_sign::{Certificate, SignError, Time, TimestampAuthority, TrustStore};

use crate::{DocId, EditError, Session, SignOptions, SignatureInfo};

fn is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    }
}

/// A configured timestamp authority. No credentials are kept in URLs or preferences.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimestampServer {
    pub name: String,
    pub url: String,
    pub policy: Option<String>,
    pub timeout_seconds: u32,
    /// Untrusted certificates are never added to the trust store automatically.
    pub require_trusted_authority: bool,
}

impl TimestampServer {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.name.len() > 200 || self.url.len() > 4096 {
            return Err("A timestamp server needs a name (at most 200 bytes) and a URL of at most 4096 bytes.".into());
        }
        let url = url::Url::parse(&self.url).map_err(|_| "The timestamp server URL is invalid.")?;
        let loopback = is_loopback(&url);
        if url.host_str().is_none() || (url.scheme() != "https" && !(url.scheme() == "http" && loopback)) {
            return Err("Use HTTPS for a timestamp server; HTTP is accepted only for an explicit loopback IP address.".into());
        }
        if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return Err("Timestamp URLs must not contain credentials or fragments.".into());
        }
        if !(1..=30).contains(&self.timeout_seconds) {
            return Err("Timestamp timeout must be between 1 and 30 seconds.".into());
        }
        if let Some(policy) = &self.policy {
            if policy.len() > 200 {
                return Err("The timestamp policy OID is too long.".into());
            }
            pdfcraft_sign::der::try_oid(policy).map_err(|_| "The timestamp policy must be a numeric object identifier.")?;
        }
        Ok(())
    }
}

/// Shared cancellation token; a cancelled response is discarded before any PDF is saved.
#[derive(Clone, Default)]
pub struct SignatureCancellation(Arc<AtomicBool>);

impl SignatureCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    /// The same Cancel action interrupts identity derivation before the signing operation.
    pub fn identity_import_control(&self) -> pdfcraft_sign::pkcs12::ImportControl {
        pdfcraft_sign::pkcs12::ImportControl::with_cancellation(self.0.clone())
    }
    fn check(&self) -> Result<(), SignError> {
        if self.is_cancelled() { Err(SignError::Pdf("timestamp operation cancelled".into())) } else { Ok(()) }
    }
}

pub struct HttpTimestampAuthority {
    server: TimestampServer,
    trust: TrustStore,
    cancellation: SignatureCancellation,
}

impl HttpTimestampAuthority {
    /// Constructing a transport does not send a request. Callers must obtain the server from
    /// an explicit user action or an automation call with network access opted in.
    pub fn new(server: TimestampServer, certificates: Vec<Certificate>, cancellation: SignatureCancellation) -> Result<Self, String> {
        server.validate()?;
        Ok(Self { server, trust: TrustStore::new(certificates), cancellation })
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn checked_response(&self, response: &[u8], query: &pdfcraft_sign::TimestampQuery) -> Result<(), SignError> {
        self.cancellation.check()?;
        let token = pdfcraft_sign::timestamp::parse_response(response, query)?;
        let signer = token.signer_certificate().ok_or_else(|| SignError::Malformed("timestamp signer certificate is missing".into()))?;
        if !signer.valid_at(token.gen_time) {
            return Err(SignError::Crypto("The timestamp authority certificate was not valid when it issued the token.".into()));
        }
        if self.server.require_trusted_authority {
            let mut pool = pdfcraft_sign::timestamp::token_certs(&token.raw);
            pool.extend(self.trust.certs.iter().cloned());
            let chain = pdfcraft_sign::x509::build_chain(&signer, &pool, Some(token.gen_time));
            if !chain.iter().any(|cert| self.trust.certs.iter().any(|trusted| cert.raw == trusted.raw)) {
                return Err(SignError::Crypto("The timestamp authority is not trusted. Import its independently verified certificate chain, or explicitly allow an untrusted timestamp; no certificate was trusted automatically.".into()));
            }
        }
        Ok(())
    }
}

impl TimestampAuthority for HttpTimestampAuthority {
    #[cfg(not(target_arch = "wasm32"))]
    fn timestamp(&self, request: &[u8]) -> Result<Vec<u8>, SignError> {
        use std::{io::Read, time::Duration};
        self.cancellation.check()?;
        let mut query = pdfcraft_sign::timestamp::parse_request(request)?;
        if query.nonce.is_none() {
            query.nonce = pdfcraft_sign::TimestampQuery::online(query.digest, query.imprint.clone())?.nonce;
        }
        if self.server.policy.is_some() {
            query.policy.clone_from(&self.server.policy);
        }
        let request = query.encode()?;
        let roots = rustls_native_certs::load_native_certs();
        let roots: Vec<_> = roots.certs.iter().map(|cert| ureq::tls::Certificate::from_der(cert.as_ref()).to_owned()).collect();
        if roots.is_empty() && self.server.url.starts_with("https:") {
            return Err(SignError::Crypto("No system TLS trust roots are available.".into()));
        }
        let mut config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(u64::from(self.server.timeout_seconds))))
            .timeout_connect(Some(Duration::from_secs(5)))
            .max_redirects(0)
            .tls_config(ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::new_with_certs(&roots)).build());
        // A literal loopback endpoint is local by construction; do not send synthetic/local
        // authorities through an environment proxy. HTTPS otherwise follows OS/proxy policy.
        if url::Url::parse(&self.server.url).is_ok_and(|url| is_loopback(&url)) {
            config = config.proxy(None);
        }
        let agent = config.build().new_agent();
        let mut response = agent
            .post(&self.server.url)
            .header("Content-Type", "application/timestamp-query")
            .header("Accept", "application/timestamp-reply")
            .header("User-Agent", concat!("PdfCraft/", env!("CARGO_PKG_VERSION")))
            .send(request.as_slice())
            .map_err(|error| SignError::Pdf(format!("Timestamp request failed: {error}")))?;
        if response.status().as_u16() != 200 {
            return Err(SignError::Pdf(format!("Timestamp server returned HTTP {}; redirects are not followed.", response.status())));
        }
        let content_type = response.headers().get("Content-Type").and_then(|value| value.to_str().ok()).unwrap_or("");
        if !content_type.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/timestamp-reply") {
            return Err(SignError::Pdf("Timestamp response must have Content-Type application/timestamp-reply.".into()));
        }
        const LIMIT: usize = 4 * 1024 * 1024;
        let mut reader = response.body_mut().as_reader();
        let mut out = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            self.cancellation.check()?;
            let count = reader.read(&mut buffer).map_err(|error| SignError::Pdf(format!("Timestamp response could not be read: {error}")))?;
            if count == 0 {
                break;
            }
            if out.len().saturating_add(count) > LIMIT {
                return Err(SignError::Pdf("Timestamp response exceeds the 4 MiB limit.".into()));
            }
            out.extend_from_slice(&buffer[..count]);
        }
        self.checked_response(&out, &query)?;
        Ok(out)
    }

    #[cfg(target_arch = "wasm32")]
    fn timestamp(&self, _request: &[u8]) -> Result<Vec<u8>, SignError> {
        self.cancellation.check()?;
        let _ = (&self.server, &self.trust);
        Err(SignError::Unsupported("online timestamp transport is available in native PdfCraft; browser transport is not configured".into()))
    }
}

/// A detached, revision-bound job. Network latency never requires holding a mutable Session.
pub struct SignatureJob {
    doc: DocId,
    generation: u64,
    source: Arc<Vec<u8>>,
    cos: pdfcraft_cos::Document,
    date: String,
    trust: TrustStore,
}

pub struct SignatureResult {
    pub doc: DocId,
    generation: u64,
    source: Arc<Vec<u8>>,
    pub bytes: Arc<Vec<u8>>,
}

impl SignatureJob {
    pub fn sign(
        self,
        id: &pdfcraft_sign::DigitalId,
        mut options: SignOptions,
        authority: Option<&dyn TimestampAuthority>,
    ) -> Result<SignatureResult, EditError> {
        if options.date.is_empty() {
            options.date.clone_from(&self.date);
        }
        let bytes = match authority {
            Some(authority) => pdfcraft_sign::sign_with_timestamp(&self.cos, id, &options, authority)?,
            None => pdfcraft_sign::sign(&self.cos, id, &options)?,
        };
        Ok(self.finish(bytes))
    }
    pub fn timestamp(self, authority: &dyn TimestampAuthority) -> Result<SignatureResult, EditError> {
        let bytes = pdfcraft_sign::timestamp_document(&self.cos, authority, &self.date)?;
        Ok(self.finish(bytes))
    }
    pub fn embed_evidence(
        self,
        evidence: &pdfcraft_sign::dss::Evidence,
        when: pdfcraft_sign::VerificationTime,
        authority: Option<&dyn TimestampAuthority>,
    ) -> Result<(SignatureResult, pdfcraft_sign::evidence::EvidenceReport), EditError> {
        let report = pdfcraft_sign::evidence::check(&self.cos, &self.source, evidence, &self.trust, when)?;
        let mut bytes = pdfcraft_sign::dss::embed(&self.cos, evidence)?;
        if let Some(authority) = authority {
            let doc = pdfcraft_cos::Document::open(Arc::new(bytes)).map_err(|e| EditError::Sign(e.to_string()))?;
            bytes = pdfcraft_sign::timestamp_document(&doc, authority, &self.date)?;
        }
        Ok((self.finish(bytes), report))
    }
    fn finish(self, bytes: Vec<u8>) -> SignatureResult {
        SignatureResult { doc: self.doc, generation: self.generation, source: self.source, bytes: Arc::new(bytes) }
    }
}

impl Session {
    pub fn signature_job(&self, id: DocId) -> Result<SignatureJob, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let editor = doc.editor.as_ref().ok_or_else(|| EditError::ReadOnly(doc.read_only_reason.clone().unwrap_or_default()))?;
        Ok(SignatureJob {
            doc: id,
            generation: doc.edit_generation(),
            source: doc.bytes.clone(),
            cos: editor.cos.clone(),
            date: self.signing_date(),
            trust: (*doc.trust).clone(),
        })
    }
    /// Check immediately before writing the result. A stale result must not overwrite the
    /// current document or a destination file after the user has continued editing.
    pub fn check_signature_result(&self, result: &SignatureResult) -> Result<(), EditError> {
        let doc = self.get(result.doc).ok_or(EditError::NoDocument)?;
        if doc.edit_generation() != result.generation || !Arc::ptr_eq(&doc.bytes, &result.source) {
            return Err(EditError::Invalid(
                "The document changed while signing or timestamping. Run the operation again; no result was saved.".into(),
            ));
        }
        Ok(())
    }
    /// The signature in field `field` (the first signed one of that name).
    fn signed_signature(&self, id: DocId, field: &str) -> Result<SignatureInfo, String> {
        let doc = self.get(id).ok_or("no such document")?;
        let Some(sig) = doc.signatures.iter().find(|s| s.field == field && s.signed) else {
            let names: Vec<&str> = doc.signatures.iter().filter(|s| s.signed).map(|s| s.field.as_str()).collect();
            return Err(if names.is_empty() {
                "the document has no signed signatures".into()
            } else {
                format!("no signed signature named {field:?} (signed: {})", names.join(", "))
            });
        };
        Ok(sig.clone())
    }

    /// View Signed Version: the bytes the signature in `field` covers (the file as it was
    /// signed), with the signature.
    pub fn signed_version_bytes(&self, id: DocId, field: &str) -> Result<(SignatureInfo, Vec<u8>), String> {
        let doc = self.get(id).ok_or("no such document")?;
        if doc.certificate_secured() {
            return Err("the signed version of a certificate-encrypted document can't be extracted separately".into());
        }
        let sig = self.signed_signature(id, field)?;
        let bytes = pdfcraft_sign::signed_version(&doc.bytes, &sig).map_err(|e| e.to_string())?.to_vec();
        Ok((sig, bytes))
    }

    /// View Signed Version: open the revision the signature in `field` covers as a new,
    /// unsaved document named "<name> (signed version)".
    pub fn open_signed_version(&mut self, id: DocId, field: &str) -> Result<DocId, String> {
        let (_, bytes) = self.signed_version_bytes(id, field)?;
        let doc = self.get(id).ok_or("no such document")?;
        let name = format!("{} (signed version).pdf", doc.name.trim_end_matches(".pdf"));
        let password = doc.password.clone();
        self.open(name, None, Arc::new(bytes), password.as_deref()).map_err(|e| e.to_string())
    }

    /// Compare Signed Version to Current Version: every change made after the signature in
    /// `field` was applied, each with whether its DocMDP/FieldMDP permissions allow it.
    pub fn compare_signed_version(&self, id: DocId, field: &str) -> Result<pdfcraft_sign::SignedVersionComparison, String> {
        let doc = self.get(id).ok_or("no such document")?;
        if doc.certificate_secured() {
            return Err("the signed version of a certificate-encrypted document can't be compared separately".into());
        }
        let editor = doc.editor.as_ref().ok_or_else(|| doc.read_only_reason.clone().unwrap_or_else(|| "the document can't be read".into()))?;
        let sig = self.signed_signature(id, field)?;
        crate::guard(|| pdfcraft_sign::compare_signed_version(&editor.cos, &doc.bytes, &sig, doc.password.as_deref()))
            .map_err(|m| format!("comparing failed unexpectedly ({m})"))?
            .map_err(|e| e.to_string())
    }

    /// Read-only validation at an explicit time; signatures and trust are never changed.
    pub fn verify_signatures_at(&self, id: DocId, when: pdfcraft_sign::VerificationTime) -> Result<Vec<SignatureInfo>, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let editor = doc.editor.as_ref().ok_or_else(|| EditError::ReadOnly(doc.read_only_reason.clone().unwrap_or_default()))?;
        Ok(pdfcraft_sign::pdf::list_cached_with_time(&editor.cos, &doc.bytes, &doc.trust, &doc.sig_cache, when))
    }
}

/// Accept a complete UTC date, never a partially parsed or normalized calendar value.
pub fn verification_time_utc(value: &str) -> Result<Time, String> {
    if value.len() != 15 || !value.ends_with('Z') || !value.as_bytes().iter().take(14).all(u8::is_ascii_digit) {
        return Err("Use a complete UTC date: YYYYMMDDHHmmSSZ.".into());
    }
    let time = Time::from_pdf(value).ok_or("The verification date is invalid.")?;
    if !(1..=9999).contains(&time.year) || Time::from_unix(time.unix()) != time {
        return Err("The verification date is not a valid UTC calendar date.".into());
    }
    Ok(time)
}

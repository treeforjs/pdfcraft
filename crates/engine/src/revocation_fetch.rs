//! Add verification information to signatures (Acrobat: Add Verification Information, "LTV
//! enable"): for every certificate in each signer's and timestamp authority's chain, fetch its
//! status from the OCSP responder its certificate names (Authority Information Access), or its
//! issuer's CRL (CRL Distribution Points), verify what comes back against the issuer, and
//! embed it with the chains in the Document Security Store.
//!
//! The network is reached only from an explicit action (never on opening or validating), only
//! for `http:` and `https:` addresses the internet-access policy allows ([`crate::url_access`]),
//! from a worker (never the UI thread), with a deadline per request and overall, a response
//! size cap and no redirects, and the number of requests capped. Every response is hostile
//! until its signature verifies; one that doesn't is dropped and reported, never embedded.
//! Without network access the certificates, and any evidence already in the document or
//! supplied as files, are still embedded.

use std::time::{Duration, Instant};

use pdfcraft_sign::revocation::{CertificateList, OcspResponse, RevocationStatus, ocsp_request};
use pdfcraft_sign::{Certificate, SignatureInfo, Time, TrustPurpose, TrustStore, dss::Evidence};

use crate::url_access::{UrlDecision, UrlPolicy, check_url};

/// Requests one run may make, and the evidence it may gather.
pub const MAX_REQUESTS: usize = 32;
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: usize = 24 * 1024 * 1024;
/// Responder and distribution-point addresses tried per certificate.
const MAX_URLS_PER_CERT: usize = 2;
/// The longest address accepted from a certificate.
const MAX_URL_LEN: usize = 2048;
/// Certificates (signer chains, authority chains) one run looks at; with the responses this
/// stays within what [`pdfcraft_sign::evidence::check`] accepts (64 items).
const MAX_CERTS: usize = 32;

/// How a run may use the network.
#[derive(Clone, Debug)]
pub struct FetchPolicy {
    /// No request at all when false (the evidence is gathered from the document only).
    pub network: bool,
    /// The internet-access policy every address is checked against.
    pub urls: UrlPolicy,
    /// Treat addresses the policy would ask about as allowed: the user agreed in the dialog
    /// (or an automation call passed `allow_prompted`). Blocked addresses stay blocked.
    pub allow_prompted: bool,
    /// Seconds per request (1–30).
    pub timeout_seconds: u32,
    /// Seconds for the whole run.
    pub total_seconds: u32,
    /// Maximum age of evidence without a next update, seconds.
    pub max_age: Option<u64>,
}

impl Default for FetchPolicy {
    fn default() -> Self {
        Self { network: false, urls: UrlPolicy::default(), allow_prompted: false, timeout_seconds: 10, total_seconds: 60, max_age: None }
    }
}

/// Fetches bytes over the network. The real one is [`HttpTransport`]; tests can substitute
/// their own, and the browser build has none.
pub trait RevocationTransport: Send + Sync {
    /// POST an OCSP request (`application/ocsp-request`) and return the response body.
    fn ocsp(&self, url: &str, request: &[u8], timeout: Duration) -> Result<Vec<u8>, String>;
    /// GET a CRL.
    fn crl(&self, url: &str, timeout: Duration) -> Result<Vec<u8>, String>;
}

/// The native HTTP transport: system TLS roots, no redirects, no cookies, bounded bodies.
pub struct HttpTransport {
    pub cancellation: crate::signature_workflow::SignatureCancellation,
}

#[cfg(not(target_arch = "wasm32"))]
impl HttpTransport {
    fn agent(&self, url: &str, timeout: Duration) -> Result<ureq::Agent, String> {
        let roots = rustls_native_certs::load_native_certs();
        let roots: Vec<_> = roots.certs.iter().map(|c| ureq::tls::Certificate::from_der(c.as_ref()).to_owned()).collect();
        if roots.is_empty() && url.starts_with("https:") {
            return Err("No system TLS trust roots are available.".into());
        }
        let mut config = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .timeout_connect(Some(timeout.min(Duration::from_secs(5))))
            .max_redirects(0)
            .http_status_as_error(false)
            .tls_config(ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::new_with_certs(&roots)).build());
        // Loopback endpoints (local responders, tests) are not sent through a proxy.
        if url::Url::parse(url).is_ok_and(|u| match u.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        }) {
            config = config.proxy(None);
        }
        Ok(config.build().new_agent())
    }

    fn body(&self, mut response: ureq::http::Response<ureq::Body>) -> Result<Vec<u8>, String> {
        use std::io::Read;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(format!("the server returned HTTP {status} (redirects are not followed)"));
        }
        let mut reader = response.body_mut().as_reader();
        let mut out = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            if self.cancellation.is_cancelled() {
                return Err("cancelled".into());
            }
            let n = reader.read(&mut buffer).map_err(|e| format!("the response could not be read: {e}"))?;
            if n == 0 {
                break;
            }
            if out.len().saturating_add(n) > MAX_RESPONSE_BYTES {
                return Err("the response is larger than 2 MiB".into());
            }
            out.extend_from_slice(buffer.get(..n).unwrap_or_default());
        }
        Ok(out)
    }
}

impl RevocationTransport for HttpTransport {
    #[cfg(not(target_arch = "wasm32"))]
    fn ocsp(&self, url: &str, request: &[u8], timeout: Duration) -> Result<Vec<u8>, String> {
        let response = self
            .agent(url, timeout)?
            .post(url)
            .header("Content-Type", "application/ocsp-request")
            .header("Accept", "application/ocsp-response")
            .header("User-Agent", concat!("PdfCraft/", env!("CARGO_PKG_VERSION")))
            .send(request)
            .map_err(|e| format!("the request failed: {e}"))?;
        self.body(response)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn crl(&self, url: &str, timeout: Duration) -> Result<Vec<u8>, String> {
        let response = self
            .agent(url, timeout)?
            .get(url)
            .header("User-Agent", concat!("PdfCraft/", env!("CARGO_PKG_VERSION")))
            .call()
            .map_err(|e| format!("the request failed: {e}"))?;
        self.body(response)
    }

    #[cfg(target_arch = "wasm32")]
    fn ocsp(&self, _url: &str, _request: &[u8], _timeout: Duration) -> Result<Vec<u8>, String> {
        Err("fetching revocation information is available in the desktop app".into())
    }

    #[cfg(target_arch = "wasm32")]
    fn crl(&self, _url: &str, _timeout: Duration) -> Result<Vec<u8>, String> {
        Err("fetching revocation information is available in the desktop app".into())
    }
}

/// What happened to one address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchAttempt {
    /// The certificate whose status was asked for (its display name).
    pub certificate: String,
    pub url: String,
    /// `ocsp` or `crl`.
    pub kind: &'static str,
    /// `good`, `revoked`, `blocked`, `asks` (the policy would ask; not allowed this time),
    /// `skipped` (limits, another address already answered), `unusable` (didn't verify) or
    /// `failed` (network).
    pub outcome: &'static str,
    pub detail: String,
}

/// The evidence a run gathered, and what it tried.
#[derive(Clone, Debug, Default)]
pub struct Fetched {
    pub evidence: Evidence,
    pub attempts: Vec<FetchAttempt>,
    /// Certificates that got verified evidence (good or revoked).
    pub covered: usize,
    /// Certificates that needed evidence (not trust anchors, issuer known).
    pub needed: usize,
}

/// The chains a document's signatures need evidence for: (chain, purpose), leaf first.
fn chains(signatures: &[SignatureInfo]) -> Vec<(Vec<Certificate>, Option<Time>, TrustPurpose)> {
    let mut out = Vec::new();
    for s in signatures.iter().filter(|s| s.signed) {
        let purpose = if s.certify.is_some() { TrustPurpose::CertifiedDocuments } else { TrustPurpose::Signatures };
        if !s.chain.is_empty() {
            out.push((s.chain.clone(), s.verification_time, purpose));
        }
        if !s.timestamp_chain.is_empty() {
            out.push((s.timestamp_chain.clone(), s.verification_time, TrustPurpose::Signatures));
        }
    }
    out
}

/// An address from a certificate worth trying: http(s), short, parseable.
fn usable_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.len() > MAX_URL_LEN {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    (matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some() && parsed.username().is_empty() && parsed.password().is_none())
        .then(|| parsed.to_string())
}

/// Gather evidence for the signatures' chains: their certificates always, and with
/// `policy.network`, verified OCSP responses or CRLs for every certificate that needs them.
/// Each piece must verify for its certificate at `now` and at the signature's verification
/// time, so the validator and the embedding check accept it.
pub fn gather(signatures: &[SignatureInfo], trust: &TrustStore, policy: &FetchPolicy, transport: &dyn RevocationTransport, now: Time) -> Fetched {
    let mut out = Fetched::default();
    let started = Instant::now();
    let total = Duration::from_secs(u64::from(policy.total_seconds.clamp(1, 300)));
    let timeout = Duration::from_secs(u64::from(policy.timeout_seconds.clamp(1, 30)));
    let mut requests = 0usize;
    let mut bytes = 0usize;
    let mut done: Vec<Vec<u8>> = Vec::new();
    let mut seen = 0usize;
    for (chain, at, purpose) in chains(signatures) {
        for c in &chain {
            if !out.evidence.certs.contains(&c.raw) && out.evidence.certs.len() < MAX_CERTS {
                out.evidence.certs.push(c.raw.clone());
            }
        }
        for (i, cert) in chain.iter().enumerate() {
            let Some(issuer) = chain.get(i + 1) else { break };
            if trust.anchor(cert, purpose).is_some() || done.contains(&cert.raw) {
                continue;
            }
            seen += 1;
            if seen > MAX_CERTS {
                break;
            }
            done.push(cert.raw.clone());
            out.needed += 1;
            let name = cert.display_name();
            let attempt =
                |url: &str, kind, outcome, detail: String| FetchAttempt { certificate: name.clone(), url: url.to_string(), kind, outcome, detail };
            // A status counts when it is decided at `now` and at the signature's time.
            let decided = |status_now: RevocationStatus, status_then: RevocationStatus| -> Option<&'static str> {
                match (status_now, status_then) {
                    (RevocationStatus::Revoked { .. }, _) | (_, RevocationStatus::Revoked { .. }) => Some("revoked"),
                    (RevocationStatus::Good, RevocationStatus::Good) => Some("good"),
                    _ => None,
                }
            };
            let then = at.unwrap_or(now);
            let mut covered = false;
            let targets: Vec<(&'static str, String)> = cert
                .ocsp_urls
                .iter()
                .filter_map(|u| usable_url(u))
                .take(MAX_URLS_PER_CERT)
                .map(|u| ("ocsp", u))
                .chain(cert.crl_urls.iter().filter_map(|u| usable_url(u)).take(MAX_URLS_PER_CERT).map(|u| ("crl", u)))
                .collect();
            if targets.is_empty() {
                out.attempts.push(attempt("", "ocsp", "skipped", "the certificate names no OCSP responder or CRL".into()));
            }
            for (kind, url) in targets {
                if covered {
                    break;
                }
                if !policy.network {
                    out.attempts.push(attempt(&url, kind, "skipped", "network access was not allowed for this run".into()));
                    continue;
                }
                match check_url(&policy.urls, &url).decision {
                    UrlDecision::Allow => {}
                    UrlDecision::Prompt if policy.allow_prompted => {}
                    UrlDecision::Prompt => {
                        out.attempts.push(attempt(&url, kind, "asks", "internet access asks about this site; allow it to fetch".into()));
                        continue;
                    }
                    _ => {
                        out.attempts.push(attempt(&url, kind, "blocked", "blocked by Internet access settings".into()));
                        continue;
                    }
                }
                if requests >= MAX_REQUESTS || bytes >= MAX_TOTAL_BYTES || started.elapsed() >= total {
                    out.attempts.push(attempt(&url, kind, "skipped", "the request limit or time limit for this run was reached".into()));
                    continue;
                }
                requests += 1;
                let left = total.saturating_sub(started.elapsed()).min(timeout).max(Duration::from_secs(1));
                // Transports are untrusted code paths too: a panic in one is an error here.
                let body = crate::guard(|| match kind {
                    "ocsp" => transport.ocsp(&url, &ocsp_request(cert, issuer), left),
                    _ => transport.crl(&url, left),
                })
                .and_then(|r| r);
                let body = match body {
                    Ok(b) if b.len() > MAX_RESPONSE_BYTES => Err("the response is larger than 2 MiB".to_string()),
                    other => other,
                };
                let body = match body {
                    Ok(b) => b,
                    Err(e) => {
                        out.attempts.push(attempt(&url, kind, "failed", e));
                        continue;
                    }
                };
                bytes = bytes.saturating_add(body.len());
                let verdict = match kind {
                    "ocsp" => OcspResponse::parse(&body)
                        .map(|r| decided(r.check_with(cert, issuer, now, policy.max_age), r.check_with(cert, issuer, then, policy.max_age))),
                    _ => CertificateList::parse(&body)
                        .map(|l| decided(l.check_with(cert, issuer, now, policy.max_age), l.check_with(cert, issuer, then, policy.max_age))),
                };
                match verdict {
                    Ok(Some(status)) => {
                        let list = if kind == "ocsp" { &mut out.evidence.ocsps } else { &mut out.evidence.crls };
                        if !list.contains(&body) {
                            list.push(body);
                        }
                        out.attempts.push(attempt(&url, kind, status, String::new()));
                        covered = true;
                    }
                    Ok(None) => out.attempts.push(attempt(
                        &url,
                        kind,
                        "unusable",
                        "the response did not verify for this certificate, or is not current (check its signer and dates)".into(),
                    )),
                    Err(e) => out.attempts.push(attempt(&url, kind, "unusable", e.to_string())),
                }
            }
            if covered {
                out.covered += 1;
            }
        }
    }
    out
}

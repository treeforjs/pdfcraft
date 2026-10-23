//! Validate explicitly supplied local evidence before embedding it. A DSS is a container,
//! not a certificate of long-term validity: coverage and trust remain separate results.

use crate::{
    Certificate, SignError, SignatureInfo, Time, TrustStore, VerificationTime, dss,
    revocation::{CertificateList, OcspResponse, RevocationStatus},
};

const MAX_ITEMS: usize = 64;
const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_ITEM: usize = 2 * 1024 * 1024;

/// Which certificate chain a coverage entry belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverageRole {
    /// The chain of a field signature's signer.
    Signer,
    /// The authority chain of the RFC 3161 token embedded in a field signature.
    SignatureTimestamp,
    /// The authority chain of a standalone document timestamp (`/ETSI.RFC3161`).
    DocumentTimestamp,
}

impl CoverageRole {
    pub fn as_str(self) -> &'static str {
        match self {
            CoverageRole::Signer => "signer",
            CoverageRole::SignatureTimestamp => "signature_timestamp",
            CoverageRole::DocumentTimestamp => "document_timestamp",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertificateCoverage {
    pub field: String,
    pub role: CoverageRole,
    pub certificate_sha256: String,
    pub checked_at: Option<Time>,
    pub status: RevocationStatus,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvidenceReport {
    pub certificates: usize,
    pub crls: usize,
    pub ocsps: usize,
    pub coverage: Vec<CertificateCoverage>,
    pub notes: Vec<String>,
}

impl EvidenceReport {
    /// All checked non-anchor certificates have verified good evidence: signer chains, the
    /// authority chains of embedded signature timestamps, and document-timestamp authority
    /// chains. This is not a PAdES conformance declaration and does not confer trust on an
    /// identity.
    pub fn has_complete_coverage(&self) -> bool {
        !self.coverage.is_empty() && self.coverage.iter().all(|item| item.status == RevocationStatus::Good)
    }
}

/// Check sizes, DER syntax, cryptographic association and validity windows. Unrelated or
/// unverifiable CRLs/OCSP responses are refused, rather than published as validation data.
pub fn check(
    doc: &pdfcraft_cos::Document,
    bytes: &[u8],
    evidence: &dss::Evidence,
    trust: &TrustStore,
    when: VerificationTime,
) -> Result<EvidenceReport, SignError> {
    let items = evidence.certs.iter().chain(&evidence.crls).chain(&evidence.ocsps);
    let mut count = 0usize;
    let mut size = 0usize;
    for item in items {
        count = count.saturating_add(1);
        size = size.saturating_add(item.len());
        if item.is_empty() || item.len() > MAX_ITEM || count > MAX_ITEMS || size > MAX_BYTES {
            return Err(SignError::Pdf("Validation evidence is limited to 64 nonempty DER files, 2 MiB each and 32 MiB in total.".into()));
        }
    }
    if count == 0 {
        return Err(SignError::Pdf("Choose certificate, CRL or OCSP evidence files.".into()));
    }
    let certificates = evidence.certs.iter().map(|raw| Certificate::parse(raw)).collect::<Result<Vec<_>, _>>()?;
    let crls = evidence.crls.iter().map(|raw| CertificateList::parse(raw)).collect::<Result<Vec<_>, _>>()?;
    let ocsps = evidence.ocsps.iter().map(|raw| OcspResponse::parse(raw)).collect::<Result<Vec<_>, _>>()?;
    let signatures = crate::pdf::list_with_time(doc, bytes, trust, when);
    if !signatures.iter().any(|signature| signature.signed) {
        return Err(SignError::Pdf("The document has no signatures to attach validation evidence to.".into()));
    }
    let mut report = EvidenceReport { certificates: certificates.len(), crls: crls.len(), ocsps: ocsps.len(), ..Default::default() };
    let mut used_crls = vec![false; crls.len()];
    let mut used_ocsps = vec![false; ocsps.len()];
    let mut checks = Checks {
        certificates: &certificates,
        crls: &crls,
        ocsps: &ocsps,
        trust,
        report: &mut report,
        used_crls: &mut used_crls,
        used_ocsps: &mut used_ocsps,
    };
    for signature in signatures.iter().filter(|signature| signature.signed) {
        let role = if signature.doc_timestamp { CoverageRole::DocumentTimestamp } else { CoverageRole::Signer };
        let Some(signer) = &signature.certificate else {
            checks.report.notes.push(format!("{}: no readable signer certificate.", signature.field));
            continue;
        };
        checks.chain(signature, role, signer, &signature.chain);
        // The authority behind an embedded signature timestamp is a separate chain whose
        // revocation status matters too; leaving it out would overstate coverage.
        if let Some(authority) = signature.timestamp_chain.first() {
            checks.chain(signature, CoverageRole::SignatureTimestamp, authority, &signature.timestamp_chain);
        } else if signature.timestamp && !signature.doc_timestamp {
            checks
                .report
                .notes
                .push(format!("{}: the embedded timestamp token could not be verified, so its authority chain is not covered.", signature.field));
        }
    }
    if used_crls.iter().chain(&used_ocsps).any(|used| !used) {
        return Err(SignError::Pdf("A supplied CRL or OCSP response could not be verified for this document's certificate chains at the selected time; no evidence was embedded.".into()));
    }
    if !report.has_complete_coverage() {
        report.notes.push(
            "Revocation coverage is incomplete or includes a revocation. Embedding this data does not establish PAdES B-LT/B-LTA compliance.".into(),
        );
    }
    report.notes.push("Evidence certificates are not added to the trusted certificate list.".into());
    Ok(report)
}

struct Checks<'a> {
    certificates: &'a [Certificate],
    crls: &'a [CertificateList],
    ocsps: &'a [OcspResponse],
    trust: &'a TrustStore,
    report: &'a mut EvidenceReport,
    used_crls: &'a mut [bool],
    used_ocsps: &'a mut [bool],
}

impl Checks<'_> {
    /// Record coverage for every non-anchor certificate in `leaf`'s chain at the signature's
    /// selected verification time.
    fn chain(&mut self, signature: &SignatureInfo, role: CoverageRole, leaf: &Certificate, known: &[Certificate]) {
        let mut pool = known.to_vec();
        pool.extend_from_slice(self.certificates);
        let purpose = if signature.certify.is_some() && role == CoverageRole::Signer {
            crate::pdf::TrustPurpose::CertifiedDocuments
        } else {
            crate::pdf::TrustPurpose::Signatures
        };
        self.trust.extend_pool(&mut pool, purpose);
        let chain = crate::x509::build_chain(leaf, &pool, signature.verification_time);
        for (index, certificate) in chain.iter().enumerate() {
            // A configured trust anchor has no issuing authority whose revocation can be
            // checked here. Merely being self-signed does not make a certificate an anchor.
            if self.trust.anchor(certificate, purpose).is_some() {
                continue;
            }
            let mut status = RevocationStatus::Unknown;
            if let (Some(issuer), Some(at)) = (chain.get(index.saturating_add(1)), signature.verification_time) {
                for (index, crl) in self.crls.iter().enumerate() {
                    let found = crl.check_with(certificate, issuer, at, self.trust.revocation_max_age);
                    if found != RevocationStatus::Unknown {
                        if let Some(used) = self.used_crls.get_mut(index) {
                            *used = true;
                        }
                        if !matches!(status, RevocationStatus::Revoked { .. }) {
                            status = found;
                        }
                    }
                }
                for (index, ocsp) in self.ocsps.iter().enumerate() {
                    let found = ocsp.check_with(certificate, issuer, at, self.trust.revocation_max_age);
                    if found != RevocationStatus::Unknown {
                        if let Some(used) = self.used_ocsps.get_mut(index) {
                            *used = true;
                        }
                        if !matches!(status, RevocationStatus::Revoked { .. }) {
                            status = found;
                        }
                    }
                }
            }
            self.report.coverage.push(CertificateCoverage {
                field: signature.field.clone(),
                role,
                certificate_sha256: certificate.fingerprint(),
                checked_at: signature.verification_time,
                status,
            });
        }
    }
}

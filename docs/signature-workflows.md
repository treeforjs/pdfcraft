# Timestamp and validation evidence workflows

Use **Use a certificate → Timestamp servers** to configure an RFC 3161 authority. No
server is enabled by default and opening or validating a PDF never makes a network
request. Each timestamp operation requires explicit permission to contact the selected
server. A request contains a digest and nonce, not the PDF or a private key.

**Timestamp document** saves a new document timestamp. **Digitally sign → Sign as** can
also attach a timestamp to the signature. Native jobs run in the background; cancel
discards the result. Closing or editing the target document while a job runs prevents
the result from being saved. A signed result is saved incrementally and its earlier
signed bytes stay unchanged. Signing clears the earlier undo history.

Servers use HTTPS with system TLS roots and the platform client's proxy environment
settings. Redirects and credentials in URLs are refused. Literal loopback IPs may use
HTTP for a local authority; these requests bypass the environment proxy. There is a
4 MiB response limit and a configured 1–30 second deadline. Cancellation is checked
before requests, during reads and before saving; a blocked read may take until its
deadline to finish. Web transport, authenticated TSA profiles and revocation fetching
are not provided in this workflow.

Replies must match the digest, nonce and requested policy. The TSA certificate needs
the critical, exclusive timeStamping extended key usage and must be valid when the
token was issued. The profile defaults to requiring a chain to an explicitly trusted
certificate. Add such certificates only after checking their identity and fingerprint
independently. Turning that requirement off permits saving an untrusted token; it
does not trust the authority or establish trusted time. TLS trust and PDF signature
trust are separate.

**Verification time** checks certificates and embedded revocation data at the best
available time, the claimed signing time, the current time or a complete UTC date
(`YYYYMMDDHHmmSSZ`). Results explicitly identify `trusted_timestamp`,
`claimed_signing_time` or `chosen_time`. A chosen or claimed date is not proof of
when signing occurred. This is a read-only report; the Signatures panel continues
to use its ordinary best-available-time validation.

**Embed validation evidence** accepts local DER certificate, CRL and OCSP files, one
path per line. Limits are 64 files, 2 MiB per file and 32 MiB total. Revocation data
must cryptographically match a document certificate chain and the selected validity
window. Invalid or unrelated evidence is refused before saving. Good evidence cannot
override a verified revocation. Certificates are never added to the trust list by
this operation. The dialog reports incomplete coverage and can optionally timestamp
the resulting DSS revision. A DSS and timestamp alone do not establish PAdES B-LT
or B-LTA conformance; fetching fresh evidence and complete archival renewal policy
remain separate work.

Coverage lists every non-anchor certificate of each signer chain, of the authority
chain behind each embedded signature timestamp, and of each document-timestamp
authority, with its role (`signer`, `signature_timestamp`, `document_timestamp`).
Complete coverage requires verified good evidence for all of them. A token whose
authority could not be verified is noted as not covered. Evidence is checked at the
selected verification time: a CRL or OCSP response must be current at that time
(`thisUpdate` ≤ time ≤ `nextUpdate`), so evidence produced after a timestamped
signature is accepted only when a verification time inside its window is selected.
A response without `nextUpdate` currently has no upper bound; a maximum-age policy
is not implemented. OCSP responses may be signed by the issuing CA or by a delegated
responder whose embedded certificate is issued by that CA, carries the OCSP-signing
extended key usage and is valid when the response was produced.

Automation exposes `sign_timestamp`, `sign_embed_evidence`, `sign_verify` and optional
`timestamp_server` on `sign_document`. Supplying a server requires
`allow_network: true`. A server object has `name`, `url`, optional `policy`,
`timeout_seconds`, and `require_trusted_authority`. All file paths obey the automation
root. Validation returns the selected time and its provenance; evidence import
returns per-certificate coverage and notes. `pdfcraft-cli tools` supplies the schemas.

The UI control channel exposes `timestamp_servers` and `signature_workflow` in
`ui.state`. `ui.command` accepts `sign.timestamp_servers`, `sign.timestamp`,
`sign.embed_evidence`, and `sign.verification_options`. `ui.set` keys are
`timestamp-servers` (JSON array), `timestamp-server` (zero-based index or `none`),
`timestamp-network` (`true`/`false`), `signature-certificates`, `signature-crls`,
`signature-ocsps` (newline-separated paths), `signature-verification-time`,
`signature-properties` (a signature field's full name: opens Signature Properties) and
`signature-properties-tab` (`summary`, `document`, `signer`, `date-time` or `legal`).
Opening a workflow resets network permission. Buttons remain available through
AccessKit for Save, Verify and Cancel.

Regression sources use original synthetic certificates, CRLs, OCSP responses and
local loopback authorities. OpenSSL independently verifies the generated RFC 3161,
CRL and OCSP outputs. No test submits a document or digest to a public service.

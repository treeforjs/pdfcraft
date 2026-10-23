# Timestamp and validation evidence workflows

Use **Preferences → Signatures → Timestamp servers** (also **Use a certificate →
Timestamp servers**) to configure RFC 3161 authorities: edit, remove or add a server
with its name, HTTPS URL, optional policy OID, timeout, trusted-authority requirement
and optional account name. An account's password is held only until PdfCraft quits
and is sent as HTTP Basic credentials; a 401/403 reports the account refused. No
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
deadline to finish. There is no web transport (the browser build refuses requests).

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
when signing occurred. The Verify mode is a read-only report. **Preferences →
Signatures → Verify signatures using** sets the time the Signatures panel and the
banner validate at (secure time else signing time, signing time, or current time).

**Embed validation evidence** accepts local DER certificate, CRL and OCSP files, one
path per line. Limits are 64 files, 2 MiB per file and 32 MiB total. Revocation data
must cryptographically match a document certificate chain and the selected validity
window. Invalid or unrelated evidence is refused before saving. Good evidence cannot
override a verified revocation. Certificates are never added to the trust list by
this operation. The dialog reports incomplete coverage and can optionally timestamp
the resulting DSS revision. A DSS and timestamp alone do not establish PAdES B-LT
or B-LTA conformance; no conformance validator has been run and there is no archival
renewal policy.

**Add verification information** (Signatures panel, `sign.ltv_enable`) LTV-enables
every signature: it gathers the signer and timestamp chains and, when **Preferences →
Signatures → Fetch revocation information online** is on and the dialog's consent is
given, fetches OCSP responses (from the certificate's Authority Information Access,
SHA-1 CertID, no nonce) or CRLs (from its CRL distribution points). Every address is
checked against the internet-access policy: allowed sites are contacted, sites it asks
about only when "Also from sites that Internet access asks about" is ticked, blocked
sites never. Requests run on the signature job's thread with no redirects, a 1–30 s
timeout each and a 60 s deadline for the run, at most 2 OCSP and 2 CRL addresses per
certificate, 32 requests, 2 MiB per response and 24 MiB per run. Each response is
treated as hostile and kept only if it verifies for its certificate at the current
time and at the signature's verification time. Optional local DER files are merged,
then the evidence is embedded as above, optionally followed by a document timestamp.

Coverage lists every non-anchor certificate of each signer chain, of the authority
chain behind each embedded signature timestamp, and of each document-timestamp
authority, with its role (`signer`, `signature_timestamp`, `document_timestamp`).
Complete coverage requires verified good evidence for all of them. A token whose
authority could not be verified is noted as not covered. Evidence is checked at the
selected verification time: a CRL or OCSP response must be current at that time
(`thisUpdate` ≤ time ≤ `nextUpdate`), so evidence produced after a timestamped
signature is also accepted: a revocation is permanent, so evidence issued later
vouches for an earlier time while the certificate was still within its validity
period when the evidence was issued. A response without `nextUpdate` counts for
**Preferences → Signatures → Maximum age** hours after it was issued (0: no limit). OCSP responses may be signed by the issuing CA or by a delegated
responder whose embedded certificate is issued by that CA, carries the OCSP-signing
extended key usage and is valid when the response was produced.

Automation exposes `sign_timestamp`, `sign_embed_evidence`, `sign_add_verification`
(`allow_network`, `allow_prompted`, `timeout_seconds`, optional local files and
`timestamp_server`), `sign_verify`, `sign_timestamp_servers_get`/`set` (with
`password_for` and `password`) and optional `timestamp_server` on `sign_document`.
Supplying a server requires `allow_network: true`. A server is a configured server's
name or an object with `name`, `url`, optional `policy`, `timeout_seconds`,
`require_trusted_authority` and optional `username`. All file paths obey the automation
root. Validation returns the selected time and its provenance; evidence import
returns per-certificate coverage and notes. `pdfcraft-cli tools` supplies the schemas.

The UI control channel exposes `timestamp_servers` and `signature_workflow` in
`ui.state`, and `signing` (the Sign As dialog's lock and graphic, the saved
appearances and the last signed-version comparison). `ui.command` accepts
`sign.timestamp_servers`, `sign.trusted_certificates`, `sign.appearances`,
`sign.timestamp`, `sign.embed_evidence`, `sign.ltv_enable`,
`sign.compare_signed_version` and `sign.verification_options`. `ui.set` keys are
`timestamp-servers` (JSON array), `timestamp-server` (zero-based index or `none`),
`timestamp-network` (`true`/`false`), `signature-certificates`, `signature-crls`,
`signature-ocsps` (newline-separated paths), `signature-verification-time`,
`signature-properties` (a signature field's full name: opens Signature Properties) and
`signature-properties-tab` (`summary`, `document`, `signer`, `date-time` or `legal`),
`signature-fetch-online` and `signature-asked-sites` (`true`/`false`), and for the open
Sign As dialog `sign-lock-document` (`true`/`false`), `sign-graphic` (`name`, `drawn`,
`picture`) and `sign-appearance` (a saved appearance's name).
Opening a workflow resets network permission. Buttons remain available through
AccessKit for Save, Verify and Cancel.

Regression sources use original synthetic certificates, CRLs, OCSP responses and
local loopback authorities. OpenSSL independently verifies the generated RFC 3161,
CRL and OCSP outputs. No test submits a document or digest to a public service.

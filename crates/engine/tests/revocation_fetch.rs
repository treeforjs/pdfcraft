//! Add verification information (LTV) with revocation fetched from a local mock responder:
//! the internet-access policy, hostile responses, deadlines and caps. Never contacts the network
//! beyond 127.0.0.1.
#![cfg(not(target_arch = "wasm32"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use pdfcraft_engine::revocation_fetch::{FetchPolicy, HttpTransport, RevocationTransport};
use pdfcraft_engine::signature_workflow::SignatureCancellation;
use pdfcraft_engine::url_access::{UrlMode, UrlPolicy};
use pdfcraft_engine::{DocId, Session, SignOptions, sign};
use sign::{DigitalId, Time};

#[path = "../../sign/tests/support/mod.rs"]
mod support;

fn at() -> Time {
    Time { year: 2026, month: 10, day: 12, hour: 12, minute: 0, second: 0 }
}

/// One request the mock saw: (method, path, body).
type Seen = Arc<Mutex<Vec<(String, String, Vec<u8>)>>>;

/// A local HTTP server answering every connection with `reply(method, path, body)`.
fn mock(reply: impl Fn(&str, &str, &[u8]) -> Vec<u8> + Send + Sync + 'static) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let log = seen.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                let mut b = [0];
                if stream.read_exact(&mut b).is_err() {
                    break;
                }
                head.push(b[0]);
                assert!(head.len() < 16384);
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            let mut first = head.lines().next().unwrap_or_default().split(' ');
            let (method, path) = (first.next().unwrap_or_default().to_string(), first.next().unwrap_or_default().to_string());
            let length: usize = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse().unwrap())
                })
                .unwrap_or(0);
            assert!(length < 4096, "only an OCSP request is ever sent");
            let mut body = vec![0; length];
            let _ = stream.read_exact(&mut body);
            log.lock().unwrap().push((method.clone(), path.clone(), body.clone()));
            let _ = stream.write_all(&reply(&method, &path, &body));
        }
    });
    (base, seen)
}

fn ok(body: &[u8], mime: &str) -> Vec<u8> {
    let mut r = format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    r.extend_from_slice(body);
    r
}

fn allow_all() -> UrlPolicy {
    UrlPolicy { mode: UrlMode::AllowAll, ..Default::default() }
}

/// A session with a document signed by `leaf` (its issuer trusted).
fn signed(issuer: &DigitalId, leaf: &DigitalId) -> (Session, DocId, Arc<Vec<u8>>) {
    let mut session = Session::new().with_clock(|| at().unix());
    session.set_trusted_certificates(vec![issuer.certificate.clone()]);
    let doc = session.open("synthetic.pdf", None, Arc::new(support::pdf()), None).unwrap();
    let bytes = session.sign(doc, leaf, SignOptions::default()).unwrap();
    session.mark_signed(doc, bytes.clone(), None).unwrap();
    (session, doc, bytes)
}

fn transport() -> HttpTransport {
    HttpTransport { cancellation: SignatureCancellation::default() }
}

#[test]
fn fetched_ocsp_makes_the_signature_long_term_verifiable() {
    let (issuer0, leaf0) = support::chain();
    let good = support::ocsp(&issuer0, &leaf0.certificate);
    let (base, seen) = mock(move |method, path, _| {
        assert_eq!((method, path), ("POST", "/ocsp"));
        ok(&good, "application/ocsp-response")
    });
    let (issuer, leaf) = support::chain_with_urls(&[&format!("{base}/ocsp")], &[]);
    assert_eq!(leaf.certificate.ocsp_urls, vec![format!("{base}/ocsp")]);
    let (mut session, doc, bytes) = signed(&issuer, &leaf);
    session.set_url_policy(&allow_all());
    let policy = session.revocation_fetch_policy(true, false);
    let job = session.signature_job(doc).unwrap();
    let (result, report) = job.add_verification_information(&sign::dss::Evidence::default(), &policy, &transport(), None).unwrap();
    assert_eq!((report.fetched.covered, report.fetched.needed), (1, 1), "{:?}", report.fetched.attempts);
    assert!(report.fetched.attempts.iter().any(|a| a.outcome == "good"));
    assert!(report.evidence.has_complete_coverage());
    // The request names the signer's certificate.
    let request = &seen.lock().unwrap()[0].2;
    assert!(request.windows(leaf.certificate.serial.len()).any(|w| w == leaf.certificate.serial.as_slice()));
    // An incremental update after the signed bytes, and the signature stays valid.
    assert!(result.bytes.starts_with(&bytes));
    assert!(Arc::ptr_eq(&session.get(doc).unwrap().bytes, &bytes), "the job leaves the document as it was");
    session.mark_signed(doc, result.bytes, None).unwrap();
    let sig = session.get(doc).unwrap().signatures.iter().find(|s| s.signed).cloned().unwrap();
    assert_eq!(sig.status, sign::Status::Valid, "{:?}", sig.details);
}

#[test]
fn a_crl_is_fetched_when_there_is_no_responder() {
    let (issuer0, _) = support::chain();
    let crl = support::crl(&issuer0, None);
    let (base, seen) = mock(move |method, _, _| {
        assert_eq!(method, "GET");
        ok(&crl, "application/pkix-crl")
    });
    let (issuer, leaf) = support::chain_with_urls(&[], &[&format!("{base}/ca.crl")]);
    let (mut session, doc, _) = signed(&issuer, &leaf);
    session.set_url_policy(&allow_all());
    let policy = session.revocation_fetch_policy(true, false);
    let (_, report) = session.signature_job(doc).unwrap().add_verification_information(&Default::default(), &policy, &transport(), None).unwrap();
    assert_eq!(report.fetched.covered, 1, "{:?}", report.fetched.attempts);
    assert_eq!(seen.lock().unwrap()[0].1, "/ca.crl");
}

#[test]
fn the_access_policy_and_the_setting_decide_whether_anything_is_sent() {
    let (base, seen) = mock(|_, _, _| ok(b"never used", "application/ocsp-response"));
    let (issuer, leaf) = support::chain_with_urls(&[&format!("{base}/ocsp")], &[]);
    let (mut session, doc, _) = signed(&issuer, &leaf);
    let local = || {
        let (i, l) = support::chain();
        sign::dss::Evidence { certs: vec![], crls: vec![support::crl(&i, None)], ocsps: vec![support::ocsp(&i, &l.certificate)] }
    };
    // Default policy asks: nothing is sent without the user's agreement.
    let policy = session.revocation_fetch_policy(true, false);
    let (_, report) = session.signature_job(doc).unwrap().add_verification_information(&local(), &policy, &transport(), None).unwrap();
    assert!(report.fetched.attempts.iter().all(|a| a.outcome == "asks"), "{:?}", report.fetched.attempts);
    // Blocked sites stay blocked, even when asked sites are allowed.
    session.set_url_policy(&UrlPolicy { mode: UrlMode::BlockAll, ..Default::default() });
    let policy = session.revocation_fetch_policy(true, true);
    let (_, report) = session.signature_job(doc).unwrap().add_verification_information(&local(), &policy, &transport(), None).unwrap();
    assert!(report.fetched.attempts.iter().all(|a| a.outcome == "blocked"), "{:?}", report.fetched.attempts);
    // The Preferences switch turns fetching off altogether.
    session.set_url_policy(&allow_all());
    session.set_fetch_revocation(false);
    let policy = session.revocation_fetch_policy(true, true);
    assert!(!policy.network);
    let (_, report) = session.signature_job(doc).unwrap().add_verification_information(&local(), &policy, &transport(), None).unwrap();
    assert!(report.fetched.attempts.iter().all(|a| a.outcome != "good"));
    // Local evidence alone still covers the signature.
    assert!(report.evidence.has_complete_coverage());
    assert!(seen.lock().unwrap().is_empty(), "no request reached the server");
}

#[test]
fn hostile_responses_are_refused_and_the_document_is_untouched() {
    let calls = Arc::new(AtomicUsize::new(0));
    let n = calls.clone();
    let (base, _) = mock(move |_, path, _| {
        n.fetch_add(1, Ordering::SeqCst);
        match path {
            "/garbage" => ok(&[0x30, 0x84, 0xff, 0xff, 0xff, 0xff, 0x01], "application/ocsp-response"),
            "/huge" => ok(&vec![0x30; 3 * 1024 * 1024], "application/ocsp-response"),
            "/redirect" => b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            "/error" => b"HTTP/1.1 500 Oops\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            "/slow" => {
                thread::sleep(Duration::from_millis(2500));
                ok(b"late", "application/ocsp-response")
            }
            _ => ok(b"", "text/plain"),
        }
    });
    let urls: Vec<String> = ["garbage", "huge"].iter().map(|p| format!("{base}/{p}")).collect();
    let crls: Vec<String> = ["redirect", "error"].iter().map(|p| format!("{base}/{p}")).collect();
    let (issuer, leaf) = support::chain_with_urls(&[&urls[0], &urls[1]], &[&crls[0], &crls[1]]);
    let (mut session, doc, bytes) = signed(&issuer, &leaf);
    session.set_url_policy(&allow_all());
    let policy = session.revocation_fetch_policy(true, false);
    // Nothing usable: the run fails (there is no evidence for the signature) and the document
    // is as it was.
    let outcome = session.signature_job(doc).unwrap().add_verification_information(&Default::default(), &policy, &transport(), None);
    match outcome {
        Err(_) => {}
        Ok((_, report)) => {
            assert_eq!(report.fetched.covered, 0);
            assert!(report.fetched.attempts.iter().all(|a| a.outcome == "unusable" || a.outcome == "failed"), "{:?}", report.fetched.attempts);
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4, "two responders and two distribution points, each once");
    assert!(Arc::ptr_eq(&session.get(doc).unwrap().bytes, &bytes));
    // A responder slower than the timeout is abandoned at the deadline.
    let (issuer, leaf) = support::chain_with_urls(&[&format!("{base}/slow")], &[]);
    let (mut session, doc, _) = signed(&issuer, &leaf);
    session.set_url_policy(&allow_all());
    let policy = FetchPolicy { timeout_seconds: 1, ..session.revocation_fetch_policy(true, false) };
    let start = Instant::now();
    let _ = session.signature_job(doc).unwrap().add_verification_information(&Default::default(), &policy, &transport(), None);
    assert!(start.elapsed() < Duration::from_millis(2400), "{:?}", start.elapsed());
}

/// A transport that only counts and answers garbage, for the request caps.
struct Counting(AtomicUsize);

impl RevocationTransport for Counting {
    fn ocsp(&self, _: &str, _: &[u8], _: Duration) -> Result<Vec<u8>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vec![0; 16])
    }
    fn crl(&self, _: &str, _: Duration) -> Result<Vec<u8>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("refused".into())
    }
}

#[test]
fn hostile_certificates_with_many_addresses_are_capped() {
    let ocsp: Vec<String> = (0..50).map(|i| format!("http://127.0.0.1:9/{i}")).collect();
    let crl: Vec<String> = (0..50).map(|i| format!("http://127.0.0.1:9/c{i}")).collect();
    let o: Vec<&str> = ocsp.iter().map(String::as_str).collect();
    let c: Vec<&str> = crl.iter().map(String::as_str).collect();
    let (issuer, leaf) = support::chain_with_urls(&o, &c);
    let (mut session, doc, _) = signed(&issuer, &leaf);
    session.set_url_policy(&allow_all());
    let policy = session.revocation_fetch_policy(true, false);
    let counting = Counting(AtomicUsize::new(0));
    let _ = session.signature_job(doc).unwrap().add_verification_information(&Default::default(), &policy, &counting, None);
    // At most two responders and two distribution points per certificate.
    assert!(counting.0.load(Ordering::SeqCst) <= 4, "{}", counting.0.load(Ordering::SeqCst));
}

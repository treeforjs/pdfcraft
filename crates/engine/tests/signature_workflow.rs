//! Synthetic local RFC 3161 workflow: never contacts a public authority.
#![cfg(not(target_arch = "wasm32"))]

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    thread,
};

use pdfcraft_engine::signature_workflow::{HttpTimestampAuthority, SignatureCancellation, TimestampServer, verification_time_utc};
use pdfcraft_engine::{Session, SignOptions, sign};
use sign::{DigestAlg, Time, TimestampAuthority};

#[path = "../../sign/tests/support/mod.rs"]
mod support;

fn at() -> Time {
    Time { year: 2026, month: 10, day: 12, hour: 12, minute: 0, second: 0 }
}

fn server(url: String) -> TimestampServer {
    TimestampServer {
        name: "Synthetic local TSA".into(),
        url,
        policy: Some("1.2.3.4".into()),
        timeout_seconds: 2,
        require_trusted_authority: true,
        username: None,
    }
}

fn local_server(reply: impl FnOnce(Vec<u8>) -> Vec<u8> + Send + 'static) -> (String, thread::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/timestamp", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut byte = [0u8; 1];
            socket.read_exact(&mut byte).unwrap();
            bytes.push(byte[0]);
            assert!(bytes.len() < 16384);
            if bytes.ends_with(b"\r\n\r\n") {
                break bytes.len();
            }
        };
        let headers = String::from_utf8(bytes.clone()).unwrap();
        assert!(headers.starts_with("POST /timestamp HTTP/1.1\r\n"));
        assert!(headers.to_ascii_lowercase().contains("content-type: application/timestamp-query"));
        let length: usize = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("Content-Length").then(|| value.trim().parse().unwrap())
            })
            .unwrap();
        assert!(length < 4096, "only a digest request, never document bytes");
        bytes.resize(header_end + length, 0);
        socket.read_exact(&mut bytes[header_end..]).unwrap();
        let request = bytes[header_end..].to_vec();
        let response = reply(request.clone());
        socket.write_all(&response).unwrap();
        request
    });
    (url, worker)
}

fn response(body: &[u8], status: &str, mime: &str) -> Vec<u8> {
    let mut response =
        format!("HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    response.extend_from_slice(body);
    response
}

#[test]
fn timestamp_local_server_saves_reopens_and_reports_trusted_time() {
    let tsa = support::timestamp_id();
    let trusted = tsa.certificate.clone();
    let (url, worker) = local_server(move |request| {
        let query = sign::timestamp::parse_request(&request).unwrap();
        assert!(query.nonce.is_some());
        assert_eq!(query.policy.as_deref(), Some("1.2.3.4"));
        let reply = sign::timestamp::respond(&tsa.key, &tsa.certificate, &[], DigestAlg::Sha256, &query, "1.2.3.4", at(), 7).unwrap();
        response(&reply, "200 OK", "application/timestamp-reply")
    });
    let transport = HttpTimestampAuthority::new(server(url), vec![trusted.clone()], SignatureCancellation::default()).unwrap();
    let mut session = Session::new().with_clock(|| 1_792_000_000);
    let bytes = session.create_from_text("Synthetic", "Original local timestamp fixture").unwrap();
    let id = session.open("fixture.pdf", None, bytes, None).unwrap();
    let before = session.get(id).unwrap().bytes.clone();
    let result = session.signature_job(id).unwrap().timestamp(&transport).unwrap();
    session.check_signature_result(&result).unwrap();
    assert!(result.bytes.starts_with(&before), "timestamp must append without changing signed bytes");
    assert!(Arc::ptr_eq(&session.get(id).unwrap().bytes, &before), "preparation cannot mutate the live document");
    session.set_trusted_certificates(vec![trusted]);
    session.mark_signed(id, result.bytes.clone(), None).unwrap();
    let reopened = session.open("reopened.pdf", None, result.bytes, None).unwrap();
    let signature = session.get(reopened).unwrap().signatures.iter().find(|signature| signature.doc_timestamp).unwrap();
    assert_eq!(signature.status, sign::Status::Valid, "{:?}", signature.details);
    assert_eq!(signature.timestamp_time, Some(at()));
    assert!(!worker.join().unwrap().windows(8).any(|window| window == b"Original"));
}

#[test]
fn transport_refuses_untrusted_wrong_mime_and_redirect_responses() {
    for case in ["untrusted", "mime", "redirect"] {
        let tsa = support::timestamp_id();
        let (url, worker) = local_server(move |request| {
            let query = sign::timestamp::parse_request(&request).unwrap();
            let reply = sign::timestamp::respond(&tsa.key, &tsa.certificate, &[], DigestAlg::Sha256, &query, "1.2.3.4", at(), 7).unwrap();
            match case {
                "mime" => response(&reply, "200 OK", "text/html"),
                "redirect" => b"HTTP/1.1 302 Found\r\nLocation: https://example.invalid/never-contact\r\nContent-Length: 0\r\n\r\n".to_vec(),
                _ => response(&reply, "200 OK", "application/timestamp-reply"),
            }
        });
        let transport = HttpTimestampAuthority::new(server(url), vec![], SignatureCancellation::default()).unwrap();
        let query = sign::TimestampQuery::online(DigestAlg::Sha256, DigestAlg::Sha256.digest(&[b"synthetic"])).unwrap();
        let error = transport.timestamp(&query.encode().unwrap()).unwrap_err().to_string();
        let expected = match case {
            "untrusted" => "not trusted",
            "mime" => "Content-Type",
            _ => "302",
        };
        assert!(error.contains(expected), "{case}: {error}");
        worker.join().unwrap();
    }
}

#[test]
fn cancellation_and_stale_result_do_not_change_the_document() {
    let cancel = SignatureCancellation::default();
    cancel.cancel();
    let transport = HttpTimestampAuthority::new(server("http://127.0.0.1:1/unused".into()), vec![], cancel).unwrap();
    let query = sign::TimestampQuery::online(DigestAlg::Sha256, DigestAlg::Sha256.digest(&[b"synthetic"])).unwrap();
    assert!(transport.timestamp(&query.encode().unwrap()).unwrap_err().to_string().contains("cancelled"));
    let mut session = Session::new().with_clock(|| 1_792_000_000);
    let bytes = session.create_from_text("Synthetic", "Original").unwrap();
    let doc = session.open("fixture.pdf", None, bytes, None).unwrap();
    let result = session.signature_job(doc).unwrap().sign(&support::timestamp_id(), SignOptions::default(), None).unwrap();
    session.check_signature_result(&result).unwrap();
    session.apply(doc, pdfcraft_engine::Edit::SetInfo { key: "Title".into(), value: "Edited while request ran".into() }).unwrap();
    assert!(session.check_signature_result(&result).is_err());
    session.close(doc);
    assert!(session.check_signature_result(&result).is_err());
}

#[test]
fn supplied_evidence_and_timestamp_are_one_detached_save_result() {
    let (issuer, leaf) = support::chain();
    let mut session = Session::new().with_clock(|| at().unix());
    session.set_trusted_certificates(vec![issuer.certificate.clone()]);
    let doc = session.open("synthetic.pdf", None, Arc::new(support::pdf()), None).unwrap();
    let bytes = session.sign(doc, &leaf, SignOptions::default()).unwrap();
    session.mark_signed(doc, bytes.clone(), None).unwrap();
    let evidence = sign::dss::Evidence {
        certs: vec![issuer.certificate.raw.clone()],
        crls: vec![support::crl(&issuer, None)],
        ocsps: vec![support::ocsp(&issuer, &leaf.certificate)],
    };
    let (result, report) = session.signature_job(doc).unwrap().embed_evidence(&evidence, sign::VerificationTime::BestAvailable, None).unwrap();
    assert!(report.has_complete_coverage());
    assert!(result.bytes.starts_with(&bytes));
    assert!(Arc::ptr_eq(&session.get(doc).unwrap().bytes, &bytes));
    session.mark_signed(doc, result.bytes, None).unwrap();
    assert!(session.get(doc).unwrap().signatures.iter().filter(|s| s.signed).all(|s| s.status == sign::Status::Valid));
}

#[test]
fn transport_bounds_response_size_deadline_and_in_flight_cancellation() {
    let query = sign::TimestampQuery::online(DigestAlg::Sha256, DigestAlg::Sha256.digest(&[b"synthetic"])).unwrap().encode().unwrap();
    for case in ["size", "deadline", "cancel"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/timestamp", listener.local_addr().unwrap());
        let cancellation = SignatureCancellation::default();
        let cancel = cancellation.clone();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut b = [0];
                stream.read_exact(&mut b).unwrap();
                header.push(b[0]);
                assert!(header.len() < 16384);
            }
            let header = String::from_utf8(header).unwrap();
            let length: usize = header
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            assert!(length < 4096);
            let mut request = vec![0; length];
            stream.read_exact(&mut request).unwrap();
            match case {
                "size" => {
                    let _ = stream.write_all(&response(&vec![0; 4 * 1024 * 1024 + 1], "200 OK", "application/timestamp-reply"));
                }
                "cancel" => {
                    cancel.cancel();
                    let _ = stream.write_all(&response(b"cancelled", "200 OK", "application/timestamp-reply"));
                }
                _ => {
                    std::thread::sleep(std::time::Duration::from_millis(1200));
                }
            }
        });
        let mut settings = server(url);
        settings.timeout_seconds = 1;
        let transport = HttpTimestampAuthority::new(settings, vec![], cancellation).unwrap();
        let start = std::time::Instant::now();
        let error = transport.timestamp(&query).unwrap_err().to_string();
        if case == "size" {
            assert!(error.contains("4 MiB"), "{error}");
        }
        if case == "cancel" {
            assert!(error.contains("cancelled"), "{error}");
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(3));
        worker.join().unwrap();
    }
}

#[test]
fn server_and_verification_dates_are_strictly_validated() {
    for url in [
        "http://example.invalid/tsa",
        "http://localhost:8000/tsa",
        "http://10.0.0.1/tsa",
        "https://user:password@example.invalid/tsa",
        "https://user@example.invalid/tsa",
        "https://example.invalid/tsa#secret",
        "file:///tmp/response",
    ] {
        assert!(server(url.into()).validate().is_err(), "{url}");
    }
    for url in ["https://example.invalid/tsa", "http://127.0.0.1:8000/tsa", "http://[::1]:8000/tsa"] {
        assert!(server(url.into()).validate().is_ok(), "{url}");
    }
    for bad in ["2026", "20260230000000Z", "20261012120060Z", "00001012120000Z", "20261301000000Z", "20261012120000+02'00'"] {
        assert!(verification_time_utc(bad).is_err(), "{bad}");
    }
    assert_eq!(verification_time_utc("20261012120000Z").unwrap(), at());
}

#[test]
fn a_server_account_sends_basic_credentials_and_needs_its_password() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/timestamp", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut b = [0];
            stream.read_exact(&mut b).unwrap();
            header.push(b[0]);
            assert!(header.len() < 16384);
        }
        let header = String::from_utf8(header).unwrap();
        let length: usize = header
            .lines()
            .find_map(|line| line.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")).map(|(_, v)| v.trim().parse().unwrap()))
            .unwrap();
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        header
    });
    let mut settings = server(url);
    settings.username = Some("ada".into());
    // An account without its password is refused before any request.
    assert!(HttpTimestampAuthority::with_trust(settings.clone(), sign::TrustStore::default(), None, SignatureCancellation::default()).is_err());
    let tsa = HttpTimestampAuthority::with_trust(settings, sign::TrustStore::default(), Some("pw".into()), SignatureCancellation::default()).unwrap();
    let query = sign::TimestampQuery::online(DigestAlg::Sha256, DigestAlg::Sha256.digest(&[b"synthetic"])).unwrap().encode().unwrap();
    let error = tsa.timestamp(&query).unwrap_err().to_string();
    assert!(error.contains("account name or password"), "{error}");
    let header = worker.join().unwrap();
    // base64("ada:pw")
    assert!(header.to_ascii_lowercase().contains("authorization: basic ywrhonb3"), "{header}");
    // Hostile account names are refused.
    for name in ["", "a:b", "line\nbreak", &"x".repeat(201)] {
        let mut s = server("https://tsa.example.com/".into());
        s.username = Some(name.into());
        assert!(s.validate().is_err(), "{name:?}");
    }
}

#[test]
fn signature_preferences_drive_validation_and_trust() {
    let (issuer, leaf) = support::chain();
    let mut session = Session::new().with_clock(|| at().unix());
    session.set_trusted_certificates(vec![issuer.certificate.clone()]);
    let doc = session.open("synthetic.pdf", None, Arc::new(support::pdf()), None).unwrap();
    let bytes = session.sign(doc, &leaf, SignOptions::default()).unwrap();
    session.mark_signed(doc, bytes, None).unwrap();
    let source = |s: &Session| s.get(doc).unwrap().signatures.iter().find(|x| x.signed).unwrap().verification_time_source;
    assert_eq!(source(&session), sign::VerificationTimeSource::ClaimedSigningTime);
    session.set_pref("signatures.verification_time", &serde_json::json!("current")).unwrap();
    assert_eq!(source(&session), sign::VerificationTimeSource::ChosenTime);
    assert!(session.set_pref("signatures.verification_time", &serde_json::json!("yesterday")).is_err());
    // Hostile numbers are clamped into 0..=87600 hours (ten years).
    assert!(session.set_pref("signatures.revocation_max_age_hours", &serde_json::json!(-5)).unwrap().clamped);
    assert_eq!(session.trust_store().revocation_max_age, None, "0 is no limit");
    assert!(session.set_pref("signatures.revocation_max_age_hours", &serde_json::json!(1e300)).unwrap().clamped);
    assert_eq!(session.trust_store().revocation_max_age, Some(87_600 * 3600));
    assert!(session.set_pref("signatures.revocation_max_age_hours", &serde_json::json!("lots")).is_err());
    session.set_pref("signatures.revocation_max_age_hours", &serde_json::json!(24)).unwrap();
    assert_eq!(session.trust_store().revocation_max_age, Some(86_400));
    // The operating-system trust settings are preferences too; certified needs signatures on.
    session.set_pref("signatures.os_trust_signatures", &serde_json::json!(true)).unwrap();
    session.set_pref("signatures.os_trust_certified", &serde_json::json!(true)).unwrap();
    assert!(session.os_trust_settings().signatures && session.os_trust_settings().certified_documents);
    assert_eq!(session.pref("signatures.os_trust_signatures"), Some(serde_json::json!(true)));
    assert!(session.set_pref("signatures.os_trust_signatures", &serde_json::json!({"on": 1})).is_err());
}

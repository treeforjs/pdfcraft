//! Preferences → Signatures through the tool table: saved appearances with a picture, a drawn
//! signature and a logo; per-certificate trust; the timestamp servers with an account; adding
//! verification information from local files and from a local mock responder; hostile input.

use std::io::{Read, Write};
use std::net::TcpListener;

use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

#[path = "../../sign/tests/support/mod.rs"]
mod support;

fn call(a: &mut Automation, name: &str, args: Value) -> Value {
    match a.call(name, &args) {
        Ok(mut c) => match c.remove(0) {
            Content::Json(value) => value,
            other => panic!("unexpected {other:?}"),
        },
        Err(e) => panic!("{name} {args}: {e}"),
    }
}

fn setup(tag: &str) -> (Automation, u64, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-sign-settings-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (issuer, leaf) = support::chain();
    std::fs::write(dir.join("source.pdf"), support::pdf()).unwrap();
    std::fs::write(dir.join("id.p12"), pdfcraft_sign::pkcs12::write(&leaf, "test").unwrap()).unwrap();
    std::fs::write(dir.join("root.der"), &issuer.certificate.raw).unwrap();
    std::fs::write(dir.join("good.crl"), support::crl(&issuer, None)).unwrap();
    std::fs::write(dir.join("good.ocsp"), support::ocsp(&issuer, &leaf.certificate)).unwrap();
    let mut img = image::RgbaImage::new(40, 20);
    for (x, y, p) in img.enumerate_pixels_mut() {
        *p = if (x + y) % 3 == 0 { image::Rgba([10, 20, 80, 255]) } else { image::Rgba([255, 255, 255, 255]) };
    }
    img.save(dir.join("signature.png")).unwrap();
    img.save(dir.join("logo.png")).unwrap();
    std::fs::write(dir.join("not-an-image.png"), b"\x89PNG\r\n\x1a\nnot really").unwrap();
    let mut a = Automation::new().with_root(&dir).unwrap().with_clock(|| pdfcraft_sign::Time::from_pdf("20261012120000Z").unwrap().unix());
    let doc = call(&mut a, "doc_open", json!({"path":"source.pdf"}))["doc"].as_u64().unwrap();
    call(&mut a, "sign_trust", json!({"paths":["root.der"]}));
    (a, doc, dir)
}

#[test]
fn saved_appearances_sign_with_a_picture_a_drawing_and_a_logo() {
    let (mut a, doc, dir) = setup("appearance");
    let list = call(
        &mut a,
        "sign_appearance_save",
        json!({"name":"Picture","appearance":{"image":"signature.png","remove_white":true,"logo":"logo.png","reason":true}}),
    );
    assert_eq!(list["appearances"][0]["graphic"]["kind"], "image");
    assert_eq!(list["appearances"][0]["logo"]["width"], 40);
    call(&mut a, "sign_appearance_save", json!({"name":"Drawn","appearance":{"strokes":[[[0,0],[10,4],[20,0]],[[5,8]]]}}));
    assert_eq!(call(&mut a, "sign_appearance_list", json!({}))["appearances"].as_array().unwrap().len(), 2);
    for (i, look) in [json!("Picture"), json!("Drawn"), json!({"strokes":[[[0,0],[3,3]]],"labels":false})].into_iter().enumerate() {
        let out = format!("signed{i}.pdf");
        let r = call(
            &mut a,
            "sign_document",
            json!({"doc":doc,"id":"id.p12","password":"test","out":out,"page":1,"rect":[20,20,180,80],"appearance":look,"lock_document":true}),
        );
        assert_eq!(r["signature"]["status"], "valid", "{r}");
        let reopened = call(&mut a, "doc_open", json!({"path":out}))["doc"].as_u64().unwrap();
        let sig = &call(&mut a, "sign_list", json!({"doc":reopened}))["signatures"][0];
        assert_eq!(sig["status"], "valid", "{sig}");
    }
    // Hostile appearances are refused before anything is written.
    for bad in [
        json!({"name":"","appearance":{}}),
        json!({"name":"x","appearance":{"image":"not-an-image.png"}}),
        json!({"name":"x","appearance":{"image":"../outside.png"}}),
        json!({"name":"x","appearance":{"strokes":"zig-zag"}}),
        json!({"name":"x","appearance":{"strokes":[]}}),
        json!({"name":"x","appearance":{"strokes":[[[1e39,0]]]}}),
        json!({"name":"x","appearance":{"image":"signature.png","strokes":[[[0,0]]]}}),
        json!({"name":"x","appearance":{"saved":"No such"}}),
        json!({"name":"x","appearance":{"date":"yes"}}),
        json!({"name":"x","appearance":{"logo":7}}),
    ] {
        assert!(a.call("sign_appearance_save", &bad).is_err(), "{bad}");
    }
    let strokes: Vec<Value> = (0..300).map(|i| json!([[i, 0]])).collect();
    assert!(a.call("sign_appearance_save", &json!({"name":"many","appearance":{"strokes":strokes}})).is_err());
    assert!(a.call("sign_document", &json!({"doc":doc,"id":"id.p12","password":"test","out":"n.pdf","appearance":"No such"})).is_err());
    assert!(!dir.join("n.pdf").exists());
    call(&mut a, "sign_appearance_delete", json!({"name":"Drawn"}));
    assert!(a.call("sign_appearance_delete", &json!({"name":"Drawn"})).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn per_certificate_trust_through_tools() {
    let (mut a, doc, dir) = setup("trust");
    call(&mut a, "sign_document", json!({"doc":doc,"id":"id.p12","password":"test","out":"signed.pdf"}));
    let signed = call(&mut a, "doc_open", json!({"path":"signed.pdf"}))["doc"].as_u64().unwrap();
    let list = call(&mut a, "sign_trust_list", json!({}));
    let fp = list["trusted"][0]["sha256"].as_str().unwrap().to_string();
    assert_eq!(list["trusted"][0]["trust"], json!({"root":true,"signatures":true,"certified_documents":true}));
    assert_eq!(call(&mut a, "sign_list", json!({"doc":signed}))["signatures"][0]["status"], "valid");
    // Pasted with colons and lowercase.
    let pasted = fp.replace(' ', ":").to_lowercase();
    call(&mut a, "sign_trust_set", json!({"sha256":pasted,"root":false}));
    assert_eq!(call(&mut a, "sign_list", json!({"doc":signed}))["signatures"][0]["status"], "unknown");
    call(&mut a, "sign_trust_set", json!({"sha256":fp,"root":true,"signatures":false}));
    assert_eq!(call(&mut a, "sign_list", json!({"doc":signed}))["signatures"][0]["status"], "unknown");
    call(&mut a, "sign_trust_set", json!({"sha256":fp,"signatures":true}));
    assert_eq!(call(&mut a, "sign_list", json!({"doc":signed}))["signatures"][0]["status"], "valid");
    for bad in [json!({"sha256":"zz"}), json!({"sha256":"00".repeat(32)}), json!({"sha256":fp,"root":"no"}), json!({})] {
        assert!(a.call("sign_trust_set", &bad).is_err(), "{bad}");
    }
    call(&mut a, "sign_trust_set", json!({"sha256":fp,"remove":true}));
    assert!(call(&mut a, "sign_trust_list", json!({}))["trusted"].as_array().unwrap().is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn timestamp_servers_with_an_account_keep_the_password_in_the_session() {
    let (mut a, _, dir) = setup("servers");
    let servers =
        json!([{"name":"Company TSA","url":"https://tsa.example.com/","timeout_seconds":5,"require_trusted_authority":true,"username":"ada"}]);
    let r = call(&mut a, "sign_timestamp_servers_set", json!({"servers":servers,"password_for":"Company TSA","password":"s3cret"}));
    assert_eq!(r["servers"][0]["username"], "ada");
    assert_eq!(r["servers"][0]["password_held"], true);
    assert!(!r.to_string().contains("s3cret"), "the password is never returned");
    for bad in [
        json!({"servers":[{"name":"x","url":"ftp://x/","timeout_seconds":5,"require_trusted_authority":true}]}),
        json!({"servers":[{"name":"x","url":"https://x/","timeout_seconds":5,"require_trusted_authority":true,"username":"a:b"}]}),
        json!({"servers":"nope"}),
        json!({"password_for":"No such","password":"x"}),
        json!({"password_for":"Company TSA","password":"x".repeat(2000)}),
    ] {
        assert!(a.call("sign_timestamp_servers_set", &bad).is_err(), "{bad}");
    }
    let many: Vec<Value> = (0..40)
        .map(|i| json!({"name":format!("s{i}"),"url":"https://tsa.example.com/","timeout_seconds":5,"require_trusted_authority":true}))
        .collect();
    assert!(a.call("sign_timestamp_servers_set", &json!({"servers":many})).is_err());
    assert_eq!(call(&mut a, "sign_timestamp_servers_get", json!({}))["servers"].as_array().unwrap().len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn add_verification_from_local_files_and_a_local_responder() {
    let (mut a, doc, dir) = setup("ltv");
    call(&mut a, "sign_document", json!({"doc":doc,"id":"id.p12","password":"test","out":"signed.pdf"}));
    let signed = std::fs::read(dir.join("signed.pdf")).unwrap();
    // No network: the local files carry the evidence.
    let r = call(
        &mut a,
        "sign_add_verification",
        json!({"doc":doc,"out":"ltv.pdf","crls":["good.crl"],"ocsps":["good.ocsp"],"certificates":["root.der"]}),
    );
    assert_eq!(r["verification"]["complete_revocation_coverage"], true, "{r}");
    assert_eq!(r["verification"]["network_allowed"], false);
    assert!(std::fs::read(dir.join("ltv.pdf")).unwrap().starts_with(&signed));
    assert!(a.call("sign_add_verification", &json!({"doc":doc,"out":"x.pdf","crls":["../outside.crl"]})).is_err());
    let many: Vec<&str> = vec!["good.crl"; 40];
    assert!(a.call("sign_add_verification", &json!({"doc":doc,"out":"x.pdf","crls":many})).is_err());
    assert!(!dir.join("x.pdf").exists());

    // A signer whose certificate names a local OCSP responder.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/ocsp", listener.local_addr().unwrap());
    let (issuer, leaf) = support::chain_with_urls(&[&url], &[]);
    let response = support::ocsp(&issuer, &leaf.certificate);
    let worker = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut b = [0];
            s.read_exact(&mut b).unwrap();
            head.push(b[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let n: usize = head
            .lines()
            .find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")).map(|(_, v)| v.trim().parse().unwrap()))
            .unwrap();
        let mut body = vec![0; n];
        s.read_exact(&mut body).unwrap();
        write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/ocsp-response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len())
            .unwrap();
        s.write_all(&response).unwrap();
    });
    std::fs::write(dir.join("online.p12"), pdfcraft_sign::pkcs12::write(&leaf, "test").unwrap()).unwrap();
    call(&mut a, "sign_document", json!({"doc":doc,"id":"online.p12","password":"test","out":"online.pdf"}));
    let online = call(&mut a, "doc_open", json!({"path":"online.pdf"}))["doc"].as_u64().unwrap();
    // Internet access asks by default: nothing is sent until the caller agrees.
    let r = call(
        &mut a,
        "sign_add_verification",
        json!({"doc":online,"out":"asked.pdf","allow_network":true,"certificates":["root.der"],"crls":["good.crl"]}),
    );
    assert!(r["verification"]["requests"].as_array().unwrap().iter().any(|q| q["outcome"] == "asks"), "{r}");
    let r = call(
        &mut a,
        "sign_add_verification",
        json!({"doc":online,"out":"online-ltv.pdf","allow_network":true,"allow_prompted":true,"timeout_seconds":5}),
    );
    worker.join().unwrap();
    assert_eq!(r["verification"]["certificates_covered_online"], 1, "{r}");
    assert_eq!(r["verification"]["complete_revocation_coverage"], true, "{r}");
    std::fs::remove_dir_all(dir).unwrap();
}

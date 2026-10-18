//! Submit Form end to end (ISO 32000-2 §12.7.6.2): `form_submit_prepare` builds what each flag
//! combination sends without sending it, and `form_submit_send` sends it to a loopback HTTP
//! server this test starts (no outside network), only with confirm: true, with timeouts and a
//! capped answer; `mailto:` actions come back as a prepared email.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use base64::Engine as _;
use pdfcraft_automation::{Automation, Content, ToolError};
use serde_json::{Value, json};

fn ok(a: &mut Automation, tool: &str, args: Value) -> Value {
    match a.call(tool, &args) {
        Ok(mut c) => match c.remove(0) {
            Content::Json(v) => v,
            other => panic!("{tool}: expected JSON, got {other:?}"),
        },
        Err(e) => panic!("{tool} {args}: {e}"),
    }
}

fn invalid(a: &mut Automation, tool: &str, args: Value) -> String {
    match a.call(tool, &args) {
        Err(ToolError::InvalidArgs(m)) => m,
        other => panic!("{tool} {args}: expected invalid arguments, got {other:?}"),
    }
}

/// A classic-xref PDF from numbered object bodies (object i+1 = `objs[i]`).
fn pdf(objs: &[String]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

/// A form with text fields (one a date, one NoExport, one empty), two comments by different
/// authors, and one Submit Form button per flag combination, all aimed at `server`.
fn form(server: &str) -> Vec<u8> {
    let button = |n: usize, name: &str, extra: &str, action: String| {
        let y = 400 - 30 * n as i32;
        format!("<< /Type /Annot /Subtype /Widget /FT /Btn /Ff 65536 /T ({name}) {extra} /Rect [300 {y} 380 {}] /P 3 0 R /A {action} >>", y + 20)
    };
    let submit = |url: &str, flags: u32, rest: &str| format!("<< /S /SubmitForm /F ({url}) /Flags {flags} {rest} >>");
    let objs = vec![
        "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 600 800] >>".into(),
        "<< /Type /Page /Parent 2 0 R /Annots [5 0 R 6 0 R 7 0 R 8 0 R 9 0 R 10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 15 0 R 16 0 R 17 0 R 18 0 R] >>".into(),
        "<< /Fields [5 0 R 6 0 R 7 0 R 8 0 R 9 0 R 12 0 R 13 0 R 14 0 R 15 0 R 16 0 R 17 0 R 18 0 R] /DA (/Helv 0 Tf 0 g) >>".into(),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (Ada Lovelace) /Rect [20 700 200 720] /P 3 0 R >>".into(),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (town) /V (London & Co) /Rect [20 670 200 690] /P 3 0 R >>".into(),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (born) /V (12/10/1815) /Rect [20 640 200 660] /P 3 0 R /AA << /F << /S /JavaScript /JS (AFDate_FormatEx\\(\"mm/dd/yyyy\"\\);) >> /K << /S /JavaScript /JS (AFDate_KeystrokeEx\\(\"mm/dd/yyyy\"\\);) >> >> >>".into(),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (secret) /Ff 4 /V (hidden) /Rect [20 610 200 630] /P 3 0 R >>".into(),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (empty) /Rect [20 580 200 600] /P 3 0 R >>".into(),
        "<< /Type /Annot /Subtype /Text /Rect [400 700 420 720] /T (Ada) /NM (c1) /Contents (Mine) >>".into(),
        "<< /Type /Annot /Subtype /Text /Rect [400 650 420 670] /T (Bob) /NM (c2) /Contents (Not mine) >>".into(),
        // ExportFormat | SubmitCoordinates, with a mapping name.
        button(0, "html", "/TM (go)", submit(&format!("{server}/in"), 4 | 16, "")),
        // ExportFormat | GetMethod | CanonicalFormat.
        button(1, "get", "", submit(&format!("{server}/q?form=1"), 4 | 8 | 512, "")),
        // IncludeAppendSaves | IncludeAnnotations | ExclNonUserAnnots | ExclFKey (FDF).
        button(2, "fdf", "", submit(&format!("{server}/fdf"), 64 | 128 | 1024 | 2048, "")),
        // XFDF | Exclude | IncludeNoValueFields, excluding town.
        button(3, "xfdf", "", submit(&format!("{server}/xfdf"), 32 | 1 | 2, "/Fields [(town)]")),
        // SubmitPDF (everything else ignored).
        button(4, "pdf", "", submit(&format!("{server}/pdf"), 256 | 4 | 32, "/Fields [(name)]")),
        // EmbedForm (FDF).
        button(5, "embed", "", submit(&format!("{server}/embed"), 8192, "")),
        // A mailto: address, as an HTML form.
        button(6, "mail", "", submit("mailto:forms@example.org?subject=Entry%20form&cc=desk%40example.org", 4, "")),
    ];
    pdf(&objs)
}

struct Request {
    head: String,
    body: Vec<u8>,
}

/// A loopback HTTP server: `/redirect` answers 302, `/big` 200 with 5000 bytes, `/slow` never
/// answers, anything else 200 "thanks". Every request it reads comes out of the receiver.
fn server() -> (String, Receiver<Request>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { return };
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head, mut body) = loop {
                    let n = s.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break (String::from_utf8_lossy(&buf[..i]).into_owned(), buf[i + 4..].to_vec());
                    }
                };
                let len = head
                    .lines()
                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                    .unwrap_or(0);
                while body.len() < len {
                    let n = s.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&chunk[..n]);
                }
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                let _ = tx.send(Request { head, body });
                let reply = match path.as_str() {
                    "/redirect" => {
                        "HTTP/1.1 302 Found\r\nLocation: http://example.org/elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                    }
                    "/big" => format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5000\r\nConnection: close\r\n\r\n{}",
                        "x".repeat(5000)
                    ),
                    "/reply" => {
                        let fdf =
                            "%FDF-1.2\n1 0 obj\n<< /FDF << /Fields [<< /T (town) /V (Paris) >>] >> >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.fdf\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{fdf}",
                            fdf.len()
                        )
                    }
                    "/slow" => {
                        std::thread::sleep(Duration::from_secs(20));
                        return;
                    }
                    _ => "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 6\r\nConnection: close\r\n\r\nthanks".to_string(),
                };
                let _ = s.write_all(reply.as_bytes());
            });
        }
    });
    (addr, rx)
}

fn setup(test: &str) -> (Automation, u64, PathBuf, String, Receiver<Request>) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-submit-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (addr, rx) = server();
    std::fs::write(dir.join("entry.pdf"), form(&addr)).unwrap();
    let mut a = Automation::new().with_root(&dir).unwrap();
    let doc = ok(&mut a, "doc_open", json!({ "path": "entry.pdf" }))["doc"].as_u64().unwrap();
    (a, doc, dir, addr, rx)
}

fn b64(v: &Value) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(v.as_str().unwrap()).unwrap()
}

fn has(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn prepare_builds_each_format_and_flag_without_sending() {
    let (mut a, doc, dir, server, requests) = setup("prepare");

    // HTML form encoding with the click position, named by the button's mapping name.
    let r = ok(&mut a, "form_submit_prepare", json!({ "doc": doc, "field": "html", "click": [5.4, 7] }));
    assert_eq!((r["target"].as_str(), r["method"].as_str(), r["format"].as_str()), (Some("web"), Some("POST"), Some("HTML")), "{r}");
    assert_eq!(r["content_type"], "application/x-www-form-urlencoded");
    assert_eq!(r["url"], format!("{server}/in"));
    assert_eq!(r["body"], "name=Ada+Lovelace&town=London+%26+Co&born=12%2F10%2F1815&go.x=5&go.y=7", "NoExport and empty fields are left out");
    assert_eq!(r["flag_names"], json!(["ExportFormat", "SubmitCoordinates"]));
    assert_eq!(r["fields"], json!(["name", "town", "born"]));
    assert_eq!(b64(&r["body_base64"]), r["body"].as_str().unwrap().as_bytes());
    // Without a click, 0,0 is sent and said.
    let r = ok(&mut a, "form_submit_prepare", json!({ "doc": doc, "field": "html" }));
    assert!(r["body"].as_str().unwrap().ends_with("&go.x=0&go.y=0") && r["notes"][0].as_str().unwrap().contains("SubmitCoordinates"), "{r}");

    // GET: the data goes in the query (after the address's own), dates in canonical form.
    let r = ok(&mut a, "form_submit_prepare", json!({ "doc": doc, "field": "get" }));
    assert_eq!(r["method"], "GET");
    assert_eq!(r["url"], format!("{server}/q?form=1&name=Ada+Lovelace&town=London+%26+Co&born=D%3A18151210000000"), "{r}");
    assert_eq!((r["bytes"].as_u64(), r["body"].as_str()), (Some(0), Some("")));

    // FDF: only this user's comments, no /F, and the incremental update once there is one.
    let r = ok(&mut a, "form_submit_prepare", json!({ "doc": doc, "field": "fdf", "user": "Ada" }));
    let body = b64(&r["body_base64"]);
    assert_eq!((r["content_type"].as_str(), r["annotations"].as_u64()), (Some("application/vnd.fdf"), Some(1)), "{r}");
    assert!(body.starts_with(b"%FDF-1.2") && has(&body, b"(Mine)") && !has(&body, b"(Not mine)"), "{}", String::from_utf8_lossy(&body));
    assert!(!has(&body, b"/F (entry.pdf)") && !has(&body, b"/Differences"), "ExclFKey; nothing appended yet");
    assert!(r["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().starts_with("IncludeAppendSaves")), "{r}");
    assert!(r.get("body").is_none(), "FDF is binary: base64 only");
    ok(&mut a, "form_fill", json!({ "doc": doc, "values": { "empty": "now filled" } }));
    let r = ok(&mut a, "form_submit_prepare", json!({ "doc": doc, "field": "fdf", "user": "Ada" }));
    let body = b64(&r["body_base64"]);
    assert!(has(&body, b"/Differences") && has(&body, b"now filled") && has(&body, b"endstream"), "{}", String::from_utf8_lossy(&body));
    assert_eq!(r["notes"], json!([]));
    // No user name: ExclNonUserAnnots keeps no comments, and says so.
    let r = ok(&mut a, "form_submit_prepare", json!({ "doc": doc, "field": "fdf" }));
    assert_eq!(r["annotations"], 0);
    assert!(r["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().starts_with("ExclNonUserAnnots")));

    // XFDF: everything but the excluded field, empty fields included, NoExport never.
    let r = ok(&mut a, "form_submit_prepare", json!({ "doc": doc, "field": "xfdf" }));
    let x = r["body"].as_str().unwrap();
    assert_eq!(r["content_type"], "application/vnd.adobe.xfdf");
    assert_eq!(r["fields"], json!(["name", "born", "empty"]));
    assert!(x.contains("<field name=\"name\">") && !x.contains("town") && !x.contains("secret") && x.contains("<f href=\"entry.pdf\"/>"), "{x}");

    // SubmitPDF wins over the other flags and sends the document as Save writes it.
    let r = ok(&mut a, "form_submit_prepare", json!({ "doc": doc, "field": "pdf" }));
    assert_eq!(
        (r["format"].as_str(), r["content_type"].as_str(), r["flag_names"].clone()),
        (Some("PDF"), Some("application/pdf"), json!(["SubmitPDF"]))
    );
    assert!(b64(&r["body_base64"]).starts_with(b"%PDF-") && r["filename"] == "entry.pdf");

    // EmbedForm: the document travels inside the FDF as an embedded file.
    let r = ok(&mut a, "form_submit_prepare", json!({ "doc": doc, "field": "embed" }));
    let body = b64(&r["body_base64"]);
    assert!(has(&body, b"/EF") && has(&body, b"/EmbeddedFile") && has(&body, b"%PDF-"), "{}", r["notes"]);

    // Options for a bare address, or replacing a button's flags and fields.
    let r = ok(&mut a, "form_submit_prepare", json!({ "doc": doc, "url": format!("{server}/x"), "options": ["XFDF", "IncludeAnnotations"] }));
    let x = r["body"].as_str().unwrap();
    assert!(x.contains("<annots>") && x.contains("Not mine") && r["annotations"] == 2, "{x}");
    let r = ok(&mut a, "form_submit_prepare", json!({ "doc": doc, "field": "html", "flags": 4, "fields": ["town"] }));
    assert_eq!((r["body"].as_str(), r["url"].clone()), (Some("town=London+%26+Co"), json!(format!("{server}/in"))));
    // A format switch keeps the flags that format uses.
    let r = ok(&mut a, "form_submit_prepare", json!({ "doc": doc, "field": "get", "format": "xfdf" }));
    assert_eq!((r["method"].as_str(), r["flag_names"].clone()), (Some("POST"), json!(["XFDF", "CanonicalFormat"])));
    assert!(r["body"].as_str().unwrap().contains("<value>D:18151210000000</value>"));

    // mailto: the email it asks for.
    let r = ok(&mut a, "form_submit_prepare", json!({ "doc": doc, "field": "mail" }));
    assert_eq!((r["target"].as_str(), r["method"].as_str(), r["sendable"].as_bool()), (Some("email"), Some("EMAIL"), Some(true)));
    assert_eq!(r["email"], json!({ "to": ["forms@example.org"], "cc": ["desk@example.org"], "bcc": [], "subject": "Entry form", "body": null }));

    // Addresses that can't be sent to are reported, not built around.
    for url in ["file:///etc/passwd", "ftp://example.org/x", "forms/in", "http://user:pw@example.org/x", "mailto:?subject=nobody"] {
        let r = ok(&mut a, "form_submit_prepare", json!({ "doc": doc, "url": url }));
        assert_eq!((r["sendable"].as_bool(), r["target"].clone()), (Some(false), Value::Null), "{url}: {r}");
        assert!(r["problem"].is_string());
    }
    // Hostile arguments are refused with a reason.
    for bad in [
        json!({ "doc": doc }),
        json!({ "doc": doc, "field": "name" }),
        json!({ "doc": doc, "field": "nope" }),
        json!({ "doc": doc, "url": "http://a.example", "flags": -1 }),
        json!({ "doc": doc, "url": "http://a.example", "flags": 1u64 << 40 }),
        json!({ "doc": doc, "url": "http://a.example", "options": ["Nope"] }),
        json!({ "doc": doc, "url": "http://a.example", "options": "XFDF" }),
        json!({ "doc": doc, "url": "http://a.example", "click": [1] }),
        json!({ "doc": doc, "url": "http://a.example", "click": ["a", 2] }),
        json!({ "doc": doc, "url": "http://a.example", "fields": [1] }),
        json!({ "doc": doc, "url": "http://a.example", "format": "docx" }),
    ] {
        invalid(&mut a, "form_submit_prepare", bad);
    }
    assert!(requests.try_recv().is_err(), "preparing sends nothing");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn send_needs_confirmation_and_reports_the_answer() {
    let (mut a, doc, dir, server, requests) = setup("send");
    let wait = |rx: &Receiver<Request>| rx.recv_timeout(Duration::from_secs(10)).unwrap();

    // No confirm (or confirm: false): refused before anything is built or sent.
    let m = invalid(&mut a, "form_submit_send", json!({ "doc": doc, "field": "html" }));
    assert!(m.contains("confirm: true"), "{m}");
    invalid(&mut a, "form_submit_send", json!({ "doc": doc, "field": "html", "confirm": false }));
    assert!(requests.try_recv().is_err());

    // POST of the HTML form.
    let r = ok(&mut a, "form_submit_send", json!({ "doc": doc, "field": "html", "click": [1, 2], "confirm": true }));
    assert_eq!(
        (r["sent"].as_bool(), r["status"].as_u64(), r["ok"].as_bool(), r["reply"].as_str()),
        (Some(true), Some(200), Some(true), Some("thanks")),
        "{r}"
    );
    assert_eq!((r["reply_content_type"].as_str(), r["reply_truncated"].as_bool()), (Some("text/plain"), Some(false)));
    let q = wait(&requests);
    assert!(q.head.starts_with("POST /in HTTP/1.1"), "{}", q.head);
    assert!(q.head.to_ascii_lowercase().contains("content-type: application/x-www-form-urlencoded"));
    assert_eq!(q.body, b"name=Ada+Lovelace&town=London+%26+Co&born=12%2F10%2F1815&go.x=1&go.y=2");

    // GET with the query.
    ok(&mut a, "form_submit_send", json!({ "doc": doc, "field": "get", "confirm": true }));
    let q = wait(&requests);
    assert!(q.head.starts_with("GET /q?form=1&name=Ada+Lovelace&town=London+%26+Co&born=D%3A18151210000000 HTTP/1.1"), "{}", q.head);
    assert!(q.body.is_empty());

    // XFDF and PDF bodies arrive with their content types.
    ok(&mut a, "form_submit_send", json!({ "doc": doc, "field": "xfdf", "confirm": true }));
    let q = wait(&requests);
    assert!(q.head.to_ascii_lowercase().contains("content-type: application/vnd.adobe.xfdf") && q.body.starts_with(b"<?xml"), "{}", q.head);
    ok(&mut a, "form_submit_send", json!({ "doc": doc, "field": "pdf", "confirm": true }));
    let q = wait(&requests);
    assert!(q.head.to_ascii_lowercase().contains("content-type: application/pdf") && q.body.starts_with(b"%PDF-"));

    // A redirect is reported, never followed.
    let r = ok(&mut a, "form_submit_send", json!({ "doc": doc, "url": format!("{server}/redirect"), "confirm": true }));
    assert_eq!((r["status"].as_u64(), r["ok"].as_bool(), r["location"].as_str()), (Some(302), Some(false), Some("http://example.org/elsewhere")));
    wait(&requests);
    // The answer is capped.
    let r = ok(&mut a, "form_submit_send", json!({ "doc": doc, "url": format!("{server}/big"), "confirm": true, "max_response_bytes": 100 }));
    assert_eq!((r["reply_bytes"].as_u64(), r["reply_truncated"].as_bool(), r["reply"].as_str().map(str::len)), (Some(100), Some(true), Some(100)));
    wait(&requests);
    // A server that never answers runs into the time limit.
    let started = std::time::Instant::now();
    let e = a.call("form_submit_send", &json!({ "doc": doc, "url": format!("{server}/slow"), "confirm": true, "timeout_seconds": 1 }));
    assert!(matches!(e, Err(ToolError::Failed(_))), "{e:?}");
    assert!(started.elapsed() < Duration::from_secs(10), "the time limit holds");
    wait(&requests);

    // An FDF answer is loaded into the form when asked (Acrobat imports it), as one undo step.
    let r = ok(&mut a, "form_submit_send", json!({ "doc": doc, "url": format!("{server}/reply"), "confirm": true }));
    assert_eq!(r["imported"], false, "only with import_reply");
    wait(&requests);
    let r = ok(&mut a, "form_submit_send", json!({ "doc": doc, "url": format!("{server}/reply"), "confirm": true, "import_reply": true }));
    assert_eq!(r["imported"], true, "{r}");
    wait(&requests);
    let town = |a: &mut Automation| {
        ok(a, "form_submit_prepare", json!({ "doc": doc, "url": "http://a.example", "flags": 4, "fields": ["town"] }))["body"].clone()
    };
    assert_eq!(town(&mut a), "town=Paris");
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(town(&mut a), "town=London+%26+Co");

    // mailto: a prepared email, never sent; the attachment inline or written to a path.
    let r = ok(&mut a, "form_submit_send", json!({ "doc": doc, "field": "mail", "confirm": true }));
    assert_eq!((r["sent"].as_bool(), r["target"].as_str()), (Some(false), Some("email")), "{r}");
    let mail = &r["email"];
    assert_eq!((mail["to"].clone(), mail["subject"].as_str()), (json!(["forms@example.org"]), Some("Entry form")));
    assert_eq!(
        (mail["attachment"]["name"].as_str(), mail["attachment"]["content_type"].as_str()),
        (Some("entry.txt"), Some("application/x-www-form-urlencoded"))
    );
    assert!(b64(&mail["attachment"]["base64"]).starts_with(b"name=Ada+Lovelace"));
    let r =
        ok(&mut a, "form_submit_send", json!({ "doc": doc, "url": "mailto:forms@example.org", "confirm": true, "attachment_path": "out/data.fdf" }));
    let written = std::fs::read(dir.join("out/data.fdf")).unwrap();
    assert!(written.starts_with(b"%FDF") && r["email"]["attachment"]["base64"].is_null());
    assert_eq!(r["email"]["subject"], "Form data: entry.pdf", "a default subject");

    // Only http and https; no credentials; limits checked.
    for url in ["file:///etc/passwd", "ftp://example.org/x", "forms/in", "http://user:pw@127.0.0.1:9/x", "javascript:alert(1)"] {
        invalid(&mut a, "form_submit_send", json!({ "doc": doc, "url": url, "confirm": true }));
    }
    for bad in [
        json!({ "doc": doc, "field": "html", "confirm": true, "timeout_seconds": 0 }),
        json!({ "doc": doc, "field": "html", "confirm": true, "timeout_seconds": 100000 }),
        json!({ "doc": doc, "field": "html", "confirm": true, "max_response_bytes": 1u64 << 40 }),
        json!({ "doc": doc, "field": "html", "confirm": true, "attachment_path": "x.txt" }),
        json!({ "doc": doc, "field": "mail", "confirm": true, "attachment_path": "../outside.txt" }),
    ] {
        assert!(a.call("form_submit_send", &bad).is_err(), "{bad}");
    }
    assert!(requests.try_recv().is_err(), "nothing else was sent");

    // Blocked by the internet access settings: refused, nothing sent.
    ok(&mut a, "trust_urls_set", json!({ "mode": "block-all" }));
    let e = a.call("form_submit_send", &json!({ "doc": doc, "field": "html", "confirm": true }));
    assert!(matches!(&e, Err(ToolError::Failed(m)) if m.starts_with("blocked")), "{e:?}");
    assert!(requests.try_recv().is_err());
    let _ = std::fs::remove_dir_all(dir);
}

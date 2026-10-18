//! Internet access from PDFs end to end: `trust_urls_get`, `trust_urls_set`, `trust_url_check`,
//! and the decision reported (or enforced) by `form_submit`, `js_run` and `link_list`.

use pdfcraft_automation::{Automation, Content, ToolError};
use serde_json::{Value, json};

fn call(app: &mut Automation, name: &str, args: Value) -> Value {
    match app.call(name, &args).unwrap().remove(0) {
        Content::Json(v) => v,
        _ => panic!("JSON result"),
    }
}

/// A temp root holding a blank one-page `blank.pdf`, opened.
fn open(tag: &str) -> (Automation, u64, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-url-access-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let blank = pdfcraft_engine::Session::new().create_blank(200.0, 200.0, 1).unwrap();
    std::fs::write(dir.join("blank.pdf"), blank.as_slice()).unwrap();
    let mut app = Automation::new().with_root(&dir).unwrap();
    let doc = call(&mut app, "doc_open", json!({"path":"blank.pdf"}))["doc"].as_u64().unwrap();
    (app, doc, dir)
}

fn decision(app: &mut Automation, url: &str) -> String {
    call(app, "trust_url_check", json!({ "url": url }))["results"][0]["decision"].as_str().unwrap().to_string()
}

#[test]
fn the_policy_is_read_changed_and_evaluated() {
    let mut app = Automation::new();
    let p = call(&mut app, "trust_urls_get", json!({}));
    assert_eq!((p["mode"].as_str(), p["default_action"].as_str(), p["rules"].clone()), (Some("custom"), Some("ask"), json!([])), "{p}");
    assert_eq!(decision(&mut app, "https://example.com/"), "prompt");

    let p = call(
        &mut app,
        "trust_urls_set",
        json!({ "default_action": "block", "rules": [
            { "host": "https://user:pw@Example.COM.:8443/x", "action": "allow" },
            { "host": "*.cdn.example.net", "action": "ask" },
            { "host": "exact.org", "action": "allow", "subdomains": false },
            { "host": "BÜCHER.de", "action": "allow" },
            { "host": "127.0.0.1", "action": "allow" },
        ] }),
    );
    let hosts: Vec<&str> = p["rules"].as_array().unwrap().iter().map(|r| r["host"].as_str().unwrap()).collect();
    assert_eq!(hosts, ["example.com", "cdn.example.net", "exact.org", "xn--bcher-kva.de", "127.0.0.1"], "{p}");
    assert_eq!(p["rules"][4]["subdomains"], false, "an IP literal matches only itself");

    let r = call(
        &mut app,
        "trust_url_check",
        json!({ "urls": [
            "https://www.example.com/a", "https://example.com@evil.net/", "https://img.cdn.example.net/", "https://www.exact.org/",
            "http://bücher.de/", "http://0x7f.1:9/", "mailto:a@example.org", "file:///etc/passwd", "javascript:alert(1)",
        ] }),
    );
    let got: Vec<(&str, &str)> =
        r["results"].as_array().unwrap().iter().map(|x| (x["decision"].as_str().unwrap(), x["kind"].as_str().unwrap())).collect();
    assert_eq!(
        got,
        [
            ("allow", "web"),
            ("block", "web"),
            ("prompt", "web"),
            ("block", "web"),
            ("allow", "web"),
            ("allow", "web"),
            ("prompt", "mail"),
            ("block", "refused"),
            ("block", "refused"),
        ],
        "{r}"
    );
    assert_eq!(r["results"][1]["host"], "evil.net");
    assert_eq!(r["results"][0]["rule"]["host"], "example.com");
    assert_eq!(r["results"][7]["scheme"], "file");
    assert!(r["results"][1]["reason"].as_str().unwrap().contains("evil.net"), "{r}");

    // Remove, then add (replacing an existing rule's action).
    let p = call(
        &mut app,
        "trust_urls_set",
        json!({ "remove": ["EXACT.org.", "nothing.example"], "add": [{ "host": "example.com", "action": "block" }] }),
    );
    assert_eq!(p["removed"], json!(["EXACT.org."]));
    assert_eq!(p["rules"].as_array().unwrap().len(), 4);
    assert_eq!(decision(&mut app, "https://example.com/"), "block");

    // The modes override the list.
    call(&mut app, "trust_urls_set", json!({ "mode": "allow-all" }));
    assert_eq!(decision(&mut app, "https://example.com/"), "allow");
    assert_eq!(decision(&mut app, "ftp://example.com/"), "block", "other schemes stay refused");
    call(&mut app, "trust_urls_set", json!({ "mode": "block-all" }));
    assert_eq!(decision(&mut app, "https://anything.example/"), "block");
    assert_eq!(decision(&mut app, "mailto:a@example.org"), "prompt", "email is reported apart");
}

#[test]
fn bad_arguments_change_nothing() {
    let mut app = Automation::new();
    call(&mut app, "trust_urls_set", json!({ "rules": [{ "host": "keep.example", "action": "allow" }] }));
    let before = call(&mut app, "trust_urls_get", json!({}));
    for bad in [
        json!({ "mode": "sometimes" }),
        json!({ "default_action": "maybe" }),
        json!({ "mode": "block-all", "add": [{ "host": "exa mple.com", "action": "allow" }] }),
        json!({ "add": [{ "host": "a.*.example", "action": "allow" }] }),
        json!({ "add": [{ "host": "ok.example" }] }),
        json!({ "add": [{ "host": "ok.example", "action": "allow", "subdomains": "yes" }] }),
        json!({ "rules": "example.com" }),
        json!({ "remove": [""] }),
        json!({ "add": [{ "host": "example.com:99999", "action": "allow" }] }),
    ] {
        assert!(matches!(app.call("trust_urls_set", &bad), Err(ToolError::InvalidArgs(_))), "{bad}");
    }
    assert_eq!(call(&mut app, "trust_urls_get", json!({})), before);
    for bad in [json!({}), json!({ "url": "https://a.example/", "urls": ["https://b.example/"] }), json!({ "urls": [] })] {
        assert!(matches!(app.call("trust_url_check", &bad), Err(ToolError::InvalidArgs(_))), "{bad}");
    }
    let many: Vec<String> = (0..101).map(|i| format!("https://h{i}.example/")).collect();
    assert!(app.call("trust_url_check", &json!({ "urls": many })).is_err());
}

#[test]
fn a_blocked_host_is_not_submitted_to_and_requests_report_the_decision() {
    let (mut app, doc, dir) = open("submit");
    call(&mut app, "trust_urls_set", json!({ "default_action": "allow", "rules": [{ "host": "127.0.0.1", "action": "block" }] }));
    // Port 9 on loopback: nothing listens, so a send that got through would fail differently.
    let e = app.call("form_submit", &json!({ "doc": doc, "url": "http://127.0.0.1:9/in", "send": true }));
    match e {
        Err(ToolError::Failed(m)) => assert!(m.starts_with("blocked") && m.contains("127.0.0.1"), "{m}"),
        other => panic!("expected a blocked failure, got {other:?}"),
    }
    // Saving the data instead is still allowed.
    let r = call(&mut app, "form_submit", json!({ "doc": doc, "url": "http://127.0.0.1:9/in", "path": "data.fdf" }));
    assert!(r["path"].is_string(), "{r}");

    // A script's requests carry the decision.
    let r = call(&mut app, "js_run", json!({ "doc": doc, "script": "app.launchURL('http://127.0.0.1/x'); app.launchURL('https://ok.example/');" }));
    let reqs = r["requests"].as_array().unwrap();
    assert_eq!(reqs.len(), 2, "{r}");
    assert_eq!(reqs[0]["url_access"]["decision"], "block", "{r}");
    assert_eq!(reqs[1]["url_access"]["decision"], "allow", "{r}");

    // So do links.
    call(&mut app, "link_add", json!({ "doc": doc, "page": 1, "rect": [10, 10, 80, 30], "url": "http://127.0.0.1/doc" }));
    let l = call(&mut app, "link_list", json!({ "doc": doc }));
    assert_eq!(l["links"][0]["url_access"]["decision"], "block", "{l}");
    std::fs::remove_dir_all(dir).unwrap();
}

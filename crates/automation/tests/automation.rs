//! End-to-end tests of the automation tools and the MCP server, on synthetic PDFs.

use std::path::{Path, PathBuf};

#[cfg(feature = "mcp")]
use pdfcraft_automation::mcp::McpServer;
use pdfcraft_automation::{Automation, Content, ToolError, tools};
use serde_json::{Value, json};

/// A PDF with `n` 200×300 pt pages reading "Page 1", "Page 2", …
fn fixture(n: usize) -> Vec<u8> {
    let mut objs: Vec<Vec<u8>> = vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()];
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    objs.push(format!("<< /Type /Pages /Kids [{}] /Count {n} /MediaBox [0 0 200 300] >>", kids.join(" ")).into_bytes());
    objs.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    for i in 0..n {
        objs.push(format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>", 5 + 2 * i).into_bytes());
        let body = format!("BT /F1 24 Tf 20 150 Td (Page {}) Tj ET", i + 1);
        objs.push(format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()).into_bytes());
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

/// A fresh directory with `a.pdf` (3 pages) and `b.pdf` (2 pages).
fn workdir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-automation-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.pdf"), fixture(3)).unwrap();
    std::fs::write(dir.join("b.pdf"), fixture(2)).unwrap();
    dir
}

fn auto(dir: &Path) -> Automation {
    Automation::new().with_root(dir).unwrap().with_clock(|| 1_700_000_000)
}

fn ok(a: &mut Automation, tool: &str, args: Value) -> Value {
    match a.call(tool, &args) {
        Ok(mut c) => match c.remove(0) {
            Content::Json(v) => v,
            other => panic!("{tool}: expected JSON, got {other:?}"),
        },
        Err(e) => panic!("{tool} {args}: {e}"),
    }
}

fn page_text(a: &mut Automation, doc: u64) -> Vec<String> {
    ok(a, "text_extract", json!({ "doc": doc }))["pages"].as_array().unwrap().iter().map(|p| p["text"].as_str().unwrap().trim().to_string()).collect()
}

#[test]
fn tool_table_is_well_formed() {
    let all = tools();
    let mut names = std::collections::HashSet::new();
    for t in &all {
        assert!(names.insert(t.name), "duplicate tool {}", t.name);
        assert!(t.name.len() <= 64 && t.name.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "bad MCP tool name {}", t.name);
        assert_eq!(t.input_schema["type"], "object", "{}", t.name);
        let props = t.input_schema["properties"].as_object().unwrap();
        for r in t.input_schema["required"].as_array().unwrap() {
            assert!(props.contains_key(r.as_str().unwrap()), "{}: required {r} is not a property", t.name);
        }
        assert!(!(t.read_only && t.destructive), "{} is both read-only and destructive", t.name);
        if let Some(c) = t.command {
            assert!(pdfcraft_engine::commands::command(c).is_some(), "{} names unregistered command {c}", t.name);
        }
    }
}

/// Each of these writes to its required `path` and replaces an existing file there, so the MCP
/// annotations must not tell clients the call is read-only or harmless (#130).
#[test]
fn file_writing_tools_are_not_read_only() {
    for name in ["doc_export_data", "accessibility_report", "image_save"] {
        let t = tools().into_iter().find(|t| t.name == name).unwrap();
        assert!(!t.read_only, "{name} writes a file but advertises read-only");
        assert!(t.destructive, "{name} overwrites its path but advertises non-destructive");
    }
}

#[test]
fn open_inspect_render_and_find() {
    let dir = workdir("inspect");
    let mut a = auto(&dir);
    let opened = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }));
    assert_eq!(opened["pages"], 3);
    assert_eq!(opened["editable"], true);
    let doc = opened["doc"].as_u64().unwrap();

    let info = ok(&mut a, "doc_info", json!({ "doc": doc }));
    assert_eq!(info["pages"].as_array().unwrap().len(), 3);
    assert_eq!(info["pages"][0]["width"], 200.0);
    assert_eq!(info["pages"][1]["label"], "2");

    assert_eq!(page_text(&mut a, doc), ["Page 1", "Page 2", "Page 3"]);

    let found = ok(&mut a, "text_find", json!({ "doc": doc, "query": "page  2" }));
    assert_eq!(found["count"], 1);
    assert_eq!(found["matches"][0]["page"], 2);
    let rect = found["matches"][0]["rects"][0].as_array().unwrap();
    assert!(rect[1].as_f64().unwrap() > 100.0 && rect[3].as_f64().unwrap() < 160.0, "rect is top-left based: {rect:?}");

    let png = a.call("page_render", &json!({ "doc": doc, "page": 1, "dpi": 72 })).unwrap();
    let Content::Png { data, width, height } = &png[0] else { panic!("expected an image") };
    assert_eq!((*width, *height), (200, 300));
    assert_eq!(&data[..8], b"\x89PNG\r\n\x1a\n");
    let decoder = png::Decoder::new(std::io::Cursor::new(data.as_slice()));
    let info = decoder.read_info().unwrap().info().clone();
    assert_eq!((info.width, info.height), (200, 300));
}

#[test]
fn edit_undo_redo_and_save_round_trip() {
    let dir = workdir("edit");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();

    let s = ok(&mut a, "page_delete", json!({ "doc": doc, "pages": [2] }));
    assert_eq!((s["pages"].as_u64(), s["dirty"].as_bool()), (Some(2), Some(true)));
    assert_eq!(page_text(&mut a, doc), ["Page 1", "Page 3"]);

    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(page_text(&mut a, doc), ["Page 1", "Page 2", "Page 3"]);
    ok(&mut a, "edit_redo", json!({ "doc": doc }));
    assert_eq!(page_text(&mut a, doc), ["Page 1", "Page 3"]);

    ok(&mut a, "page_insert_file", json!({ "doc": doc, "path": "b.pdf", "pages": [2], "at": 1 }));
    ok(&mut a, "page_insert_blank", json!({ "doc": doc, "at": 4 }));
    ok(&mut a, "page_move", json!({ "doc": doc, "pages": [3], "to": 1 }));
    ok(&mut a, "page_rotate", json!({ "doc": doc, "pages": [1], "degrees": 90 }));
    ok(&mut a, "doc_set_info", json!({ "doc": doc, "key": "Title", "value": "Automated" }));
    assert_eq!(page_text(&mut a, doc), ["Page 3", "Page 2", "Page 1", ""]);

    let saved = ok(&mut a, "doc_save", json!({ "doc": doc, "path": "out/edited.pdf" }));
    assert_eq!(saved["incremental"], false);
    assert_eq!(saved["document"]["dirty"], false);

    let mut b = auto(&dir);
    let re = ok(&mut b, "doc_open", json!({ "path": "out/edited.pdf" }))["doc"].as_u64().unwrap();
    let info = ok(&mut b, "doc_info", json!({ "doc": re }));
    assert_eq!(info["title"], "Automated");
    assert_eq!(info["pages"][0]["rotation"], 90);
    assert_eq!(page_text(&mut b, re), ["Page 3", "Page 2", "Page 1", ""]);

    // Saving in place appends an incremental update.
    ok(&mut b, "doc_set_info", json!({ "doc": re, "key": "Author", "value": "Agent" }));
    let before = std::fs::metadata(dir.join("out/edited.pdf")).unwrap().len();
    let again = ok(&mut b, "doc_save", json!({ "doc": re }));
    assert_eq!(again["incremental"], true);
    let after = std::fs::read(dir.join("out/edited.pdf")).unwrap();
    assert!(after.len() as u64 > before);
    assert_eq!(after.windows(5).filter(|w| w == b"%%EOF").count(), 2);
}

#[test]
fn combine_extract_and_split() {
    let dir = workdir("organize");
    let mut a = auto(&dir);
    let combined = ok(&mut a, "doc_combine", json!({ "paths": ["a.pdf", "b.pdf"], "out": "ab.pdf", "open": true }));
    let doc = combined["document"]["doc"].as_u64().unwrap();
    assert_eq!(combined["document"]["pages"], 5);
    assert_eq!(page_text(&mut a, doc), ["Page 1", "Page 2", "Page 3", "Page 1", "Page 2"]);
    // Chosen pages per file, in the order given.
    let some = ok(&mut a, "doc_combine", json!({ "paths": ["a.pdf", "b.pdf"], "pages": ["3, 1", null], "open": true }));
    assert_eq!(page_text(&mut a, some["document"]["doc"].as_u64().unwrap()), ["Page 3", "Page 1", "Page 1", "Page 2"]);
    assert!(matches!(a.call("doc_combine", &json!({ "paths": ["a.pdf", "b.pdf"], "pages": ["9", null] })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("doc_combine", &json!({ "paths": ["a.pdf", "b.pdf"], "pages": ["1"] })), Err(ToolError::InvalidArgs(_))));

    let ex = ok(&mut a, "page_extract", json!({ "doc": doc, "pages": [2, 4] }));
    let ex_doc = ex["document"]["doc"].as_u64().unwrap();
    assert_eq!(page_text(&mut a, ex_doc), ["Page 2", "Page 1"]);
    assert!(ex.get("path").is_none());

    let split = ok(&mut a, "doc_split", json!({ "doc": doc, "every": 2, "out_dir": "parts" }));
    let files = split["files"].as_array().unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!((files[2]["first_page"].as_u64(), files[2]["last_page"].as_u64()), (Some(5), Some(5)));
    for f in files {
        assert!(Path::new(f["path"].as_str().unwrap()).starts_with(dir.canonicalize().unwrap()));
    }
}

#[test]
fn errors_are_specific_and_safe() {
    let dir = workdir("errors");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();

    let err = |a: &mut Automation, tool: &str, args: Value| a.call(tool, &args).unwrap_err();
    assert!(matches!(err(&mut a, "nope", json!({})), ToolError::UnknownTool(_)));
    assert!(matches!(err(&mut a, "page_delete", json!({ "doc": doc, "pages": [9] })), ToolError::InvalidArgs(m) if m.contains("3 pages")));
    assert!(matches!(err(&mut a, "page_delete", json!({ "doc": doc, "pages": [0] })), ToolError::InvalidArgs(_)));
    assert!(matches!(err(&mut a, "page_delete", json!({ "doc": doc, "page": [1] })), ToolError::InvalidArgs(m) if m.contains("unknown argument")));
    assert!(matches!(err(&mut a, "page_rotate", json!({ "doc": doc, "pages": [1], "degrees": 45 })), ToolError::InvalidArgs(_)));
    assert!(matches!(err(&mut a, "doc_info", json!({ "doc": 99 })), ToolError::Failed(_)));
    assert!(matches!(err(&mut a, "edit_undo", json!({ "doc": doc })), ToolError::Failed(_)));

    // Unsaved changes are never dropped silently.
    ok(&mut a, "page_delete", json!({ "doc": doc, "pages": [1] }));
    assert!(matches!(err(&mut a, "doc_close", json!({ "doc": doc })), ToolError::Failed(m) if m.contains("unsaved")));
    ok(&mut a, "doc_close", json!({ "doc": doc, "discard_changes": true }));
    assert_eq!(ok(&mut a, "doc_list", json!({}))["documents"], json!([]));

    // The root confines reads and writes.
    let outside = std::env::temp_dir().join("pdfcraft-automation-outside.pdf");
    std::fs::write(&outside, fixture(1)).unwrap();
    assert!(matches!(err(&mut a, "doc_open", json!({ "path": outside.to_str().unwrap() })), ToolError::Failed(m) if m.contains("outside")));
    assert!(
        matches!(err(&mut a, "doc_open", json!({ "path": "../pdfcraft-automation-outside.pdf" })), ToolError::Failed(m) if m.contains("outside"))
    );
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    assert!(a.call("doc_save", &json!({ "doc": doc, "path": "new/../../escape.pdf" })).is_err());
    assert!(a.call("doc_save", &json!({ "doc": doc, "path": outside.to_str().unwrap() })).is_err());
    let _ = std::fs::remove_file(outside);
}

/// `root/` (with `inside.pdf`) next to `outside/` (with the file `secret.pdf` and the folder
/// `sub`), all in a fresh temporary directory. Returns (base, canonical root).
fn sandbox(test: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("pdfcraft-automation-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("root")).unwrap();
    std::fs::create_dir_all(base.join("outside/sub")).unwrap();
    std::fs::write(base.join("root/inside.pdf"), fixture(1)).unwrap();
    std::fs::write(base.join("outside/secret.pdf"), fixture(1)).unwrap();
    let root = base.join("root").canonicalize().unwrap();
    (base, root)
}

/// A link `root/<name>` to the directory `target`: a symlink on Unix, a junction on Windows
/// (which needs no privilege).
fn link_dir(root: &Path, name: &str, target: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, root.join(name)).unwrap();
    #[cfg(windows)]
    {
        // Rebuilt from components so every separator is `\` (cmd reads `/x` as a switch).
        let (link, target): (PathBuf, PathBuf) = (root.join(name).components().collect(), target.components().collect());
        let status = std::process::Command::new("cmd").arg("/C").arg("mklink").arg("/J").arg(link).arg(target).output().unwrap();
        assert!(status.status.success(), "mklink /J failed: {}", String::from_utf8_lossy(&status.stderr));
    }
}

#[test]
#[cfg(any(unix, windows))]
fn saving_to_a_remembered_path_rechecks_the_root() {
    let (base, root) = sandbox("remembered-save-root");
    std::fs::create_dir(root.join("documents")).unwrap();
    let original = fixture(1);
    std::fs::write(root.join("documents/report.pdf"), &original).unwrap();
    let mut a = auto(&root);
    let doc = ok(&mut a, "doc_open", json!({ "path": "documents/report.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "doc_set_info", json!({ "doc": doc, "key": "Title", "value": "Unsaved" }));

    // Another process can replace a directory after the document was opened. Its stored
    // canonical path must not bypass the same check used by an explicit Save As path.
    std::fs::rename(root.join("documents"), root.join("original-documents")).unwrap();
    link_dir(&root, "documents", &base.join("outside"));
    std::fs::write(base.join("outside/report.pdf"), b"KEEP OUTSIDE FILE").unwrap();
    let error = a.call("doc_save", &json!({ "doc": doc })).unwrap_err();
    assert!(error.to_string().contains("outside the allowed directory"), "{error}");
    assert_eq!(std::fs::read(base.join("outside/report.pdf")).unwrap(), b"KEEP OUTSIDE FILE");
    assert_eq!(std::fs::read(root.join("original-documents/report.pdf")).unwrap(), original);
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc }))["document"]["dirty"], true);

    let saved = ok(&mut a, "doc_save", json!({ "doc": doc, "path": "recovered.pdf" }));
    assert_eq!(saved["document"]["dirty"], false);
    assert!(root.join("recovered.pdf").is_file());
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn root_refusals_do_not_reveal_what_exists_outside() {
    // Regression test for #136: every path outside the root gets the same refusal, whether it
    // exists, is a file or a folder, or passes through a missing folder.
    let (base, root) = sandbox("root-oracle");
    let mut a = auto(&root);
    let refusal = |p: &str| ToolError::Failed(format!("{p} is outside the allowed directory {}", root.display()));
    let abs = |rel: &str| base.join(rel).to_str().unwrap().to_owned();

    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut reads = vec![
        "../outside/secret.pdf".to_owned(),
        "../outside/nope.pdf".into(),
        "../outside/secret.pdf/x".into(),
        "../outside/sub".into(),
        "../outside/sub/x".into(),
        "../nowhere/at/all.pdf".into(),
        "missing/../../outside/secret.pdf".into(),
        "missing/../../outside/nope.pdf".into(),
        "../../../../../../../../../../../../../../../../../../../../../../../../x.pdf".into(),
        abs("outside/secret.pdf"),
        abs("outside/nope.pdf"),
        abs("outside/sub/x"),
        abs("nowhere/x.pdf"),
        abs("nowhere/../outside/nope.pdf"),
        abs("outside/../outside/secret.pdf"),
        abs("root/../outside/secret.pdf"),
    ];
    #[cfg(windows)]
    {
        reads.extend([
            r"..\outside\secret.pdf".to_owned(),
            r"..\outside/nope.pdf".into(),
            "../outside/secret.pdf.".into(),
            "../outside/secret.pdf ".into(),
            // Another network share or device namespace is refused by name, without contacting
            // it (`.invalid` never resolves, so a regression fails instead of reaching a host).
            r"\\pdfcraft-test.invalid\share\secret.pdf".into(),
            "//pdfcraft-test.invalid/share/secret.pdf".into(),
            r"\\?\UNC\pdfcraft-test.invalid\share\secret.pdf".into(),
            r"\\.\pipe\pdfcraft-test".into(),
            r"\\?\GLOBALROOT\Device\Null".into(),
        ]);
        let other = base.join("outside/secret.pdf").canonicalize().unwrap();
        reads.push(other.to_str().unwrap().to_owned()); // the verbatim \\?\C:\… form
        if let Some(drive) = (b'D'..=b'Z').rev().map(|d| format!("{}:\\", d as char)).find(|d| !Path::new(d).exists()) {
            reads.push(format!("{drive}secret.pdf")); // a drive that doesn't exist
        }
    }
    for p in &reads {
        assert_eq!(a.call("doc_open", &json!({ "path": p })).unwrap_err(), refusal(p), "reading {p}");
    }

    let writes = [
        "../outside/sub/../y.pdf".to_owned(),
        "../outside/nosub/../y.pdf".into(),
        "../outside/y.pdf".into(),
        "../outside/nosub/y.pdf".into(),
        "../outside/secret.pdf".into(),
        "../outside/secret.pdf/y.pdf".into(),
        "new/../../escape.pdf".into(),
        abs("outside/nosub/deeper/y.pdf"),
    ];
    let doc = ok(&mut a, "doc_open", json!({ "path": "inside.pdf" }))["doc"].as_u64().unwrap();
    for p in &writes {
        assert_eq!(a.call("doc_save", &json!({ "doc": doc, "path": p })).unwrap_err(), refusal(p), "writing {p}");
    }
    #[cfg(windows)]
    {
        // In a verbatim path `/` is not a separator, so `x/../..` can't climb out of it either.
        for tail in [r"\x/../../outside/v.pdf", r"\x/../../outside/secret.pdf"] {
            let p = format!("{}{tail}", root.display());
            if let Ok(c) = a.call("doc_save", &json!({ "doc": doc, "path": p })) {
                let Content::Json(v) = &c[0] else { panic!("{p}: expected JSON") };
                assert!(Path::new(v["path"].as_str().unwrap()).starts_with(&root), "{p} wrote {v}");
            }
            // (What lands outside the root, if anything, is checked below.)
        }
    }

    // A link inside the root that leads out of it is refused the same way, below it too.
    link_dir(&base.join("root"), "link", &base.join("outside"));
    for p in ["link/secret.pdf", "link/nope.pdf", "link/sub/x", "link"] {
        assert_eq!(a.call("doc_open", &json!({ "path": p })).unwrap_err(), refusal(p), "reading {p}");
    }
    for p in ["link/new.pdf", "link/nosub/new.pdf", "link/secret.pdf"] {
        assert_eq!(a.call("doc_save", &json!({ "doc": doc, "path": p })).unwrap_err(), refusal(p), "writing {p}");
    }
    // So is a link whose target is gone: whether a link's target exists stays hidden too.
    std::fs::create_dir_all(base.join("outside/gone")).unwrap();
    link_dir(&base.join("root"), "broken", &base.join("outside/gone"));
    std::fs::remove_dir(base.join("outside/gone")).unwrap();
    for p in ["broken", "broken/x.pdf", "broken/x/y.pdf"] {
        assert_eq!(a.call("doc_open", &json!({ "path": p })).unwrap_err(), refusal(p), "reading {p}");
        assert_eq!(a.call("doc_save", &json!({ "doc": doc, "path": p })).unwrap_err(), refusal(p), "writing {p}");
    }

    // Nothing was written outside the root, and the outside files are untouched.
    let mut left: Vec<String> =
        std::fs::read_dir(base.join("outside")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, ["secret.pdf", "sub"]);
    assert_eq!(std::fs::read(base.join("outside/secret.pdf")).unwrap(), fixture(1));
    assert!(!base.join("escape.pdf").exists() && !base.join("y.pdf").exists());

    // Inside the root nothing changes: missing files say so, and existing ones open and save,
    // however the path is spelled.
    let missing = a.call("doc_open", &json!({ "path": "missing-inside.pdf" })).unwrap_err();
    assert!(matches!(&missing, ToolError::Failed(m) if m.starts_with("missing-inside.pdf: ") && !m.contains("outside")), "{missing:?}");
    let not_dir = a.call("doc_open", &json!({ "path": "inside.pdf/x" })).unwrap_err();
    assert!(matches!(&not_dir, ToolError::Failed(m) if !m.contains("outside")), "{not_dir:?}");
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut opens = vec![
        "inside.pdf".to_owned(),
        "./inside.pdf".into(),
        "sub/../inside.pdf".into(),
        "../root/inside.pdf".into(),
        "../outside/../root/inside.pdf".into(),
        "../nowhere/../root/inside.pdf".into(),
        abs("root/inside.pdf"),
        abs("outside/../root/inside.pdf"),
        abs("nowhere/../root/inside.pdf"),
        root.join("inside.pdf").to_str().unwrap().to_owned(),
    ];
    #[cfg(windows)]
    {
        let plain = abs("root/inside.pdf");
        opens.extend([plain.to_lowercase(), plain.to_uppercase(), r"..\root\inside.pdf".into()]);
    }
    for p in &opens {
        ok(&mut a, "doc_open", json!({ "path": p }));
    }
    for p in ["new.pdf", "fresh/dir/new.pdf", "fresh/../also-new.pdf"] {
        ok(&mut a, "doc_save", json!({ "doc": doc, "path": p }));
    }
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": abs("root/abs-new.pdf") }));
    for f in ["new.pdf", "fresh/dir/new.pdf", "also-new.pdf", "abs-new.pdf"] {
        assert!(root.join(f).is_file(), "{f} was written inside the root");
    }

    // Without a root, paths are used as given.
    let mut free = Automation::new();
    ok(&mut free, "doc_open", json!({ "path": abs("outside/secret.pdf") }));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn writing_to_a_folder_touches_nothing_beside_it() {
    // "." names the root itself. Saving there used to stage its temporary file next to the
    // root, outside it, overwriting and then deleting any file of that name. Staging names are
    // random now and a failed rename removes the staging file, so the "is a folder" refusal is
    // what this checks; the listings and the file at the old staging name are canaries.
    let (base, root) = sandbox("root-itself");
    let mut a = auto(&root);
    let beside = base.join(".root.pdfcraft-tmp");
    std::fs::write(&beside, "SENTINEL").unwrap();
    std::fs::create_dir_all(root.join("folder")).unwrap();
    let listing = |dir: &Path| {
        let mut names: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        names.sort();
        names
    };
    let (base_before, folder_before) = (listing(&base), listing(&root.join("folder")));
    let doc = ok(&mut a, "doc_open", json!({ "path": "inside.pdf" }))["doc"].as_u64().unwrap();
    for p in [".", "", "folder", "folder/"] {
        let e = a.call("doc_save", &json!({ "doc": doc, "path": p })).unwrap_err();
        assert!(matches!(&e, ToolError::Failed(m) if m.contains("is a folder")), "{p:?}: {e:?}");
    }
    let png = vec![1, 2, 3];
    assert!(a.write_output(".", &png).is_err());
    assert_eq!(std::fs::read_to_string(&beside).unwrap(), "SENTINEL");
    assert_eq!(listing(&base), base_before, "nothing was left beside the root");
    assert_eq!(listing(&root.join("folder")), folder_before, "nothing was left in the folder");
    // `image_save` adds an extension when the path has none, which turned "." into `root.png`
    // beside the root.
    ok(&mut a, "doc_export_images", json!({ "doc": doc, "folder": "src", "dpi": 18 }));
    let pic = ok(&mut a, "doc_create", json!({ "from": "images", "paths": ["src/inside_page_1.png"] }))["doc"].as_u64().unwrap();
    for p in [".", "", "folder", "src/.."] {
        let e = a.call("image_save", &json!({ "doc": pic, "page": 1, "image": 1, "path": p })).unwrap_err();
        assert!(matches!(&e, ToolError::Failed(m) if m.contains("is a folder")), "{p:?}: {e:?}");
    }
    assert!(!base.join("root.png").exists() && !root.join("folder.png").exists());
    ok(&mut a, "image_save", json!({ "doc": pic, "page": 1, "image": 1, "path": "folder/picture" }));
    assert!(root.join("folder/picture.png").is_file());
    // Folder outputs may still name the root.
    ok(&mut a, "doc_split", json!({ "doc": doc, "every": 1, "out_dir": "." }));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn file_names_from_documents_stay_in_the_output_folder() {
    // Folder outputs name their files after the document. A document name with separators,
    // `..` or (on Windows) a drive letter used to take those files out of the folder, and out
    // of the root.
    let (base, root) = sandbox("doc-names");
    let mut a = auto(&root);
    let names = ["../../escape", "../../escape.pdf", "x/../../../escape.pdf", "/tmp/escape.pdf", r"x\C:escape.pdf", "C:escape.pdf", "..", "."];
    for (i, name) in names.iter().enumerate() {
        let doc = ok(&mut a, "doc_create", json!({ "from": "blank", "pages": 2, "name": name }))["doc"].as_u64().unwrap();
        let out = format!("out{i}");
        let mut files: Vec<String> = Vec::new();
        let r = ok(&mut a, "doc_export_images", json!({ "doc": doc, "folder": out, "dpi": 10 }));
        files.extend(r["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_owned()));
        let r = ok(&mut a, "page_extract", json!({ "doc": doc, "pages": [1], "separate": true, "out_dir": out }));
        files.extend(r["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_owned()));
        let r = ok(&mut a, "doc_split", json!({ "doc": doc, "every": 1, "out_dir": out }));
        files.extend(r["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap().to_owned()));
        assert_eq!(files.len(), 5, "{name:?}: {files:?}");
        for f in &files {
            let f = Path::new(f);
            assert_eq!(f.parent(), Some(root.join(&out).as_path()), "{name:?} wrote {}", f.display());
            assert!(f.is_file(), "{name:?}: {} exists", f.display());
        }
    }
    // The same for the images a page uses, with a document that has one.
    let doc = ok(&mut a, "doc_open", json!({ "path": "inside.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "doc_export_images", json!({ "doc": doc, "folder": "src", "dpi": 18 }));
    let pic =
        ok(&mut a, "doc_create", json!({ "from": "images", "paths": ["src/inside_page_1.png"], "name": "../../escape" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "doc_export_all_images", json!({ "doc": pic, "folder": "all" }));
    let files = r["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "{r}");
    for f in files {
        assert_eq!(Path::new(f["path"].as_str().unwrap()).parent(), Some(root.join("all").as_path()), "{r}");
    }
    let mut left: Vec<String> = std::fs::read_dir(&base).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, ["outside", "root"]);

    // The root itself must be a folder.
    assert!(Automation::new().with_root(root.join("inside.pdf")).is_err());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn command_list_reports_enablement_and_tools() {
    let dir = workdir("commands");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let list = ok(&mut a, "command_list", json!({ "doc": doc }));
    let find = |id: &str| list["commands"].as_array().unwrap().iter().find(|c| c["id"] == id).unwrap().clone();
    assert_eq!(find("page.rotate")["tool"], "page_rotate");
    assert_eq!(find("page.rotate")["enabled"], true);
    assert_eq!(find("edit.undo")["enabled"], false);
    ok(&mut a, "page_rotate", json!({ "doc": doc, "pages": [1], "degrees": 90 }));
    let list = ok(&mut a, "command_list", json!({ "doc": doc }));
    let undo = list["commands"].as_array().unwrap().iter().find(|c| c["id"] == "edit.undo").unwrap().clone();
    assert_eq!(undo["enabled"], true);
    assert_eq!(undo["label"], "Undo Rotate page");
}

// ---- MCP ---------------------------------------------------------------------------------------

#[cfg(feature = "mcp")]
fn rpc(server: &mut McpServer, id: u64, method: &str, params: Value) -> Value {
    let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
    serde_json::from_str(&server.handle_line(&line).expect("a reply")).unwrap()
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_session_over_stdio() {
    let dir = workdir("mcp");
    let path = dir.join("a.pdf");
    let input = [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "doc_open", "arguments": { "path": path } } }),
        json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "page_render", "arguments": { "doc": 1, "page": 2, "dpi": 36 } } }),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    let mut out = Vec::new();
    McpServer::new(Automation::new()).serve(input.as_bytes(), &mut out).unwrap();
    let replies: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(replies.len(), 4, "the notification gets no reply");

    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "pdfcraft");
    assert_eq!(replies[1]["result"]["tools"].as_array().unwrap().len(), tools().len());
    assert_eq!(replies[2]["result"]["structuredContent"]["pages"], 3);
    assert_eq!(replies[2]["result"]["isError"], false);
    let img = &replies[3]["result"]["content"][0];
    assert_eq!((img["type"].as_str(), img["mimeType"].as_str()), (Some("image"), Some("image/png")));
    use base64::Engine as _;
    let png = base64::engine::general_purpose::STANDARD.decode(img["data"].as_str().unwrap()).unwrap();
    assert_eq!(&png[1..4], b"PNG");
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_errors() {
    let mut s = McpServer::new(Automation::new());
    assert_eq!(rpc(&mut s, 1, "initialize", json!({ "protocolVersion": "1999-01-01" }))["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(rpc(&mut s, 2, "ping", json!({}))["result"], json!({}));
    assert_eq!(rpc(&mut s, 3, "prompts/list", json!({}))["error"]["code"], -32601);
    assert_eq!(rpc(&mut s, 6, "resources/read", json!({ "uri": "pdfcraft://doc/9/info" }))["error"]["code"], -32602);
    assert_eq!(rpc(&mut s, 4, "tools/call", json!({ "name": "nope" }))["error"]["code"], -32602);
    let failed = rpc(&mut s, 5, "tools/call", json!({ "name": "doc_open", "arguments": { "path": "/definitely/not/here.pdf" } }));
    assert_eq!(failed["result"]["isError"], true);
    assert!(failed["result"]["content"][0]["text"].as_str().unwrap().contains("here.pdf"));
    let bad: Value = serde_json::from_str(&s.handle_line("{not json").unwrap()).unwrap();
    assert_eq!(bad["error"]["code"], -32700);
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_resources_expose_open_documents() {
    let dir = workdir("mcp-resources");
    let mut s = McpServer::new(auto(&dir));
    assert!(rpc(&mut s, 1, "initialize", json!({}))["result"]["capabilities"]["resources"].is_object());
    assert_eq!(rpc(&mut s, 2, "resources/list", json!({}))["result"]["resources"].as_array().unwrap().len(), 2);
    assert_eq!(rpc(&mut s, 3, "resources/templates/list", json!({}))["result"]["resourceTemplates"].as_array().unwrap().len(), 4);
    rpc(&mut s, 4, "tools/call", json!({ "name": "doc_open", "arguments": { "path": "a.pdf" } }));
    let list = rpc(&mut s, 5, "resources/list", json!({}))["result"]["resources"].as_array().cloned().unwrap();
    assert_eq!(list.len(), 2 + 3 + 2, "info, text, three page images and two session resources");
    assert_eq!(list[0]["uri"], "pdfcraft://doc/1/info");
    let read = |s: &mut McpServer, uri: &str| rpc(s, 6, "resources/read", json!({ "uri": uri }))["result"]["contents"][0].clone();
    let text = read(&mut s, "pdfcraft://doc/1/text");
    assert_eq!(text["mimeType"], "text/plain");
    assert!(text["text"].as_str().unwrap().contains("Page 2\nPage 2"), "{text}");
    assert_eq!(read(&mut s, "pdfcraft://doc/1/page/3/text")["text"], "Page 3");
    let info: Value = serde_json::from_str(read(&mut s, "pdfcraft://doc/1/info")["text"].as_str().unwrap()).unwrap();
    assert_eq!(info["pages"].as_array().unwrap().len(), 3);
    let img = read(&mut s, "pdfcraft://doc/1/page/1/image?dpi=36");
    use base64::Engine as _;
    let png = base64::engine::general_purpose::STANDARD.decode(img["blob"].as_str().unwrap()).unwrap();
    assert_eq!(&png[1..4], b"PNG");
    assert_eq!(rpc(&mut s, 7, "resources/read", json!({ "uri": "pdfcraft://doc/1/page/9/image" }))["error"]["code"], -32602);
}

/// A server in compact mode with `dir` as its root.
#[cfg(feature = "mcp")]
fn compact_server(dir: &Path) -> McpServer {
    McpServer::new(auto(dir)).with_compact(true)
}

/// Run a tool through the compact server's `tool_call`.
#[cfg(feature = "mcp")]
fn via_tool_call(s: &mut McpServer, id: u64, tool: &str, arguments: Value) -> Value {
    rpc(s, id, "tools/call", json!({ "name": "tool_call", "arguments": { "name": tool, "arguments": arguments } }))
}

#[cfg(feature = "mcp")]
fn tool_names(reply: &Value) -> Vec<String> {
    reply["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_default_lists_every_tool_and_has_no_meta_tools() {
    let mut s = McpServer::new(Automation::new());
    let names = tool_names(&rpc(&mut s, 1, "tools/list", json!({})));
    assert_eq!(names.len(), tools().len());
    assert!(!names.iter().any(|n| n == "tool_search" || n == "tool_call"));
    assert!(!rpc(&mut s, 2, "initialize", json!({}))["result"]["instructions"].as_str().unwrap().contains("tool_search"));
    // Without --compact the meta tools do not exist.
    assert_eq!(rpc(&mut s, 3, "tools/call", json!({ "name": "tool_search" }))["error"]["code"], -32602);
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_compact_lists_the_core_tools_and_two_meta_tools() {
    let mut s = McpServer::new(Automation::new()).with_compact(true);
    let list = rpc(&mut s, 1, "tools/list", json!({}));
    let mut expected: Vec<String> = pdfcraft_automation::mcp::COMPACT_CORE_TOOLS.iter().map(|n| n.to_string()).collect();
    expected.extend(["tool_search".to_string(), "tool_call".to_string()]);
    assert_eq!(tool_names(&list), expected);
    let all: Vec<&str> = tools().iter().map(|t| t.name).collect();
    for core in pdfcraft_automation::mcp::COMPACT_CORE_TOOLS {
        assert!(all.contains(core), "core tool {core} is not in the tool table");
    }
    // Core tools are the real definitions, and the list is far smaller than the full one.
    let full = rpc(&mut McpServer::new(Automation::new()), 2, "tools/list", json!({}));
    let core_open = list["result"]["tools"][0].clone();
    assert_eq!(Some(&core_open), full["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "doc_open"));
    assert!(list.to_string().len() * 5 < full.to_string().len(), "compact {} vs full {}", list.to_string().len(), full.to_string().len());
    for meta in &list["result"]["tools"].as_array().unwrap()[pdfcraft_automation::mcp::COMPACT_CORE_TOOLS.len()..] {
        assert_eq!(meta["inputSchema"]["type"], "object");
    }
    let instructions = rpc(&mut s, 3, "initialize", json!({}))["result"]["instructions"].as_str().unwrap().to_string();
    assert!(instructions.contains("tool_search") && instructions.contains("tool_call"), "{instructions}");
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_compact_tool_search_filters_by_query_and_category() {
    let mut s = McpServer::new(Automation::new()).with_compact(true);
    let mut search = |args: Value| rpc(&mut s, 1, "tools/call", json!({ "name": "tool_search", "arguments": args }));

    let everything = search(json!({}));
    assert_eq!(everything["result"]["isError"], false);
    assert_eq!(everything["result"]["structuredContent"]["count"], tools().len());
    let first = &everything["result"]["structuredContent"]["tools"][0];
    assert!(first["name"].is_string() && first["description"].is_string() && first["read_only"].is_boolean(), "{first}");

    let rotate = search(json!({ "query": "ROTATE" }))["result"]["structuredContent"].clone();
    let names: Vec<&str> = rotate["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"page_rotate"), "{names:?}");
    assert!(names.len() < tools().len());

    let docs = search(json!({ "category": "doc" }))["result"]["structuredContent"].clone();
    let expected = tools().iter().filter(|t| t.name.starts_with("doc_")).count();
    assert!(expected > 0);
    assert_eq!(docs["count"], expected);
    assert!(docs["tools"].as_array().unwrap().iter().all(|t| t["name"].as_str().unwrap().starts_with("doc_") && t["category"] == "doc"));

    let both = search(json!({ "category": "page", "query": "rotate" }))["result"]["structuredContent"].clone();
    assert!(both["tools"].as_array().unwrap().iter().all(|t| t["name"].as_str().unwrap().starts_with("page_")));
    assert!(both["tools"].as_array().unwrap().iter().any(|t| t["name"] == "page_rotate"));

    // No match is an empty result that lists the categories, not an error.
    let none = search(json!({ "category": "nonsense" }));
    assert_eq!(none["result"]["isError"], false);
    assert_eq!(none["result"]["structuredContent"]["count"], 0);
    assert!(none["result"]["structuredContent"]["categories"].as_array().unwrap().iter().any(|c| c == "page"));

    // One tool in full.
    let one = search(json!({ "name": "page_rotate" }))["result"]["structuredContent"]["tool"].clone();
    assert_eq!(one["name"], "page_rotate");
    assert_eq!(one["input_schema"]["type"], "object");
    let def = tools().into_iter().find(|t| t.name == "page_rotate").unwrap();
    assert_eq!(one["input_schema"], def.input_schema);
    assert_eq!(one["description"], def.description);

    // Bad arguments are tool errors.
    for bad in [json!({ "name": "no_such_tool" }), json!({ "query": 3 }), json!({ "category": ["doc"] }), json!("doc")] {
        let r = search(bad.clone());
        assert_eq!(r["result"]["isError"], true, "{bad}");
        assert!(r["result"]["content"][0]["text"].as_str().is_some_and(|t| !t.is_empty()));
    }
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_compact_tool_call_dispatches_to_any_tool() {
    let dir = workdir("mcp-compact");
    let mut s = compact_server(&dir);
    let opened = via_tool_call(&mut s, 1, "doc_open", json!({ "path": "a.pdf" }));
    assert_eq!(opened["result"]["isError"], false);
    assert_eq!(opened["result"]["structuredContent"]["pages"], 3);

    // A tool outside the core list, through the meta tool.
    let deleted = via_tool_call(&mut s, 2, "page_delete", json!({ "doc": 1, "pages": [3] }));
    assert_eq!(deleted["result"]["isError"], false, "{deleted}");
    assert_eq!(deleted["result"]["structuredContent"]["pages"], 2);

    // The same tool called by name directly still works, and sees the same session.
    let listed = rpc(&mut s, 3, "tools/call", json!({ "name": "doc_list", "arguments": {} }));
    assert_eq!(listed["result"]["isError"], false);
    assert_eq!(listed["result"]["structuredContent"]["documents"][0]["pages"], 2);

    // Images come back as images.
    let png = via_tool_call(&mut s, 4, "page_render", json!({ "doc": 1, "page": 1, "dpi": 36 }));
    assert_eq!(png["result"]["content"][0]["type"], "image");

    // `arguments` may be left out for tools that need none.
    assert_eq!(via_tool_call(&mut s, 5, "doc_list", Value::Null)["result"]["isError"], false);
    let no_args = rpc(&mut s, 6, "tools/call", json!({ "name": "tool_call", "arguments": { "name": "doc_list" } }));
    assert_eq!(no_args["result"]["isError"], false);

    // The result equals what the direct call returns.
    let direct = rpc(&mut s, 7, "tools/call", json!({ "name": "doc_info", "arguments": { "doc": 1 } }));
    let wrapped = via_tool_call(&mut s, 8, "doc_info", json!({ "doc": 1 }));
    assert_eq!(direct["result"], wrapped["result"]);
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_compact_tool_call_reports_errors_without_panicking() {
    let dir = workdir("mcp-compact-errors");
    let mut s = compact_server(&dir);
    let text = |r: &Value| r["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string();

    let unknown = via_tool_call(&mut s, 1, "no_such_tool", json!({}));
    assert_eq!(unknown["result"]["isError"], true);
    assert!(text(&unknown).contains("no_such_tool") && text(&unknown).contains("tool_search"), "{}", text(&unknown));

    // The tool's own validation errors pass through.
    let missing = via_tool_call(&mut s, 2, "doc_open", json!({}));
    assert_eq!(missing["result"]["isError"], true);
    let outside = via_tool_call(&mut s, 3, "doc_open", json!({ "path": "/definitely/not/here.pdf" }));
    assert_eq!(outside["result"]["isError"], true);
    let wrong_type = via_tool_call(&mut s, 4, "page_delete", json!({ "doc": "one", "pages": "all" }));
    assert_eq!(wrong_type["result"]["isError"], true);

    // Malformed meta-tool arguments.
    for args in [
        json!({}),
        json!({ "name": 7 }),
        json!({ "name": "doc_list", "arguments": [1] }),
        json!("doc_list"),
        Value::Null,
        json!({ "name": "tool_call" }),
        json!({ "name": "tool_search" }),
    ] {
        let r = rpc(&mut s, 5, "tools/call", json!({ "name": "tool_call", "arguments": args }));
        assert_eq!(r["result"]["isError"], true, "{args}");
        assert!(!text(&r).is_empty());
    }

    // Direct calls keep the usual protocol error for an unknown tool, and the session survives all of it.
    assert_eq!(rpc(&mut s, 6, "tools/call", json!({ "name": "nope" }))["error"]["code"], -32602);
    assert_eq!(via_tool_call(&mut s, 7, "doc_open", json!({ "path": "a.pdf" }))["result"]["isError"], false);
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_compact_tool_search_summarizes_the_first_sentence() {
    let mut s = McpServer::new(Automation::new()).with_compact(true);
    let mut summary = |tool: &str| {
        let found = rpc(
            &mut s,
            1,
            "tools/call",
            json!({ "name": "tool_search", "arguments": { "query": tool, "category": tool.split('_').next().unwrap() } }),
        );
        found["result"]["structuredContent"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == tool).unwrap()["description"]
            .as_str()
            .unwrap()
            .to_string()
    };
    // Abbreviations such as "e.g." do not end the summary.
    assert_eq!(summary("page_number"), "Label a range of pages (e.g. i, ii, iii for front matter, or A-1, A-2 for an appendix).");
    assert_eq!(
        summary("accessibility_fix"),
        "Apply the checker's automatic fix for a rule: primary-language (value: the language, e.g. en-US), title (value: the title; default the current title or file name; also shows it in the title bar) or tab-order (every page tabs in structure order)."
    );
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_compact_meta_tools_reject_unknown_arguments() {
    let dir = workdir("mcp-compact-keys");
    let mut s = compact_server(&dir);
    let list = rpc(&mut s, 1, "tools/list", json!({}));
    let metas = &list["result"]["tools"].as_array().unwrap()[pdfcraft_automation::mcp::COMPACT_CORE_TOOLS.len()..];
    assert_eq!(metas.len(), 2);
    for meta in metas {
        assert_eq!(meta["inputSchema"]["additionalProperties"], false, "{}", meta["name"]);
    }
    let text = |r: &Value| r["error"]["message"].as_str().or_else(|| r["result"]["content"][0]["text"].as_str()).unwrap_or_default().to_string();

    // A misspelled filter is an error, not an unfiltered listing.
    let typo = rpc(&mut s, 2, "tools/call", json!({ "name": "tool_search", "arguments": { "qurey": "rotate" } }));
    assert_eq!(typo["error"]["code"], -32602, "{typo}");
    assert!(text(&typo).contains("\"qurey\"") && text(&typo).contains("query"), "{}", text(&typo));
    let extra = rpc(&mut s, 3, "tools/call", json!({ "name": "tool_search", "arguments": { "query": "rotate", "extra": 1 } }));
    assert_eq!(extra["error"]["code"], -32602, "{extra}");
    assert!(text(&extra).contains("\"extra\""), "{}", text(&extra));

    // tool_call rejects extra outer keys before running anything.
    let misspelled =
        rpc(&mut s, 4, "tools/call", json!({ "name": "tool_call", "arguments": { "name": "doc_open", "argumentz": { "path": "a.pdf" } } }));
    assert_eq!(misspelled["error"]["code"], -32602, "{misspelled}");
    assert!(text(&misspelled).contains("\"argumentz\""), "{}", text(&misspelled));
    let extra = rpc(&mut s, 5, "tools/call", json!({ "name": "tool_call", "arguments": { "name": "doc_list", "unexpected": "value" } }));
    assert_eq!(extra["error"]["code"], -32602, "{extra}");
    assert!(text(&extra).contains("\"unexpected\""), "{}", text(&extra));
    assert_eq!(rpc(&mut s, 6, "tools/call", json!({ "name": "doc_list", "arguments": {} }))["result"]["structuredContent"]["documents"], json!([]));

    // Keys inside "arguments" are left to the target tool, which rejects its unknown ones itself.
    let inner = via_tool_call(&mut s, 7, "doc_list", json!({ "unexpected": "value" }));
    assert_eq!(inner["result"]["isError"], true, "{inner}");
    assert!(text(&inner).contains("doc_list: unknown argument \"unexpected\""), "{}", text(&inner));
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_compact_tool_call_keeps_the_root_confinement() {
    let root = workdir("mcp-compact-root");
    let outside_dir = workdir("mcp-compact-outside");
    let outside = outside_dir.join("a.pdf").to_string_lossy().into_owned();
    let text = |r: &Value| r["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string();

    // Direct call and tool_call both refuse a real PDF outside the root, with the same words.
    let mut confined = compact_server(&root);
    let direct = rpc(&mut confined, 1, "tools/call", json!({ "name": "doc_open", "arguments": { "path": outside } }));
    assert_eq!(direct["result"]["isError"], true, "{direct}");
    assert!(text(&direct).contains("is outside the allowed directory"), "{}", text(&direct));
    let wrapped = via_tool_call(&mut confined, 2, "doc_open", json!({ "path": outside }));
    assert_eq!(wrapped["result"]["isError"], true, "{wrapped}");
    assert_eq!(text(&wrapped), text(&direct));
    // Nothing was opened.
    let listed = rpc(&mut confined, 3, "tools/call", json!({ "name": "doc_list", "arguments": {} }));
    assert_eq!(listed["result"]["structuredContent"]["documents"], json!([]));

    // The same file opens through tool_call on a server without a root, so the refusal came from the root.
    let mut open = McpServer::new(Automation::new()).with_compact(true);
    let opened = via_tool_call(&mut open, 4, "doc_open", json!({ "path": outside }));
    assert_eq!(opened["result"]["isError"], false, "{opened}");
    assert_eq!(opened["result"]["structuredContent"]["pages"], 3);

    // And a file inside the root still opens through the confined server.
    assert_eq!(via_tool_call(&mut confined, 5, "doc_open", json!({ "path": "a.pdf" }))["result"]["isError"], false);
}

#[test]
fn parallel_text_extraction_keeps_page_order_and_follows_edits() {
    let dir = workdir("parallel");
    std::fs::write(dir.join("long.pdf"), fixture(40)).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "long.pdf" }))["doc"].as_u64().unwrap();
    let expected: Vec<String> = (1..=40).map(|i| format!("Page {i}")).collect();
    assert_eq!(page_text(&mut a, doc), expected);
    assert_eq!(ok(&mut a, "text_find", json!({ "doc": doc, "query": "page 3" }))["count"], 11); // 3, 30–39
    ok(&mut a, "page_delete", json!({ "doc": doc, "pages": [3] }));
    assert_eq!(ok(&mut a, "text_find", json!({ "doc": doc, "query": "page 3" }))["count"], 10);
    assert_eq!(page_text(&mut a, doc)[2], "Page 4");
}

#[test]
fn bookmarks_through_tools() {
    let dir = workdir("bookmarks");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "bookmark_list", json!({ "doc": doc }))["bookmarks"], json!([]));
    ok(&mut a, "bookmark_add", json!({ "doc": doc, "title": "Intro", "page": 1 }));
    ok(&mut a, "bookmark_add", json!({ "doc": doc, "title": "Body", "page": 2 }));
    ok(&mut a, "bookmark_add", json!({ "doc": doc, "title": "Detail", "page": 3, "parent": [2] }));
    ok(&mut a, "bookmark_move", json!({ "doc": doc, "path": [1], "parent": [2], "position": 1 })); // Intro under Body
    ok(&mut a, "bookmark_rename", json!({ "doc": doc, "path": [1, 2], "title": "Details" }));
    ok(&mut a, "bookmark_set_page", json!({ "doc": doc, "path": [1, 1], "page": 3 }));
    let list = ok(&mut a, "bookmark_list", json!({ "doc": doc }))["bookmarks"].clone();
    assert_eq!(list[0]["title"], "Body");
    assert_eq!(list[0]["children"][0]["title"], "Intro");
    assert_eq!(list[0]["children"][0]["page"], 3);
    assert_eq!(list[0]["children"][0]["path"], json!([1, 1]));
    assert_eq!(list[0]["children"][1]["title"], "Details");
    assert!(matches!(a.call("bookmark_delete", &json!({ "doc": doc, "path": [9] })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("bookmark_delete", &json!({ "doc": doc, "path": [0] })), Err(ToolError::InvalidArgs(_))));
    ok(&mut a, "bookmark_delete", json!({ "doc": doc, "path": [1, 1] }));
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "marked.pdf" }));
    let mut b = auto(&dir);
    let re = ok(&mut b, "doc_open", json!({ "path": "marked.pdf" }))["doc"].as_u64().unwrap();
    let list = ok(&mut b, "bookmark_list", json!({ "doc": re }))["bookmarks"].clone();
    assert_eq!((list[0]["title"].as_str(), list[0]["children"][0]["title"].as_str()), (Some("Body"), Some("Details")));
}

#[test]
fn numbering_pages_through_tools() {
    let dir = workdir("labels");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "page_number", json!({ "doc": doc, "from": 1, "to": 1, "style": "upper-roman" }));
    assert_eq!(r["labels"], json!(["I", "2", "3"]));
    let r = ok(&mut a, "page_number", json!({ "doc": doc, "from": 2, "to": 3, "prefix": "B-", "start": 5 }));
    assert_eq!(r["labels"], json!(["I", "B-5", "B-6"]));
    assert!(matches!(a.call("page_number", &json!({ "doc": doc, "from": 2, "to": 4 })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(a.call("page_number", &json!({ "doc": doc, "from": 1, "to": 1, "style": "klingon" })), Err(ToolError::InvalidArgs(_))));
}

#[test]
fn comments_through_tools() {
    let dir = workdir("comments");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    // "Page 2" sits at y = 150 (user space) = 150 from the top of the 300 pt page.
    let hl = ok(&mut a, "comment_add", json!({ "doc": doc, "page": 2, "type": "highlight", "find": "page 2", "contents": "check", "author": "Ada" }));
    assert_eq!(hl["comment"]["lines"], 1);
    let id = hl["comment"]["id"].as_str().unwrap().to_string();
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 2, "type": "note", "at": [150, 20], "contents": "Sticky" }));
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "rectangle", "rect": [10, 10, 60, 40], "color": "blue", "width": 3 }));
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "textbox", "rect": [10, 200, 190, 240], "contents": "Hello", "font_size": 10 }));
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "ink", "strokes": [[[10, 280], [50, 260], [90, 285]]] }));
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 3, "type": "arrow", "from": [10, 10], "to": [100, 100] }));
    ok(&mut a, "comment_reply", json!({ "doc": doc, "id": id, "text": "Looks right", "author": "Bob" }));
    ok(&mut a, "comment_set_status", json!({ "doc": doc, "id": id, "status": "accepted", "author": "Bob" }));

    let list = ok(&mut a, "comment_list", json!({ "doc": doc }));
    assert_eq!(list["count"], 6, "{list}");
    let c = list["comments"].as_array().unwrap().iter().find(|c| c["id"] == id.as_str()).unwrap().clone();
    assert_eq!((c["type"].as_str(), c["author"].as_str(), c["contents"].as_str()), (Some("Highlight"), Some("Ada"), Some("check")));
    assert_eq!(c["status"], "Accepted");
    assert_eq!(c["replies"].as_array().unwrap().len(), 1);
    assert_eq!(c["replies"][0]["contents"], "Looks right");
    // The highlight covers the found text, reported in top-left-origin points.
    let r: Vec<f64> = c["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert!(r[0] >= 15.0 && r[0] <= 25.0 && r[1] > 120.0 && r[3] < 160.0, "{r:?}");
    let rect = list["comments"].as_array().unwrap().iter().find(|c| c["type"] == "Square").unwrap().clone();
    assert_eq!(rect["color"], "#0078D6");
    assert_eq!(rect["rect"], json!([10.0, 10.0, 60.0, 40.0]));
    let note = list["comments"].as_array().unwrap().iter().find(|c| c["type"] == "Text").unwrap().clone();
    assert_eq!(note["rect"], json!([150.0, 20.0, 170.0, 40.0]), "note icon hangs from its top-left point");

    // Edit by page + index; several changes are one undo step.
    let (page, index) = (rect["page"].as_u64().unwrap(), rect["index"].as_u64().unwrap());
    ok(&mut a, "comment_edit", json!({ "doc": doc, "page": page, "index": index, "color": "#FF0000", "move": [5, 5], "contents": "moved" }));
    let list = ok(&mut a, "comment_list", json!({ "doc": doc, "page": 1 }));
    let rect = list["comments"].as_array().unwrap().iter().find(|c| c["type"] == "Square").unwrap().clone();
    assert_eq!((rect["color"].as_str(), rect["contents"].as_str()), (Some("#FF0000"), Some("moved")));
    assert_eq!(rect["rect"], json!([15.0, 15.0, 65.0, 45.0]));
    let undo = ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(undo["undone"], "Edit comment");
    ok(&mut a, "edit_redo", json!({ "doc": doc }));

    // Editing a text box re-fits its rectangle to the new text: the wrap width and top edge
    // stay, the height follows the wrapped lines.
    let tb = list["comments"].as_array().unwrap().iter().find(|c| c["type"] == "FreeText").unwrap().clone();
    let long = "the quick brown fox jumps over the lazy dog ".repeat(3);
    ok(&mut a, "comment_edit", json!({ "doc": doc, "page": tb["page"], "index": tb["index"], "contents": long.trim_end() }));
    let list = ok(&mut a, "comment_list", json!({ "doc": doc, "page": 1 }));
    let tb = list["comments"].as_array().unwrap().iter().find(|c| c["type"] == "FreeText").unwrap().clone();
    let r: Vec<f64> = tb["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert_eq!((r[0], r[1], r[2] - r[0]), (10.0, 200.0, 180.0), "width and top edge stay: {r:?}");
    assert!(r[3] - r[1] > 40.0, "the box grew to fit the wrapped lines: {r:?}");

    // The rendered page shows the rectangle's border.
    let png = a.call("page_render", &json!({ "doc": doc, "page": 1, "dpi": 72 })).unwrap();
    assert!(matches!(png[0], Content::Png { .. }));

    ok(&mut a, "comment_delete", json!({ "doc": doc, "id": id }));
    assert_eq!(ok(&mut a, "comment_list", json!({ "doc": doc }))["count"], 5);
    assert!(matches!(a.call("comment_delete", &json!({ "doc": doc, "id": id })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("comment_add", &json!({ "doc": doc, "page": 1, "type": "highlight", "find": "nowhere" })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("comment_add", &json!({ "doc": doc, "page": 1, "type": "rectangle" })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(
        a.call("comment_add", &json!({ "doc": doc, "page": 1, "type": "note", "at": [1, 1], "color": "mauve" })),
        Err(ToolError::InvalidArgs(_))
    ));
    assert!(matches!(a.call("comment_edit", &json!({ "doc": doc, "page": 1, "index": 1 })), Err(ToolError::InvalidArgs(_))));

    // Comments survive a save and reopen.
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "commented.pdf" }));
    let mut b = auto(&dir);
    let re = ok(&mut b, "doc_open", json!({ "path": "commented.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut b, "comment_list", json!({ "doc": re }))["count"], 5);
}

#[test]
fn protecting_through_tools() {
    let dir = workdir("protect");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc }))["security"]["protected"], false);
    assert!(matches!(a.call("doc_protect", &json!({ "doc": doc })), Err(ToolError::Failed(_))), "a password is required");
    let r = ok(
        &mut a,
        "doc_protect",
        json!({ "doc": doc, "open_password": "open", "permissions_password": "boss", "printing": "low", "changes": "comment-fill-sign" }),
    );
    assert_eq!(r["security"]["pending"], true);
    assert_eq!(
        (r["security"]["printing"].as_str(), r["security"]["annotate"].as_bool(), r["security"]["copy"].as_bool()),
        (Some("low"), Some(true), Some(false))
    );
    assert!(!r.to_string().contains("boss"), "passwords are never echoed");
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "locked.pdf" }));
    // Another session: the open password is required, and the restrictions hold.
    let mut b = auto(&dir);
    assert!(matches!(b.call("doc_open", &json!({ "path": "locked.pdf" })), Err(ToolError::Failed(_))));
    let re = ok(&mut b, "doc_open", json!({ "path": "locked.pdf", "password": "open" }))["doc"].as_u64().unwrap();
    assert!(matches!(b.call("page_delete", &json!({ "doc": re, "pages": [1] })), Err(ToolError::Failed(_))));
    assert!(matches!(b.call("doc_unprotect", &json!({ "doc": re })), Err(ToolError::Failed(_))));
    ok(&mut b, "comment_add", json!({ "doc": re, "page": 1, "type": "note", "at": [10, 10], "contents": "allowed" }));
    // With the permissions password everything is possible, including removing security.
    let owner = ok(&mut b, "doc_open", json!({ "path": "locked.pdf", "password": "boss" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut b, "doc_unprotect", json!({ "doc": owner }))["security"]["protected"], false);
    ok(&mut b, "doc_save", json!({ "doc": owner, "path": "open.pdf" }));
    let mut c = auto(&dir);
    ok(&mut c, "doc_open", json!({ "path": "open.pdf" }));
}

#[test]
fn combine_opens_protected_files_with_their_passwords() {
    let dir = workdir("combine-passwords");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    // Opens with "open"; only "boss" may assemble pages.
    ok(&mut a, "doc_protect", json!({ "doc": doc, "open_password": "open", "permissions_password": "boss", "changes": "none" }));
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "locked.pdf" }));
    let combine = |a: &mut Automation, passwords: Value| {
        a.call("doc_combine", &json!({ "paths": ["locked.pdf", "b.pdf"], "passwords": passwords, "open": true }))
    };
    let err = |r: Result<_, ToolError>| match r {
        Err(ToolError::Failed(m)) => m,
        Err(other) => panic!("expected a failure, got {other:?}"),
        Ok(_) => panic!("expected a failure"),
    };
    assert!(err(combine(&mut a, Value::Null)).contains("password-protected"));
    assert!(err(combine(&mut a, json!(["wrong", null]))).contains("password is wrong"));
    assert!(err(combine(&mut a, json!(["open", null]))).contains("don't allow copying pages"));
    assert!(matches!(combine(&mut a, json!(["boss"])), Err(ToolError::InvalidArgs(_))), "one per path");
    let done = ok(&mut a, "doc_combine", json!({ "paths": ["locked.pdf", "b.pdf"], "passwords": ["boss", null], "open": true }));
    assert!(!done.to_string().contains("boss"), "passwords are never echoed");
    let out = done["document"]["doc"].as_u64().unwrap();
    assert_eq!(page_text(&mut a, out), ["Page 1", "Page 2", "Page 3", "Page 1", "Page 2"]);
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": out }))["security"]["protected"], false, "the result is not encrypted");
}

/// Restrictions exist only behind a permissions password: open_password alone encrypts and
/// restricts nothing, and asking for a restriction without one is refused rather than ignored (#134).
#[test]
fn protecting_with_an_open_password_alone_restricts_nothing() {
    let dir = workdir("protect-open-only");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    for (key, value) in [("copy", json!(false)), ("changes", json!("none")), ("printing", json!("none")), ("accessibility", json!(false))] {
        match a.call("doc_protect", &json!({ "doc": doc, "open_password": "openme", key: value })) {
            Err(ToolError::InvalidArgs(m)) => assert!(m.contains(key) && m.contains("permissions_password"), "{key}: {m}"),
            other => panic!("{key} without permissions_password: {other:?}"),
        }
    }
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc }))["security"]["protected"], false, "a refused call changes nothing");
    let r = ok(&mut a, "doc_protect", json!({ "doc": doc, "open_password": "openme" }));
    assert_eq!(
        (r["security"]["protected"].as_bool(), r["security"]["copy"].as_bool(), r["security"]["modify"].as_bool()),
        (Some(true), Some(true), Some(true))
    );
    assert_eq!(r["security"]["printing"], "high");
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "open-only.pdf" }));
    let mut b = auto(&dir);
    let re = ok(&mut b, "doc_open", json!({ "path": "open-only.pdf", "password": "openme" }))["doc"].as_u64().unwrap();
    let s = ok(&mut b, "doc_info", json!({ "doc": re }))["security"].clone();
    assert_eq!(
        (s["protected"].as_bool(), s["copy"].as_bool(), s["modify"].as_bool(), s["printing"].as_str()),
        (Some(true), Some(true), Some(true), Some("high"))
    );
    ok(&mut b, "page_delete", json!({ "doc": re, "pages": [1] }));
    // The schema says so too.
    let def = tools().into_iter().find(|t| t.name == "doc_protect").unwrap();
    assert!(def.description.contains("permissions_password"), "{}", def.description);
    for key in ["printing", "changes", "copy", "accessibility"] {
        let desc = def.input_schema["properties"][key]["description"].as_str().unwrap();
        assert!(desc.contains("permissions_password"), "{key}: {desc}");
    }
}

#[test]
fn forms_through_tools() {
    let dir = workdir("forms");
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dist/demo/pdfcraft-showcase.pdf");
    if !src.exists() {
        eprintln!("skipped: run `cargo xtask demo-pdf` for the showcase form");
        return;
    }
    std::fs::copy(&src, dir.join("show.pdf")).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "show.pdf" }))["doc"].as_u64().unwrap();
    let list = ok(&mut a, "form_fields", json!({ "doc": doc }));
    let fields = list["fields"].as_array().unwrap().clone();
    assert!(fields.len() >= 5, "{list}");
    let pick = |t: &str| fields.iter().find(|f| f["type"] == t && f["read_only"] == false).cloned();
    let text = pick("text").expect("a text field");
    let mut values = serde_json::Map::new();
    values.insert(text["name"].as_str().unwrap().into(), json!("Filled by an agent"));
    if let Some(cb) = pick("checkbox") {
        values.insert(cb["name"].as_str().unwrap().into(), json!(true));
    }
    if let Some(combo) = pick("combo") {
        values.insert(combo["name"].as_str().unwrap().into(), combo["options"][1]["label"].clone());
    }
    let r = ok(&mut a, "form_fill", json!({ "doc": doc, "values": values }));
    assert_eq!(r["undo"], "Fill in form");
    let after = ok(&mut a, "form_fields", json!({ "doc": doc }));
    let get = |name: &str| after["fields"].as_array().unwrap().iter().find(|f| f["name"] == name).unwrap()["value"].clone();
    assert_eq!(get(text["name"].as_str().unwrap()), "Filled by an agent");
    // The page shows it.
    let page = text["page"].as_u64().unwrap();
    let found = ok(&mut a, "text_find", json!({ "doc": doc, "query": "Filled by an agent" }));
    assert_eq!(found["count"], 1, "rendered on page {page}");
    assert!(matches!(a.call("form_fill", &json!({ "doc": doc, "values": { "no such field": "x" } })), Err(ToolError::Failed(_))));
    ok(&mut a, "form_reset", json!({ "doc": doc }));
    let reset = ok(&mut a, "form_fields", json!({ "doc": doc }));
    let v = reset["fields"].as_array().unwrap().iter().find(|f| f["name"] == text["name"]).unwrap()["value"].clone();
    assert_ne!(v, "Filled by an agent");
}

/// A form whose fields are only page widgets (the `/Fields` list is empty) still lists, fills and
/// resets through the tools, and the leniency shows up as a repair note.
#[test]
fn orphan_form_fields_through_tools() {
    /// One page with two text fields that appear only as page widgets: the AcroForm lists none.
    fn orphan_form() -> Vec<u8> {
        let objs: Vec<&str> = vec![
            "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>",                                     // 1
            "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>",                     // 2
            "<< /Type /Page /Parent 2 0 R /Annots [5 0 R 6 0 R] >>",                                 // 3
            "<< /Fields [] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 7 0 R >> >> >>",               // 4
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (alpha) /Rect [10 200 90 220] /P 3 0 R >>", // 5
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (beta) /Rect [10 150 90 170] /P 3 0 R >>",  // 6
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",                                // 7
        ];
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
    let dir = workdir("orphan-forms");
    std::fs::write(dir.join("orphan.pdf"), orphan_form()).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "orphan.pdf" }))["doc"].as_u64().unwrap();
    let list = ok(&mut a, "form_fields", json!({ "doc": doc }));
    assert_eq!(list["count"], 2, "{list}");
    let r = ok(&mut a, "form_fill", json!({ "doc": doc, "values": { "alpha": "Filled by an agent" } }));
    assert_eq!(r["undo"], "Fill in alpha");
    let after = ok(&mut a, "form_fields", json!({ "doc": doc }));
    let alpha = after["fields"].as_array().unwrap().iter().find(|f| f["name"] == "alpha").unwrap();
    assert_eq!(alpha["value"], "Filled by an agent");
    // The page shows it.
    assert_eq!(ok(&mut a, "text_find", json!({ "doc": doc, "query": "Filled by an agent" }))["count"], 1);
    // The leniency is not silent: the repair note names the adopted fields.
    let info = ok(&mut a, "doc_info", json!({ "doc": doc }));
    assert!(info["repairs"].as_array().unwrap().iter().any(|r| r.as_str().unwrap_or("").contains("page annotations")), "{}", info["repairs"]);
    ok(&mut a, "form_reset", json!({ "doc": doc }));
    let reset = ok(&mut a, "form_fields", json!({ "doc": doc }));
    let v = reset["fields"].as_array().unwrap().iter().find(|f| f["name"] == "alpha").unwrap()["value"].clone();
    assert_ne!(v, "Filled by an agent");
}

#[test]
fn duplicating_and_cropping_through_tools() {
    let dir = workdir("boxes");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "page_duplicate", json!({ "doc": doc, "pages": [2] }));
    assert_eq!(r["pages"], 4);
    assert_eq!(page_text(&mut a, doc), ["Page 1", "Page 2", "Page 2", "Page 3"]);
    // Crop page 1 by margins, page 2 to a rect drawn from the top-left.
    let r = ok(&mut a, "page_set_box", json!({ "doc": doc, "pages": [1], "margins": [10, 20, 30, 40] }));
    assert_eq!(r["page_sizes"][0], json!([160.0, 240.0]));
    let r = ok(&mut a, "page_set_box", json!({ "doc": doc, "pages": [2], "rect": [0, 0, 100, 150] }));
    assert_eq!(r["page_sizes"][1], json!([100.0, 150.0]));
    assert_eq!(page_text(&mut a, doc)[1], "Page 2", "the text at y = 150 is still inside");
    // Reset and errors.
    let r = ok(&mut a, "page_set_box", json!({ "doc": doc, "pages": [1] }));
    assert_eq!(r["page_sizes"][0], json!([200.0, 300.0]));
    assert!(matches!(a.call("page_set_box", &json!({ "doc": doc, "margins": [150, 0, 150, 0] })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("page_set_box", &json!({ "doc": doc, "rect": [0, 0, 10, 10] })), Err(ToolError::InvalidArgs(_))));
}

#[test]
fn headers_watermarks_and_backgrounds_through_tools() {
    let dir = workdir("marks");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "doc_header_footer", json!({ "doc": doc, "footer_center": "<<Page 1 of n>>", "header_right": "ACME" }));
    let texts = page_text(&mut a, doc);
    assert!(texts[2].contains("Page 3 of 3") && texts[0].contains("ACME"), "{texts:?}");
    ok(&mut a, "doc_watermark", json!({ "doc": doc, "pages": [1], "text": "DRAFT", "opacity": 0.2 }));
    assert!(page_text(&mut a, doc)[0].contains("DRAFT"));
    ok(&mut a, "doc_background", json!({ "doc": doc, "color": "yellow" }));
    ok(&mut a, "doc_remove_marks", json!({ "doc": doc, "kind": "watermark" }));
    assert!(!page_text(&mut a, doc)[0].contains("DRAFT"));
    assert!(matches!(a.call("doc_remove_marks", &json!({ "doc": doc, "kind": "watermark" })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("doc_header_footer", &json!({ "doc": doc })), Err(ToolError::Failed(_))), "no text");
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "marked.pdf" }));
}

#[test]
fn exporting_images_and_text_through_tools() {
    let dir = workdir("export");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "doc_export_images", json!({ "doc": doc, "folder": "out", "dpi": 72, "pages": [1, 3] }));
    assert_eq!(r["count"], 2);
    assert!(dir.join("out/a_page_3.png").exists());
    ok(&mut a, "doc_export_images", json!({ "doc": doc, "folder": "out", "dpi": 72, "pages": [2], "format": "jpeg", "quality": 70 }));
    assert!(std::fs::read(dir.join("out/a_page_2.jpg")).unwrap().starts_with(&[0xFF, 0xD8]));
    ok(&mut a, "doc_export_images", json!({ "doc": doc, "folder": "out", "dpi": 72, "pages": [2], "format": "tiff" }));
    let tif = std::fs::read(dir.join("out/a_page_2.tif")).unwrap();
    assert!(tif.starts_with(b"II*\0") || tif.starts_with(b"MM\0*"));
    assert!(matches!(a.call("doc_export_images", &json!({ "doc": doc, "folder": "out", "format": "webp" })), Err(ToolError::InvalidArgs(_))));
    ok(&mut a, "doc_export_text", json!({ "doc": doc, "path": "a.txt" }));
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "Page 1\n\u{c}Page 2\n\u{c}Page 3\n");
    assert!(a.call("doc_export_text", &json!({ "doc": doc, "path": "/etc/x.txt" })).is_err(), "confined to the root");
}

#[test]
fn accessibility_check_report_and_fixes_through_tools() {
    let dir = workdir("a11y");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "accessibility_check", json!({ "doc": doc }));
    assert_eq!(r["results"].as_array().unwrap().len(), 32);
    let status =
        |r: &Value, id: &str| r["results"].as_array().unwrap().iter().find(|x| x["rule"] == id).unwrap()["status"].as_str().unwrap().to_owned();
    for id in ["tagged-pdf", "primary-language", "title", "tagged-content"] {
        assert_eq!(status(&r, id), "failed", "{id}");
    }
    assert_eq!((status(&r, "color-contrast"), status(&r, "scripts")), ("skipped".into(), "manual".into()));
    assert_eq!(status(&ok(&mut a, "accessibility_check", json!({ "doc": doc, "all": true })), "color-contrast"), "manual");
    let docs = ok(&mut a, "accessibility_check", json!({ "doc": doc, "categories": ["document"] }));
    assert_eq!(docs["skipped"], 25, "24 other rules and colour contrast");
    // Fixes.
    assert!(matches!(a.call("accessibility_fix", &json!({ "doc": doc, "rule": "primary-language" })), Err(ToolError::Failed(_))), "needs a language");
    assert_eq!(ok(&mut a, "accessibility_fix", json!({ "doc": doc, "rule": "primary-language", "value": "en-GB" }))["status"], "passed");
    assert_eq!(ok(&mut a, "accessibility_fix", json!({ "doc": doc, "rule": "title" }))["status"], "passed");
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc }))["title"], "a");
    assert_eq!(ok(&mut a, "edit_undo", json!({ "doc": doc }))["undone"], "Set document title");
    assert!(matches!(a.call("accessibility_fix", &json!({ "doc": doc, "rule": "tagged-pdf" })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("accessibility_check", &json!({ "doc": doc, "rules": ["nope"] })), Err(ToolError::InvalidArgs(_))));
    // The report.
    let r = ok(&mut a, "accessibility_report", json!({ "doc": doc, "path": "report.html" }));
    assert_eq!(r["failed"].as_u64(), Some(3), "tagging, tagged content and (undone) title");
    let html = std::fs::read_to_string(dir.join("report.html")).unwrap();
    assert!(html.contains("Accessibility Report") && html.contains("a.pdf"));
    // Figures: list, describe, mark decorative.
    std::fs::write(
        dir.join("figures.pdf"),
        b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 5 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /StructParents 0 >> endobj
4 0 obj << /Length 74 >> stream
/Figure << /MCID 0 >> BDC 10 150 40 20 re f EMC /Figure << /MCID 1 >> BDC 100 100 50 30 re f EMC
endstream endobj
5 0 obj << /Type /StructTreeRoot /K 6 0 R /ParentTree 9 0 R >> endobj
6 0 obj << /S /Document /P 5 0 R /K [7 0 R 8 0 R] >> endobj
7 0 obj << /S /Figure /P 6 0 R /Pg 3 0 R /K 0 >> endobj
8 0 obj << /S /Figure /P 6 0 R /Pg 3 0 R /K 1 >> endobj
9 0 obj << /Nums [0 [7 0 R 8 0 R]] >> endobj
trailer << /Root 1 0 R >>
%%EOF"
            .as_slice(),
    )
    .unwrap();
    let fd = ok(&mut a, "doc_open", json!({ "path": "figures.pdf" }))["doc"].as_u64().unwrap();
    let figs = ok(&mut a, "accessibility_figures", json!({ "doc": fd }));
    assert_eq!(figs["count"], 2);
    assert_eq!(figs["figures"][0]["rect"], json!([10.0, 30.0, 50.0, 50.0]), "top-left-origin points");
    let first = figs["figures"][0]["figure"].as_u64().unwrap();
    let second = figs["figures"][1]["figure"].as_u64().unwrap();
    let r = ok(&mut a, "accessibility_set_alt", json!({ "doc": fd, "figure": first, "alt": "A small bar" }));
    assert_eq!(r["figures"][0]["alt"], "A small bar");
    let r = ok(&mut a, "accessibility_set_alt", json!({ "doc": fd, "figure": second, "decorative": true }));
    assert_eq!(r["count"], 1);
    let check = ok(&mut a, "accessibility_check", json!({ "doc": fd, "rules": ["figures-alt-text", "tagged-content"] }));
    assert_eq!(check["failed"], 0, "{check}");
    assert!(matches!(a.call("accessibility_set_alt", &json!({ "doc": fd, "figure": 6, "alt": "x" })), Err(ToolError::Failed(_))));
}

#[test]
fn editing_existing_text_through_tools() {
    let dir = workdir("edit-text");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let lines = ok(&mut a, "text_lines", json!({ "doc": doc, "page": 2 }));
    assert_eq!((lines["count"].as_u64(), lines["lines"][0]["text"].as_str()), (Some(1), Some("Page 2")));
    let r = ok(&mut a, "text_edit", json!({ "doc": doc, "page": 2, "line": 1, "text": "Section two" }));
    assert_eq!(r["line"]["text"], "Section two");
    assert_eq!(page_text(&mut a, doc)[1], "Section two");
    assert!(matches!(a.call("text_edit", &json!({ "doc": doc, "page": 2, "line": 9, "text": "x" })), Err(ToolError::InvalidArgs(_))));
    assert_eq!(ok(&mut a, "edit_undo", json!({ "doc": doc }))["undone"], "Edit text");
    let paras = ok(&mut a, "text_paragraphs", json!({ "doc": doc, "page": 3 }));
    assert_eq!(paras["paragraphs"][0]["text"], "Page 3");
    let r = ok(&mut a, "text_edit", json!({ "doc": doc, "page": 3, "paragraph": 1, "text": "Part three" }));
    assert_eq!(r["paragraph"]["text"], "Part three");
    assert!(matches!(a.call("text_edit", &json!({ "doc": doc, "page": 3, "paragraph": 4, "text": "x" })), Err(ToolError::InvalidArgs(_))));
    // Formatting only: font, size, colour, alignment.
    ok(
        &mut a,
        "text_edit",
        json!({ "doc": doc, "page": 3, "paragraph": 1, "font": "times", "bold": true, "size": 20, "color": "#cc0000", "align": "center" }),
    );
    let p = &ok(&mut a, "text_paragraphs", json!({ "doc": doc, "page": 3 }))["paragraphs"][0];
    assert_eq!((p["text"].as_str(), p["font"].as_str(), p["size"].as_f64()), (Some("Part three"), Some("Times-Bold"), Some(20.0)));
    // Moved 15 pt right and 10 pt down (dy is up; the listing's rect is measured from the top).
    let rect = |p: &serde_json::Value| -> Vec<f64> { p["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect() };
    let before = rect(p);
    ok(&mut a, "text_edit", json!({ "doc": doc, "page": 3, "paragraph": 1, "dx": 15, "dy": -10 }));
    let p = ok(&mut a, "text_paragraphs", json!({ "doc": doc, "page": 3 }))["paragraphs"][0].clone();
    let after = rect(&p);
    assert!((after[0] - before[0] - 15.0).abs() < 0.5 && (after[1] - before[1] - 10.0).abs() < 0.5, "{before:?} → {after:?}");
    assert_eq!(p["text"], "Part three");
    // A narrow width rewraps "Part three" onto two lines.
    ok(&mut a, "text_edit", json!({ "doc": doc, "page": 3, "paragraph": 1, "width": 50 }));
    let p = &ok(&mut a, "text_paragraphs", json!({ "doc": doc, "page": 3 }))["paragraphs"][0];
    assert_eq!(p["lines"].as_array().map(Vec::len), Some(2), "{p}");
}

/// An original Type 3 subset: A and B are simple rectangle paths, embedded in this fixture.
fn embedded_page_font_pdf() -> Vec<u8> {
    use pdfcraft_cos::{Dict, Document, ObjRef, Object, SaveOptions, Stream, write_incremental};
    let mut doc = Document::open(std::sync::Arc::new(fixture(1))).unwrap();
    let mut procedures = Dict::new();
    for (name, width) in [("A", 600), ("B", 700), ("space", 250)] {
        let path = if name == "space" { "250 0 d0".into() } else { format!("{width} 0 0 0 500 700 d1 0 0 500 700 re f") };
        procedures.set(name.as_bytes().to_vec(), Object::Ref(doc.add(Object::Stream(Stream::flate(Dict::new(), path.as_bytes())))));
    }
    let mut encoding = Dict::new();
    encoding.set(
        b"Differences".to_vec(),
        Object::Array(vec![Object::Int(32), Object::name("space"), Object::Int(65), Object::name("A"), Object::name("B")]),
    );
    let mut descriptor = Dict::new();
    descriptor.set(b"FontName".to_vec(), Object::name("ABCDEF+ContributorShapes-Bold"));
    descriptor.set(b"Flags".to_vec(), Object::Int(262148));
    let descriptor = doc.add(Object::Dict(descriptor));
    let mut font = Dict::new();
    font.set(b"Type".to_vec(), Object::name("Font"));
    font.set(b"Subtype".to_vec(), Object::name("Type3"));
    font.set(b"FontDescriptor".to_vec(), Object::Ref(descriptor));
    font.set(b"FontMatrix".to_vec(), Object::Array([0.001, 0.0, 0.0, 0.001, 0.0, 0.0].map(Object::Real).to_vec()));
    font.set(b"FontBBox".to_vec(), Object::Array([0, 0, 500, 700].map(Object::Int).to_vec()));
    font.set(b"CharProcs".to_vec(), Object::Dict(procedures));
    font.set(b"Encoding".to_vec(), Object::Dict(encoding));
    font.set(b"FirstChar".to_vec(), Object::Int(32));
    font.set(b"LastChar".to_vec(), Object::Int(66));
    font.set(
        b"Widths".to_vec(),
        Object::Array(
            (32..=66)
                .map(|c| {
                    Object::Int(match c {
                        32 => 250,
                        65 => 600,
                        66 => 700,
                        _ => 0,
                    })
                })
                .collect(),
        ),
    );
    let font = doc.add(Object::Dict(font));
    doc.update_dict(ObjRef::new(4, 0), |page| {
        let mut resources = page.get(b"Resources").unwrap().as_dict().unwrap().clone();
        let mut fonts = resources.get(b"Font").unwrap().as_dict().unwrap().clone();
        fonts.set(b"Embedded".to_vec(), Object::Ref(font));
        resources.set(b"Font".to_vec(), Object::Dict(fonts));
        page.set(b"Resources".to_vec(), Object::Dict(resources));
    })
    .unwrap();
    write_incremental(&doc, &SaveOptions::default()).unwrap()
}

#[test]
fn selecting_embedded_page_fonts_round_trips_and_rejects_without_history_changes() {
    let dir = workdir("page-font-choice");
    std::fs::write(dir.join("embedded.pdf"), embedded_page_font_pdf()).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({"path":"embedded.pdf"}))["doc"].as_u64().unwrap();
    let fonts = ok(&mut a, "text_fonts", json!({"doc":doc,"page":1,"text":"AB\n\t BA"}));
    let embedded = fonts["fonts"].as_array().unwrap().iter().find(|f| f["resource"] == "Embedded").unwrap();
    assert_eq!((embedded["subset"].as_bool(), embedded["bold"].as_bool(), embedded["encodable"].as_bool()), (Some(true), Some(true), Some(true)));
    ok(&mut a, "text_edit", json!({"doc":doc,"page":1,"paragraph":1,"text":"AB BA","font_resource":"Embedded"}));
    let before = a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes.clone();
    for args in [
        json!({"text":"ABC","font_resource":"Embedded"}),
        json!({"font_resource":"Missing"}),
        json!({"font_resource":"Embedded","italic":true}),
        json!({"font_resource":"Embedded","bold":false}),
        json!({"font_resource":"Embedded","font":"times"}),
    ] {
        let mut args = args;
        args["doc"] = json!(doc);
        args["page"] = json!(1);
        args["paragraph"] = json!(1);
        assert!(a.call("text_edit", &args).is_err(), "{args}");
        let current = a.session().get(pdfcraft_engine::DocId(doc)).unwrap();
        assert_eq!(current.bytes, before, "failed selection never changes bytes");
        assert_eq!(current.can_undo(), Some("Edit text"));
        assert_eq!(current.can_redo(), None);
    }
    ok(&mut a, "edit_undo", json!({"doc":doc}));
    assert_eq!(page_text(&mut a, doc)[0].trim(), "Page 1", "failed edits add no undo step");
    ok(&mut a, "edit_redo", json!({"doc":doc}));
    assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes, before);
    ok(&mut a, "text_edit", json!({"doc":doc,"page":1,"paragraph":1,"size":18,"color":"#0000ff"}));
    let p = &ok(&mut a, "text_paragraphs", json!({"doc":doc,"page":1}))["paragraphs"][0];
    assert_eq!(p["font"], "ABCDEF+ContributorShapes-Bold");
    ok(&mut a, "doc_save", json!({"doc":doc,"path":"selected.pdf"}));
    let reopened = ok(&mut a, "doc_open", json!({"path":"selected.pdf"}))["doc"].as_u64().unwrap();
    let p = &ok(&mut a, "text_paragraphs", json!({"doc":reopened,"page":1}))["paragraphs"][0];
    assert_eq!((p["font"].as_str(), p["text"].as_str(), p["size"].as_f64()), (Some("ABCDEF+ContributorShapes-Bold"), Some("AB BA"), Some(18.0)));
    let rendered = a.call("page_render", &json!({"doc":reopened,"page":1,"dpi":72})).unwrap();
    assert!(rendered.iter().any(|c| matches!(c, Content::Png { data, .. } if !data.is_empty())));
    // Independent italic/bold toggles retain the source family and the other style.
    ok(&mut a, "text_edit", json!({"doc":reopened,"page":1,"paragraph":1,"font":"times"}));
    assert_eq!(ok(&mut a, "text_paragraphs", json!({"doc":reopened,"page":1}))["paragraphs"][0]["font"], "Times-Bold");
    ok(&mut a, "text_edit", json!({"doc":reopened,"page":1,"paragraph":1,"italic":true}));
    assert_eq!(ok(&mut a, "text_paragraphs", json!({"doc":reopened,"page":1}))["paragraphs"][0]["font"], "Times-BoldItalic");
    ok(&mut a, "text_edit", json!({"doc":reopened,"page":1,"paragraph":1,"bold":false}));
    assert_eq!(ok(&mut a, "text_paragraphs", json!({"doc":reopened,"page":1}))["paragraphs"][0]["font"], "Times-Italic");
}

#[test]
fn excessive_paragraph_apply_and_preview_preserve_bytes_and_redo() {
    let dir = workdir("paragraph-input-budget");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({"path":"a.pdf"}))["doc"].as_u64().unwrap();
    ok(&mut a, "text_edit", json!({"doc":doc,"page":1,"paragraph":1,"text":"Changed"}));
    let changed = a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes.clone();
    ok(&mut a, "edit_undo", json!({"doc":doc}));
    let original = a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes.clone();
    let text = "x".repeat(1_000_001);
    for preview in [false, true] {
        let error = a.call("text_edit", &json!({"doc":doc,"page":1,"paragraph":1,"text":text,"preview":preview})).unwrap_err();
        assert!(error.to_string().contains("1,000,000 characters"));
        let current = a.session().get(pdfcraft_engine::DocId(doc)).unwrap();
        assert_eq!(current.bytes, original);
        assert_eq!(current.can_undo(), None);
        assert_eq!(current.can_redo(), Some("Edit text"));
    }
    // Direct engine callers get the same early guard before page inspection/private copies.
    let current = a.session().get(pdfcraft_engine::DocId(doc)).unwrap();
    let error = current.text_edit_preview(usize::MAX, 0, &text, &Default::default(), 300).err().unwrap();
    assert!(error.contains("1,000,000 characters"));
    let mut direct = pdfcraft_engine::Session::new();
    let direct_id = direct.open("budget.pdf", None, original.clone(), None).unwrap();
    let error =
        direct.apply(direct_id, pdfcraft_engine::Edit::EditTextBlock { page: usize::MAX, block: 0, text, style: Default::default() }).unwrap_err();
    assert!(error.to_string().contains("1,000,000 characters"));
    assert_eq!(direct.get(direct_id).unwrap().bytes, original);
    ok(&mut a, "edit_redo", json!({"doc":doc}));
    assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes, changed);
}

/// M7: replacing a whole paragraph's text keeps the untouched words' codes, TJ kerning and
/// repeated spaces, so every pixel outside the changed word is identical, through undo/redo,
/// save and reopen.
#[test]
fn whole_paragraph_text_edits_keep_untouched_words_codes_and_pixels_through_save() {
    use pdfcraft_cos::{Dict, Document, ObjRef, Object, SaveOptions, Stream, write_incremental};
    let dir = workdir("whole-paragraph-preserve");
    let mut cos = Document::open(std::sync::Arc::new(fixture(1))).unwrap();
    let content = "BT /F1 12 Tf 20 150 Td [(AV) -150 (AW) 80 (ay)] TJ (  double  gap end) Tj ET BT /F1 12 Tf 20 100 Td (Other paragraph) Tj ET";
    let stream = cos.add(Object::Stream(Stream::flate(Dict::new(), content.as_bytes())));
    cos.update_dict(ObjRef::new(4, 0), |page| page.set(b"Contents".to_vec(), Object::Ref(stream))).unwrap();
    std::fs::write(dir.join("kerned.pdf"), write_incremental(&cos, &SaveOptions::default()).unwrap()).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({"path":"kerned.pdf"}))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "text_paragraphs", json!({"doc":doc,"page":1}))["paragraphs"][0]["text"], "AVAWay  double  gap end");
    let pixels = |a: &mut Automation, doc| {
        let rendered = a.call("page_render", &json!({"doc":doc,"page":1,"dpi":144})).unwrap();
        let Content::Png { data, .. } = &rendered[0] else { panic!("expected page PNG") };
        image::load_from_memory(data).unwrap().to_rgba8()
    };
    let original = a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes.clone();
    let before = pixels(&mut a, doc);
    let edited = "AVAWay  double  gap fin";
    let applied = ok(&mut a, "text_edit", json!({"doc":doc,"page":1,"paragraph":1,"text":edited}));
    assert_eq!(applied["paragraph"]["text"], edited);
    let after = pixels(&mut a, doc);
    // Helvetica 12 pt: "AVAWay  double  gap " ends at 20 + 121.572 pt. Only the changed word, from
    // there on its own row, may differ (2 px per point at 144 dpi; the row is 138–154 pt down).
    let changed = |x: u32, y: u32| x >= 2 * 141 && (2 * 135..2 * 158).contains(&y);
    let mut differing = 0;
    for (x, y, pixel) in after.enumerate_pixels() {
        if before.get_pixel(x, y) != pixel {
            assert!(changed(x, y), "pixel {x},{y} outside the changed word differs");
            differing += 1;
        }
    }
    assert!(differing > 0, "the changed word is drawn");
    let changed_bytes = a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes.clone();
    ok(&mut a, "edit_undo", json!({"doc":doc}));
    assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes, original);
    ok(&mut a, "edit_redo", json!({"doc":doc}));
    assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes, changed_bytes);
    ok(&mut a, "doc_save", json!({"doc":doc,"path":"kerned-saved.pdf"}));
    let saved = ok(&mut a, "doc_open", json!({"path":"kerned-saved.pdf"}))["doc"].as_u64().unwrap();
    let paragraphs = ok(&mut a, "text_paragraphs", json!({"doc":saved,"page":1}));
    assert_eq!(paragraphs["paragraphs"][0]["text"], edited);
    assert_eq!(paragraphs["paragraphs"][1]["text"], "Other paragraph");
    // The saved content shows the untouched codes with the source TJ numbers.
    let reopened = Document::open(std::sync::Arc::new(std::fs::read(dir.join("kerned-saved.pdf")).unwrap())).unwrap();
    let page = pdfcraft_model::pages(&reopened).swap_remove(0);
    let data = match &*reopened.resolve(page.dict.get(b"Contents").unwrap()) {
        Object::Stream(s) => String::from_utf8_lossy(&s.decoded().unwrap()).into_owned(),
        other => panic!("{other:?}"),
    };
    let compact: String = data.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(compact.contains("[(AV) -150 (AW) 80 (ay double gap fin)] TJ"), "{data}");
    assert!(data.contains("(ay  double  gap fin)"), "repeated spaces are kept: {data}");
    assert!(!data.contains("(  double  gap end)"), "{data}");
}

/// M7: paragraphs turned by 90°, 180° (text matrix) and 30° (CTM) are edited through the
/// run-preserving path: untouched codes and kerning stay, and every pixel outside the changed
/// word is identical, through undo/redo, save and reopen.
#[test]
fn turned_paragraph_edits_keep_every_pixel_outside_the_changed_word() {
    use pdfcraft_cos::{Dict, Document, ObjRef, Object, SaveOptions, Stream, write_incremental};
    // (name, cm, Tm): the changed word starts at text x = 110, placed by its own Td.
    let cases = [
        ("turn90", [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], [0.0, 1.0, -1.0, 0.0, 100.0, 20.0]),
        ("turn180", [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], [-1.0, 0.0, 0.0, -1.0, 180.0, 200.0]),
        ("turn30", [0.866025, 0.5, -0.5, 0.866025, 30.0, 60.0], [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
    ];
    let then = |m: [f64; 6], n: [f64; 6]| {
        [
            m[0] * n[0] + m[1] * n[2],
            m[0] * n[1] + m[1] * n[3],
            m[2] * n[0] + m[3] * n[2],
            m[2] * n[1] + m[3] * n[3],
            m[4] * n[0] + m[5] * n[2] + n[4],
            m[4] * n[1] + m[5] * n[3] + n[5],
        ]
    };
    let dir = workdir("turned-paragraph-pixels");
    for (name, cm, tm) in cases {
        let numbers = |m: [f64; 6]| m.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" ");
        let content = format!(
            "q {} cm BT /F1 12 Tf {} Tm [(AV) -150 (AW) 80 (ay)] TJ (  turned) Tj 110 0 Td (end) Tj ET Q BT /F1 12 Tf 20 280 Td (Other words) Tj ET",
            numbers(cm),
            numbers(tm)
        );
        let mut cos = Document::open(std::sync::Arc::new(fixture(1))).unwrap();
        let stream = cos.add(Object::Stream(Stream::flate(Dict::new(), content.as_bytes())));
        cos.update_dict(ObjRef::new(4, 0), |page| page.set(b"Contents".to_vec(), Object::Ref(stream))).unwrap();
        let file = format!("{name}.pdf");
        std::fs::write(dir.join(&file), write_incremental(&cos, &SaveOptions::default()).unwrap()).unwrap();
        let mut a = auto(&dir);
        let doc = ok(&mut a, "doc_open", json!({"path":file}))["doc"].as_u64().unwrap();
        let paragraphs = ok(&mut a, "text_paragraphs", json!({"doc":doc,"page":1}));
        assert_eq!(paragraphs["paragraphs"][0]["text"], "AVAWay  turned end", "{name}");
        let pixels = |a: &mut Automation, doc| {
            let rendered = a.call("page_render", &json!({"doc":doc,"page":1,"dpi":144})).unwrap();
            let Content::Png { data, .. } = &rendered[0] else { panic!("expected page PNG") };
            image::load_from_memory(data).unwrap().to_rgba8()
        };
        let original = a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes.clone();
        let before = pixels(&mut a, doc);
        let edited = "AVAWay  turned fin";
        let applied = ok(&mut a, "text_edit", json!({"doc":doc,"page":1,"paragraph":1,"text":edited}));
        assert_eq!(applied["paragraph"]["text"], edited, "{name}");
        let after = pixels(&mut a, doc);
        // A pixel may differ only if its centre lies on the changed word: in its text space,
        // x from 110 to 135 and y from the descent to the ascent, plus a pixel (0.5 pt) of
        // antialiasing all round.
        let [a0, b0, c0, d0, e0, f0] = then(tm, cm);
        let det = a0 * d0 - b0 * c0;
        let changed = |x: u32, y: u32| {
            let (px, py) = ((f64::from(x) + 0.5) / 2.0 - e0, 300.0 - (f64::from(y) + 0.5) / 2.0 - f0);
            let (tx, ty) = ((d0 * px - c0 * py) / det, (a0 * py - b0 * px) / det);
            (109.0..136.0).contains(&tx) && (-4.0..10.5).contains(&ty)
        };
        let mut differing = 0;
        for (x, y, pixel) in after.enumerate_pixels() {
            if before.get_pixel(x, y) != pixel {
                assert!(changed(x, y), "{name}: pixel {x},{y} outside the changed word differs");
                differing += 1;
            }
        }
        assert!(differing > 0, "{name}: the changed word is drawn");
        let changed_bytes = a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes.clone();
        ok(&mut a, "edit_undo", json!({"doc":doc}));
        assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes, original, "{name}");
        ok(&mut a, "edit_redo", json!({"doc":doc}));
        assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes, changed_bytes, "{name}");
        let saved_file = format!("{name}-saved.pdf");
        ok(&mut a, "doc_save", json!({"doc":doc,"path":saved_file}));
        let saved = ok(&mut a, "doc_open", json!({"path":saved_file}))["doc"].as_u64().unwrap();
        let paragraphs = ok(&mut a, "text_paragraphs", json!({"doc":saved,"page":1}));
        assert_eq!(paragraphs["paragraphs"][0]["text"], edited, "{name}");
        assert_eq!(paragraphs["paragraphs"][1]["text"], "Other words", "{name}");
        assert!(pixels(&mut a, saved) == after, "{name}: the saved file renders as edited");
        // The untouched codes are shown with the source TJ numbers.
        let reopened = Document::open(std::sync::Arc::new(std::fs::read(dir.join(&saved_file)).unwrap())).unwrap();
        let page = pdfcraft_model::pages(&reopened).swap_remove(0);
        let data = match &*reopened.resolve(page.dict.get(b"Contents").unwrap()) {
            Object::Stream(s) => String::from_utf8_lossy(&s.decoded().unwrap()).into_owned(),
            other => panic!("{other:?}"),
        };
        let compact: String = data.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(compact.contains("[(AV) -150 (AW) 80 (ay turned)] TJ"), "{name}: {data}");
    }
}

#[test]
fn selected_color_keeps_q_restored_black_and_other_visible_glyphs() {
    use pdfcraft_cos::{Dict, Document, ObjRef, Object, SaveOptions, Stream, write_incremental};
    let dir = workdir("restored-colors");
    let mut cos = Document::open(std::sync::Arc::new(fixture(1))).unwrap();
    let content =
        "BT /F1 24 Tf 20 150 Td (A) Tj ET q /DeviceRGB cs 1 0 0 scn BT /F1 24 Tf 36.008 150 Td (B) Tj ET Q BT /F1 24 Tf 52.016 150 Td (C) Tj ET";
    let stream = cos.add(Object::Stream(Stream::flate(Dict::new(), content.as_bytes())));
    cos.update_dict(ObjRef::new(4, 0), |page| page.set(b"Contents".to_vec(), Object::Ref(stream))).unwrap();
    std::fs::write(dir.join("colors.pdf"), write_incremental(&cos, &SaveOptions::default()).unwrap()).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({"path":"colors.pdf"}))["doc"].as_u64().unwrap();
    let pixels = |a: &mut Automation, doc| {
        let rendered = a.call("page_render", &json!({"doc":doc,"page":1,"dpi":144})).unwrap();
        let Content::Png { data, .. } = &rendered[0] else { panic!("expected page PNG") };
        image::load_from_memory(data).unwrap().to_rgba8()
    };
    let before = pixels(&mut a, doc);
    let applied = ok(&mut a, "text_edit", json!({"doc":doc,"page":1,"paragraph":1,"range_start":0,"range_end":1,"color":"#0000ff"}));
    assert_eq!(applied["paragraph"]["lines"], 1, "style fragments are one visual row");
    let after = pixels(&mut a, doc);
    let mut red = 0;
    let mut black = 0;
    let mut blue = 0;
    for (x, y, color) in after.enumerate_pixels() {
        let [r, g, b, alpha] = color.0;
        if x >= 73 {
            assert_eq!(color, before.get_pixel(x, y), "unselected glyph at {x},{y} changed");
            red += usize::from(r > 150 && g < 60 && b < 60 && alpha > 200);
            black += usize::from(r < 40 && g < 40 && b < 40 && alpha > 200);
        } else {
            blue += usize::from(b > 150 && r < 60 && g < 60 && alpha > 200);
        }
    }
    assert!(red > 5 && black > 5 && blue > 5, "real red B, black C and blue A pixels: {red}, {black}, {blue}");
    let runs = &ok(&mut a, "text_paragraphs", json!({"doc":doc,"page":1}))["paragraphs"][0]["runs"];
    assert_eq!(runs[2]["color"], json!([0.0, 0.0, 0.0]));
    ok(&mut a, "doc_save", json!({"doc":doc,"path":"restyled.pdf"}));
    let reopened = ok(&mut a, "doc_open", json!({"path":"restyled.pdf"}))["doc"].as_u64().unwrap();
    assert_eq!(pixels(&mut a, reopened), after);
}

#[test]
fn selected_recolor_preserves_other_device_space_pixels() {
    use pdfcraft_cos::{Dict, Document, ObjRef, Object, SaveOptions, Stream, write_incremental};
    let dir = workdir("device-space-pixels");
    for (index, (initial, next)) in [("0.2 g", "0.7 sc"), ("1 0 0 rg", "0 1 0 scn"), ("0 1 1 0 k", "1 0 0 0 sc")].into_iter().enumerate() {
        let mut cos = Document::open(std::sync::Arc::new(fixture(1))).unwrap();
        let content = format!(
            "BT /F1 24 Tf 20 150 Td (A) Tj ET q {initial} BT /F1 24 Tf 36.008 150 Td (B) Tj {next} 1 0 0 1 52.016 150 Tm (C) Tj ET Q BT /F1 24 Tf 69.344 150 Td (D) Tj ET"
        );
        let stream = cos.add(Object::Stream(Stream::flate(Dict::new(), content.as_bytes())));
        cos.update_dict(ObjRef::new(4, 0), |page| page.set(b"Contents".to_vec(), Object::Ref(stream))).unwrap();
        let path = format!("device-{index}.pdf");
        std::fs::write(dir.join(&path), write_incremental(&cos, &SaveOptions::default()).unwrap()).unwrap();
        let mut a = auto(&dir);
        let doc = ok(&mut a, "doc_open", json!({"path":path}))["doc"].as_u64().unwrap();
        let pixels = |a: &mut Automation| {
            let rendered = a.call("page_render", &json!({"doc":doc,"page":1,"dpi":144})).unwrap();
            let Content::Png { data, .. } = &rendered[0] else { panic!("expected page PNG") };
            image::load_from_memory(data).unwrap().to_rgba8()
        };
        let before = pixels(&mut a);
        ok(&mut a, "text_edit", json!({"doc":doc,"page":1,"paragraph":1,"range_start":1,"range_end":2,"color":"#0000ff"}));
        let after = pixels(&mut a);
        assert_ne!(after, before, "selected B visibly changes");
        for (x, y, color) in after.enumerate_pixels() {
            if !(73..104).contains(&x) {
                assert_eq!(color, before.get_pixel(x, y), "{initial} then {next}: unselected pixel at {x},{y} changed");
            }
        }
    }
}

#[test]
fn selected_unsupported_graphics_leave_preview_bytes_and_history_unchanged() {
    use pdfcraft_cos::{Dict, Document, ObjRef, Object, SaveOptions, Stream, write_incremental};
    let dir = workdir("unsupported-graphics");
    let mut cos = Document::open(std::sync::Arc::new(fixture(1))).unwrap();
    let content = "q BT /F1 12 Tf 1 Tr 1 0 0 RG 2 w 20 200 Td (Special) Tj ET Q BT /F1 12 Tf 20 100 Td (Normal) Tj ET";
    let stream = cos.add(Object::Stream(Stream::flate(Dict::new(), content.as_bytes())));
    cos.update_dict(ObjRef::new(4, 0), |page| page.set(b"Contents".to_vec(), Object::Ref(stream))).unwrap();
    std::fs::write(dir.join("graphics.pdf"), write_incremental(&cos, &SaveOptions::default()).unwrap()).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({"path":"graphics.pdf"}))["doc"].as_u64().unwrap();
    ok(&mut a, "text_edit", json!({"doc":doc,"page":1,"paragraph":2,"range_start":0,"range_end":1,"color":"#0000ff"}));
    let before = a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes.clone();
    for preview in [false, true] {
        let err =
            a.call("text_edit", &json!({"doc":doc,"page":1,"paragraph":1,"range_start":0,"range_end":1,"size":18,"preview":preview})).unwrap_err();
        assert!(err.to_string().contains("appearance is preserved unchanged"), "{err}");
        let current = a.session().get(pdfcraft_engine::DocId(doc)).unwrap();
        assert_eq!(current.bytes, before);
        assert_eq!(current.can_undo(), Some("Edit text"));
        assert_eq!(current.can_redo(), None);
    }
    ok(&mut a, "edit_undo", json!({"doc":doc}));
    assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().can_undo(), None);
    ok(&mut a, "edit_redo", json!({"doc":doc}));
    assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes, before);
}

#[test]
fn selected_underlines_preview_roundtrip_and_preserve_history() {
    let dir = workdir("selected-underlines");
    std::fs::write(dir.join("source.pdf"), fixture(1)).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({"path":"source.pdf"}))["doc"].as_u64().unwrap();
    let original = a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes.clone();
    let png = |content: Vec<Content>| {
        content
            .into_iter()
            .find_map(|c| match c {
                Content::Png { data, .. } => Some(data),
                _ => None,
            })
            .unwrap()
    };
    let before_pixels = png(a.call("page_render", &json!({"doc":doc,"page":1,"dpi":72})).unwrap());
    let preview = png(a
        .call("text_edit", &json!({"doc":doc,"page":1,"paragraph":1,"range_start":0,"range_end":4,"underline":true,"preview":true,"max_side":300}))
        .unwrap());
    assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes, original);
    ok(&mut a, "text_edit", json!({"doc":doc,"page":1,"paragraph":1,"range_start":0,"range_end":4,"underline":true}));
    let rendered = png(a.call("page_render", &json!({"doc":doc,"page":1,"dpi":72})).unwrap());
    assert_eq!(preview, rendered, "the real proposed PDF includes its underline paths");
    assert_ne!(before_pixels, rendered, "the underline is visible");
    let paragraphs = ok(&mut a, "text_paragraphs", json!({"doc":doc,"page":1}));
    let runs = paragraphs["paragraphs"][0]["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!((runs[0]["start"].as_u64(), runs[0]["end"].as_u64(), runs[0]["underline"].as_bool()), (Some(0), Some(4), Some(true)));
    assert_eq!(runs[1]["underline"], false);
    ok(&mut a, "doc_save", json!({"doc":doc,"path":"underlined.pdf"}));
    // Saving makes the written file the working bytes, so the undo target is captured after it.
    let underlined = a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes.clone();
    let reopened = ok(&mut a, "doc_open", json!({"path":"underlined.pdf"}))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "text_paragraphs", json!({"doc":reopened,"page":1}))["paragraphs"][0]["runs"], paragraphs["paragraphs"][0]["runs"]);
    ok(&mut a, "text_edit", json!({"doc":doc,"page":1,"paragraph":1,"range_start":0,"range_end":4,"underline":false}));
    assert_eq!(png(a.call("page_render", &json!({"doc":doc,"page":1,"dpi":72})).unwrap()), before_pixels);
    ok(&mut a, "edit_undo", json!({"doc":doc}));
    assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes, underlined);
    assert_eq!(ok(&mut a, "text_paragraphs", json!({"doc":doc,"page":1}))["paragraphs"][0]["runs"], paragraphs["paragraphs"][0]["runs"]);
    // Already underlined: no extra history step and redo remains available.
    ok(&mut a, "text_edit", json!({"doc":doc,"page":1,"paragraph":1,"range_start":0,"range_end":4,"underline":true}));
    assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().can_redo(), Some("Edit text"));
    ok(&mut a, "edit_redo", json!({"doc":doc}));
    assert_eq!(png(a.call("page_render", &json!({"doc":doc,"page":1,"dpi":72})).unwrap()), before_pixels);
}

#[test]
fn selected_character_runs_keep_surroundings_through_reflow_history_and_save() {
    let dir = workdir("character-runs");
    std::fs::write(dir.join("runs.pdf"), embedded_page_font_pdf()).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({"path":"runs.pdf"}))["doc"].as_u64().unwrap();
    ok(&mut a, "text_edit", json!({"doc":doc,"page":1,"paragraph":1,"text":"Before AB after","font":"times","size":12}));
    let initial = a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes.clone();
    // Already-satisfied formatting must not consume an undo step or clear redo.
    ok(&mut a, "text_edit", json!({"doc":doc,"page":1,"paragraph":1,"range_start":0,"range_end":6,"bold":false}));
    assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes, initial);
    ok(&mut a, "edit_undo", json!({"doc":doc}));
    assert_eq!(ok(&mut a, "text_paragraphs", json!({"doc":doc,"page":1}))["paragraphs"][0]["text"], "Page 1");
    ok(&mut a, "text_edit", json!({"doc":doc,"page":1,"paragraph":1,"bold":false}));
    assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().can_redo(), Some("Edit text"));
    ok(&mut a, "edit_redo", json!({"doc":doc}));
    let side = {
        let page = &a.session().get(pdfcraft_engine::DocId(doc)).unwrap().info.pages[0];
        page.width.max(page.height) as u32
    };
    let preview = a.call("text_edit", &json!({"doc":doc,"page":1,"paragraph":1,"range_start":7,"range_end":9,"font_resource":"Embedded","color":"#0000ff","preview":true,"max_side":side})).unwrap();
    assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes, initial);
    ok(&mut a, "text_edit", json!({"doc":doc,"page":1,"paragraph":1,"range_start":7,"range_end":9,"font_resource":"Embedded","color":"#0000ff"}));
    let actual = a.call("page_render", &json!({"doc":doc,"page":1,"dpi":72})).unwrap();
    let png = |content: &[Content]| {
        content
            .iter()
            .find_map(|c| match c {
                Content::Png { data, .. } => Some(data.clone()),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(png(&preview), png(&actual), "preview pixels equal the accepted PDF pixels");
    let p = &ok(&mut a, "text_paragraphs", json!({"doc":doc,"page":1}))["paragraphs"][0];
    assert_eq!(p["runs"].as_array().unwrap().len(), 3);
    assert_eq!(p["runs"][0]["font"], "Times-Roman");
    assert_eq!(p["runs"][1]["font"], "ABCDEF+ContributorShapes-Bold");
    assert_eq!(p["runs"][2]["font"], "Times-Roman");
    let selected = a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes.clone();
    for range in [
        json!({"range_start":0,"range_end":6,"font_resource":"Embedded"}),
        json!({"range_start":7}),
        json!({"range_start":9,"range_end":8}),
        json!({"range_start":0,"range_end":1000}),
    ] {
        let mut args = range;
        args["doc"] = json!(doc);
        args["page"] = json!(1);
        args["paragraph"] = json!(1);
        assert!(a.call("text_edit", &args).is_err(), "{args}");
        assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes, selected);
    }
    ok(&mut a, "edit_undo", json!({"doc":doc}));
    assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes, initial);
    ok(&mut a, "edit_redo", json!({"doc":doc}));
    assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes, selected);
    ok(&mut a, "text_edit", json!({"doc":doc,"page":1,"paragraph":1,"width":65}));
    ok(&mut a, "doc_save", json!({"doc":doc,"path":"mixed-saved.pdf"}));
    let saved = ok(&mut a, "doc_open", json!({"path":"mixed-saved.pdf"}))["doc"].as_u64().unwrap();
    let p = &ok(&mut a, "text_paragraphs", json!({"doc":saved,"page":1}))["paragraphs"][0];
    assert_eq!(p["text"], "Before AB after");
    assert!(p["line_count"].as_u64().unwrap() > 1);
    assert!(p["runs"].as_array().unwrap().iter().any(|r| r["font"] == "ABCDEF+ContributorShapes-Bold"));
    assert!(
        a.call("page_render", &json!({"doc":saved,"page":1,"dpi":96}))
            .unwrap()
            .iter()
            .any(|c| matches!(c, Content::Png { data, .. } if !data.is_empty()))
    );
}

/// One page whose content is one stream in three pieces (#155): a `TJ` array ends the middle
/// piece and its operator starts the last one.
fn split_streams_pdf() -> Vec<u8> {
    let piece = |s: &str| format!("<< /Length {} >>\nstream\n{s}\nendstream", s.len());
    let objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents [4 0 R 5 0 R 6 0 R] /Resources << /Font << /F1 7 0 R >> >> >>".into(),
        piece("/P << /MCID 0"),
        piece(">> BDC BT /F1 12 Tf 72 700 Td (Target) Tj 0 -20 Td [(After) -20 (wards)]"),
        piece("TJ ET EMC"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
    ];
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

#[test]
fn a_line_split_across_content_streams_is_listed_and_edited() {
    let dir = workdir("split-streams");
    std::fs::write(dir.join("split.pdf"), split_streams_pdf()).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "split.pdf" }))["doc"].as_u64().unwrap();
    let lines = ok(&mut a, "text_lines", json!({ "doc": doc, "page": 1 }));
    let texts: Vec<&str> = lines["lines"].as_array().unwrap().iter().filter_map(|l| l["text"].as_str()).collect();
    assert_eq!(texts, ["Target", "Afterwards"], "{lines}");
    let r = ok(&mut a, "text_edit", json!({ "doc": doc, "page": 1, "line": 2, "text": "Later" }));
    assert_eq!(r["line"]["text"], "Later");
    assert_eq!(page_text(&mut a, doc)[0].split_whitespace().collect::<Vec<_>>(), ["Target", "Later"]);
}

#[test]
fn paragraph_bold_without_font_keeps_the_source_family() {
    let dir = workdir("paragraph-bold");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "text_edit", json!({ "doc": doc, "page": 2, "paragraph": 1, "font": "times" }));
    let bold = ok(&mut a, "text_edit", json!({ "doc": doc, "page": 2, "paragraph": 1, "bold": true }));
    assert_eq!(bold["paragraph"]["font"], "Times-Bold");
    let regular = ok(&mut a, "text_edit", json!({ "doc": doc, "page": 2, "paragraph": 1, "bold": false }));
    assert_eq!(regular["paragraph"]["font"], "Times-Roman");
    assert_eq!(regular["paragraph"]["text"], "Page 2");
    assert!(matches!(a.call("text_edit", &json!({ "doc": doc, "page": 2, "paragraph": 1, "bold": "yes" })), Err(ToolError::InvalidArgs(_))));
}

#[test]
fn editing_page_images_through_tools() {
    let dir = workdir("page-images");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "doc_export_images", json!({ "doc": doc, "folder": "src", "dpi": 18, "pages": [1, 2] }));
    let made = ok(&mut a, "doc_create", json!({ "from": "images", "paths": ["src/a_page_1.png"] }))["doc"].as_u64().unwrap();
    let list = ok(&mut a, "page_images", json!({ "doc": made, "page": 1 }));
    assert_eq!(list["count"], 1);
    let r = ok(&mut a, "image_edit", json!({ "doc": made, "page": 1, "image": 1, "action": "move", "rect": [5, 5, 25, 35] }));
    assert_eq!(r["undo"], "Move image");
    let moved = ok(&mut a, "page_images", json!({ "doc": made, "page": 1 }))["images"][0]["rect"].clone();
    assert_eq!(moved, json!([5.0, 5.0, 25.0, 35.0]));
    // Crop to its left half: still the same bitmap, now showing only that part.
    let r = ok(&mut a, "image_edit", json!({ "doc": made, "page": 1, "image": 1, "action": "crop", "rect": [5, 5, 15, 35] }));
    assert_eq!(r["undo"], "Crop image");
    let cropped = ok(&mut a, "page_images", json!({ "doc": made, "page": 1 }))["images"][0].clone();
    assert_eq!(cropped["kind"], "image");
    assert_eq!(cropped["rect"], json!([5.0, 5.0, 15.0, 35.0]));
    assert!(matches!(
        a.call("image_edit", &json!({ "doc": made, "page": 1, "image": 1, "action": "crop", "rect": [100, 100, 120, 120] })),
        Err(ToolError::InvalidArgs(_) | ToolError::Failed(_))
    ));
    ok(&mut a, "image_edit", json!({ "doc": made, "page": 1, "image": 1, "action": "rotate" }));
    ok(&mut a, "image_edit", json!({ "doc": made, "page": 1, "image": 1, "action": "flip_horizontal" }));
    let saved = ok(&mut a, "image_save", json!({ "doc": made, "page": 1, "image": 1, "path": "out/picture" }));
    assert_eq!(saved["format"], "png");
    assert!(std::fs::read(dir.join("out/picture.png")).unwrap().starts_with(b"\x89PNG"));
    ok(&mut a, "image_edit", json!({ "doc": made, "page": 1, "image": 1, "action": "replace", "path": "src/a_page_2.png" }));
    assert_eq!(ok(&mut a, "page_images", json!({ "doc": made, "page": 1 }))["count"], 1);
    ok(&mut a, "image_edit", json!({ "doc": made, "page": 1, "image": 1, "action": "delete" }));
    assert_eq!(ok(&mut a, "page_images", json!({ "doc": made, "page": 1 }))["count"], 0);
    assert!(matches!(a.call("image_edit", &json!({ "doc": made, "page": 1, "image": 1, "action": "delete" })), Err(ToolError::InvalidArgs(_))));
}

#[test]
fn cropping_a_page_image_through_tools_hides_the_rest() {
    let dir = workdir("crop-image");
    let mut jpeg = Vec::new();
    image::RgbImage::from_pixel(40, 60, image::Rgb([220, 30, 30])).write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();
    std::fs::write(dir.join("red.jpg"), jpeg).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_create", json!({ "from": "images", "paths": ["red.jpg"] }))["doc"].as_u64().unwrap();
    let rect: Vec<f64> = ok(&mut a, "page_images", json!({ "doc": doc, "page": 1 }))["images"][0]["rect"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    let mid = (rect[0] + rect[2]) / 2.0;
    let pixels = |a: &mut Automation| {
        let rendered = a.call("page_render", &json!({ "doc": doc, "page": 1, "dpi": 72 })).unwrap();
        let Content::Png { data, .. } = &rendered[0] else { panic!("expected page PNG") };
        image::load_from_memory(data).unwrap().to_rgba8()
    };
    let red = |p: &image::Rgba<u8>| p[0] > 150 && p[1] < 100;
    let (lx, rx, y) = (((rect[0] + mid) / 2.0) as u32, ((mid + rect[2]) / 2.0) as u32, ((rect[1] + rect[3]) / 2.0) as u32);
    let before = pixels(&mut a);
    assert!(red(before.get_pixel(lx, y)) && red(before.get_pixel(rx, y)));
    // Keep the left half.
    ok(&mut a, "image_edit", json!({ "doc": doc, "page": 1, "image": 1, "action": "crop", "rect": [rect[0], rect[1], mid, rect[3]] }));
    let after = pixels(&mut a);
    assert!(red(after.get_pixel(lx, y)), "the kept half still shows");
    assert!(!red(after.get_pixel(rx, y)), "the cropped half is hidden: {:?}", after.get_pixel(rx, y));
    // The crop survives saving and reopening, and undo brings the whole image back.
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "out/cropped.pdf" }));
    let re = ok(&mut a, "doc_open", json!({ "path": "out/cropped.pdf" }))["doc"].as_u64().unwrap();
    let kept = ok(&mut a, "page_images", json!({ "doc": re, "page": 1 }))["images"][0].clone();
    assert_eq!(kept["kind"], "image");
    assert!((kept["rect"][2].as_f64().unwrap() - mid).abs() < 0.01, "{kept}");
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert!(red(pixels(&mut a).get_pixel(rx, y)));
}

#[test]
fn editing_form_artwork_through_tools() {
    use pdfcraft_cos::{Dict, Document, Object, SaveOptions, Stream, write_incremental};
    let dir = workdir("form-artwork");
    let mut cos = Document::open(std::sync::Arc::new(fixture(1))).unwrap();
    let page = pdfcraft_model::pages(&cos)[0].clone();
    let mut form = Dict::new();
    form.set(b"Subtype".to_vec(), Object::name("Form"));
    form.set(b"BBox".to_vec(), Object::Array([0, 0, 80, 40].map(Object::Int).to_vec()));
    let artwork = cos.add(Object::Stream(Stream::flate(form, b"0.2 0.5 0.9 rg 0 0 80 40 re f")));
    let contents = cos.add(Object::Stream(Stream::flate(Dict::new(), b"q 1 0 0 1 20 240 cm /Figure Do Q")));
    cos.update_dict(page.obj, |d| {
        d.set(b"Contents".to_vec(), Object::Ref(contents));
        let mut xo = Dict::new();
        xo.set(b"Figure".to_vec(), Object::Ref(artwork));
        let mut resources = Dict::new();
        resources.set(b"XObject".to_vec(), Object::Dict(xo));
        d.set(b"Resources".to_vec(), Object::Dict(resources));
    })
    .unwrap();
    std::fs::write(dir.join("figure.pdf"), write_incremental(&cos, &SaveOptions::default()).unwrap()).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({"path": "figure.pdf"}))["doc"].as_u64().unwrap();
    let before = ok(&mut a, "page_images", json!({"doc": doc, "page": 1}))["images"][0].clone();
    assert_eq!(before["kind"], "form");
    assert_eq!(before["pixels"], json!([0, 0]));
    assert_eq!(before["rect"], json!([20.0, 20.0, 100.0, 60.0]));
    let rendered_before = a.call("page_render", &json!({"doc": doc, "page": 1, "dpi": 72})).unwrap();
    assert!(a.call("image_save", &json!({"doc": doc, "page": 1, "image": 1, "path": "out/figure"})).is_err());
    ok(&mut a, "image_edit", json!({"doc": doc, "page": 1, "image": 1, "action": "move", "rect": [40, 50, 120, 90]}));
    assert_eq!(ok(&mut a, "page_images", json!({"doc": doc, "page": 1}))["images"][0]["rect"], json!([40.0, 50.0, 120.0, 90.0]));
    let rendered_moved = a.call("page_render", &json!({"doc": doc, "page": 1, "dpi": 72})).unwrap();
    assert_ne!(rendered_before, rendered_moved, "the rendered artwork actually moves");
    ok(&mut a, "doc_save", json!({"doc": doc, "path": "out/moved.pdf"}));
    let re = ok(&mut a, "doc_open", json!({"path": "out/moved.pdf"}))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "page_images", json!({"doc": re, "page": 1}))["images"][0]["rect"], json!([40.0, 50.0, 120.0, 90.0]));
    ok(&mut a, "image_edit", json!({"doc": re, "page": 1, "image": 1, "action": "delete"}));
    assert_eq!(ok(&mut a, "page_images", json!({"doc": re, "page": 1}))["count"], 0);
    ok(&mut a, "edit_undo", json!({"doc": re}));
    assert_eq!(ok(&mut a, "page_images", json!({"doc": re, "page": 1}))["count"], 1);
    ok(&mut a, "edit_undo", json!({"doc": doc}));
    assert_eq!(ok(&mut a, "page_images", json!({"doc": doc, "page": 1}))["images"][0], before);
    assert_eq!(a.call("page_render", &json!({"doc": doc, "page": 1, "dpi": 72})).unwrap(), rendered_before);
}

#[test]
fn auditing_space_through_tools() {
    let dir = workdir("audit");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "doc_audit_space", json!({ "doc": doc }));
    let rows = r["categories"].as_array().unwrap();
    let total: f64 = rows.iter().map(|x| x["percent"].as_f64().unwrap()).sum();
    assert!((total - 100.0).abs() < 0.1, "{r}");
    let content = rows.iter().find(|x| x["category"] == "Content Streams").unwrap();
    assert!(content["bytes"].as_u64().unwrap() > 0, "{r}");
    // Measured in the file: the rows add up to its size exactly, nothing estimated.
    let size = std::fs::metadata(dir.join("a.pdf")).unwrap().len();
    assert_eq!(rows.iter().map(|x| x["bytes"].as_u64().unwrap()).sum::<u64>(), size, "{r}");
    assert_eq!((r["file_bytes"].as_u64(), r["total_bytes"].as_u64(), r["estimated_objects"].as_u64()), (Some(size), Some(size), Some(0)), "{r}");
    // The three pages' content streams, each measured from "n 0 obj" to "endobj".
    assert_eq!(content["objects"], 3, "{r}");
    let fonts = rows.iter().find(|x| x["category"] == "Fonts").unwrap();
    assert_eq!(fonts["objects"], 1, "one Helvetica shared by every page counts once: {r}");
    let o = &r["overhead"];
    let overhead = rows.iter().find(|x| x["category"] == "Document Overhead").unwrap()["bytes"].as_u64().unwrap();
    assert_eq!(o["cross_reference"].as_u64().unwrap() + o["unclaimed_objects"].as_u64().unwrap() + o["other"].as_u64().unwrap(), overhead, "{r}");
}

/// One page drawing "Hello" in an embedded, complete TrueType font (an OFL font already in the
/// repository), with a classic cross-reference table.
fn embedded_font_fixture() -> Vec<u8> {
    let program: &[u8] = include_bytes!("../../../assets/fonts/DancingScript.ttf");
    let widths = vec!["500"; 95].join(" ");
    let body = "BT /F1 24 Tf 20 150 Td (Hello) Tj ET";
    let mut objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
        format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()).into_bytes(),
        format!("<< /Type /Font /Subtype /TrueType /BaseFont /DancingScript /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 126 /Widths [{widths}] /FontDescriptor 6 0 R >>").into_bytes(),
        b"<< /Type /FontDescriptor /FontName /DancingScript /Flags 32 /FontBBox [0 -300 1000 900] /ItalicAngle 0 /Ascent 900 /Descent -300 /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>".to_vec(),
    ];
    let mut file = format!("<< /Length {} /Length1 {} >>\nstream\n", program.len(), program.len()).into_bytes();
    file.extend_from_slice(program);
    file.extend_from_slice(b"\nendstream");
    objs.push(file);
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

#[test]
fn auditing_fonts_and_unembedding_through_tools() {
    let dir = workdir("unembed");
    let fixture = embedded_font_fixture();
    std::fs::write(dir.join("font.pdf"), &fixture).unwrap();
    let program_len = include_bytes!("../../../assets/fonts/DancingScript.ttf").len() as u64;
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "font.pdf" }))["doc"].as_u64().unwrap();

    // The audit puts the font program under Fonts, measured to the byte.
    let r = ok(&mut a, "doc_audit_space", json!({ "doc": doc }));
    let rows = r["categories"].as_array().unwrap();
    let fonts = rows.iter().find(|x| x["category"] == "Fonts").unwrap();
    // Font dictionary, descriptor and program, each as "n 0 obj\n…\nendobj\n" in the file.
    let span = |n: usize| {
        let head = format!("\n{n} 0 obj\n");
        let s = fixture.windows(head.len()).position(|w| w == head.as_bytes()).unwrap() + 1;
        (s + fixture[s..].windows(7).position(|w| w == b"endobj\n").unwrap() + 7 - s) as u64
    };
    assert_eq!(fonts["bytes"].as_u64().unwrap(), span(5) + span(6) + span(7), "{r}");
    assert!(fonts["bytes"].as_u64().unwrap() > program_len, "{r}");
    assert_eq!(fonts["objects"], 3, "{r}");
    assert_eq!(rows.iter().map(|x| x["bytes"].as_u64().unwrap()).sum::<u64>(), fixture.len() as u64);

    let listed = ok(&mut a, "doc_embedded_fonts", json!({ "doc": doc }));
    assert_eq!(listed["count"], 1, "{listed}");
    let f = &listed["fonts"][0];
    assert_eq!(
        (f["name"].as_str(), f["program"].as_str(), f["can_unembed"].as_bool()),
        (Some("DancingScript"), Some("FontFile2"), Some(true)),
        "{listed}"
    );
    assert_eq!(f["program_bytes"].as_u64(), Some(program_len));

    // Unknown names are refused with what is embedded.
    match a.call("doc_optimize", &json!({ "doc": doc, "path": "x.pdf", "unembed_fonts": ["Nope"] })) {
        Err(ToolError::InvalidArgs(m)) => assert!(m.contains("DancingScript"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(!dir.join("x.pdf").exists(), "nothing written");

    let out = ok(&mut a, "doc_optimize", json!({ "doc": doc, "path": "unembedded.pdf", "unembed_fonts": ["DancingScript"] }));
    assert_eq!(out["fonts_unembedded"], 1, "{out}");
    assert!(out["bytes_after"].as_u64().unwrap() + program_len / 2 < out["bytes_before"].as_u64().unwrap(), "{out}");
    let small = ok(&mut a, "doc_open", json!({ "path": "unembedded.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "doc_embedded_fonts", json!({ "doc": small }))["count"], 0);
    // The text keeps its codes: it still extracts the same.
    assert_eq!(page_text(&mut a, small), vec!["Hello".to_string()]);
    assert_eq!(page_text(&mut a, doc), vec!["Hello".to_string()]);
    let after = ok(&mut a, "doc_audit_space", json!({ "doc": small }));
    let fonts_after = after["categories"].as_array().unwrap().iter().find(|x| x["category"] == "Fonts").unwrap()["bytes"].as_u64().unwrap();
    assert!(fonts_after < 1_000, "{after}");
}

#[test]
fn exporting_all_images_through_tools() {
    let dir = workdir("export-all-images");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "doc_export_all_images", json!({ "doc": doc, "folder": "none" }))["count"], 0, "text only");
    // A PDF made from two page renders (PNG, JPEG) holds two images.
    ok(&mut a, "doc_export_images", json!({ "doc": doc, "folder": "src", "dpi": 36, "pages": [1] }));
    ok(&mut a, "doc_export_images", json!({ "doc": doc, "folder": "src", "dpi": 36, "pages": [2], "format": "jpeg" }));
    let made = ok(&mut a, "doc_create", json!({ "from": "images", "paths": ["src/a_page_1.png", "src/a_page_2.jpg"] }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "doc_export_all_images", json!({ "doc": made, "folder": "imgs" }));
    assert_eq!(r["count"], 2, "{r}");
    let files: Vec<&str> = r["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert!(files[0].ends_with("_Page_1_Image_0001.png") && files[1].ends_with("_Page_2_Image_0002.jpg"), "{files:?}");
    assert_eq!(std::fs::read(files[1]).unwrap(), std::fs::read(dir.join("src/a_page_2.jpg")).unwrap(), "JPEG unchanged");
    assert!(std::fs::read(files[0]).unwrap().starts_with(b"\x89PNG"));
    assert_eq!(ok(&mut a, "doc_export_all_images", json!({ "doc": made, "folder": "imgs2", "pages": [2] }))["count"], 1);
    assert_eq!(ok(&mut a, "doc_export_all_images", json!({ "doc": made, "folder": "imgs3", "min_size": 10000 }))["count"], 0);
}

#[test]
fn fill_and_sign_through_tools() {
    let dir = workdir("fill");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "fill_sign_add", json!({ "doc": doc, "page": 1, "type": "text", "at": [20, 40], "text": "Ada Lovelace" }));
    ok(&mut a, "fill_sign_add", json!({ "doc": doc, "page": 1, "type": "check", "at": [20, 80] }));
    ok(&mut a, "fill_sign_add", json!({ "doc": doc, "page": 1, "type": "date", "at": [20, 100] }));
    ok(&mut a, "fill_sign_add", json!({ "doc": doc, "page": 1, "type": "signature", "at": [20, 150], "text": "Ada Lovelace" }));
    assert!(matches!(
        a.call("fill_sign_add", &json!({ "doc": doc, "page": 1, "type": "initials", "at": [20, 190], "text": "   " })),
        Err(ToolError::InvalidArgs(_))
    ));
    let texts = page_text(&mut a, doc);
    assert!(texts[0].contains("Ada Lovelace") && texts[0].contains("11/14/2023"), "{texts:?}");
    // Preferences ▸ Date format, and a one-off pattern.
    let f = ok(&mut a, "fill_sign_date_format", json!({}));
    assert_eq!((f["format"].as_str(), f["today"].as_str()), (Some("m/d/yyyy"), Some("11/14/2023")), "{f}");
    assert_eq!(ok(&mut a, "fill_sign_date_format", json!({ "format": "yyyy.mm.dd." }))["today"], "2023.11.14.");
    ok(&mut a, "fill_sign_add", json!({ "doc": doc, "page": 1, "type": "date", "at": [20, 220] }));
    ok(&mut a, "fill_sign_add", json!({ "doc": doc, "page": 1, "type": "date", "at": [20, 240], "format": "d \\de mmmm" }));
    let texts = page_text(&mut a, doc);
    assert!(texts[0].contains("2023.11.14.") && texts[0].contains("14 de November"), "{texts:?}");
    assert!(matches!(a.call("fill_sign_date_format", &json!({ "format": "HH:MM" })), Err(ToolError::InvalidArgs(_))));
    assert_eq!(ok(&mut a, "fill_sign_date_format", json!({}))["format"], "yyyy.mm.dd.", "a rejected format keeps the previous one");
    // Month and weekday names in a chosen language, for the preference or one date.
    let f = ok(&mut a, "fill_sign_date_format", json!({ "format": "d. mmmm yyyy", "language": "cs" }));
    assert_eq!((f["language"].as_str(), f["today"].as_str()), (Some("cs"), Some("14. listopadu 2023")), "{f}");
    assert_eq!(f["languages"].as_array().unwrap().len(), pdfcraft_engine::dates::DATE_LANGUAGES.len(), "every date language");
    ok(&mut a, "fill_sign_add", json!({ "doc": doc, "page": 1, "type": "date", "at": [20, 280], "format": "d \\de mmmm", "language": "es" }));
    assert!(page_text(&mut a, doc)[0].contains("14 de noviembre"));
    assert!(matches!(a.call("fill_sign_date_format", &json!({ "format": "yyy" })), Err(ToolError::InvalidArgs(_))));
    // A date the PDF's text font can't hold is refused, not saved as "?".
    match a.call("fill_sign_add", &json!({ "doc": doc, "page": 1, "type": "date", "at": [20, 300], "format": "dddd", "language": "ja" })) {
        Err(ToolError::InvalidArgs(e)) => assert!(e.contains("火曜日") && e.contains("can't be written into the PDF yet"), "{e}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let f = ok(&mut a, "fill_sign_date_format", json!({ "format": "dddd", "language": "ja" }));
    assert_eq!((f["today"].as_str(), f["unwritable"].as_str()), (Some("火曜日"), Some("火曜日")));
    ok(&mut a, "fill_sign_date_format", json!({ "format": "d. mmmm yyyy", "language": "cs" }));
    assert!(matches!(a.call("fill_sign_date_format", &json!({ "format": "dd", "language": "xx" })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(
        a.call("fill_sign_add", &json!({ "doc": doc, "page": 1, "type": "date", "at": [20, 300], "language": "xx" })),
        Err(ToolError::InvalidArgs(_))
    ));
    let f = ok(&mut a, "fill_sign_date_format", json!({ "language": "auto" }));
    assert_eq!((f["format"].as_str(), f["language"].as_str(), f["today"].as_str()), (Some("d. mmmm yyyy"), Some("auto"), Some("14. November 2023")));
    assert!(matches!(
        a.call("fill_sign_add", &json!({ "doc": doc, "page": 1, "type": "date", "at": [20, 260], "format": "Year" })),
        Err(ToolError::InvalidArgs(_))
    ));
    let list = ok(&mut a, "comment_list", json!({ "doc": doc }));
    assert_eq!(list["count"], 7);
    assert!(matches!(a.call("fill_sign_add", &json!({ "doc": doc, "page": 1, "type": "text", "at": [1, 1] })), Err(ToolError::InvalidArgs(_))));
}

#[test]
fn image_signatures_through_tools_preserve_transparency_and_survive_save() {
    let dir = workdir("image-signatures");
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 120, 40);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let rgba: Vec<u8> = (0..40)
            .flat_map(|y| (0..120).flat_map(move |x| if (40..80).contains(&x) && (10..30).contains(&y) { [0, 0, 0, 255] } else { [0, 0, 0, 0] }))
            .collect();
        encoder.write_header().unwrap().write_image_data(&rgba).unwrap();
    }
    std::fs::write(dir.join("signature.png"), png).unwrap();
    std::fs::write(dir.join("broken.png"), b"broken").unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_create", json!({ "from": "blank", "width": 200, "height": 300 }))["doc"].as_u64().unwrap();
    for (kind, y) in [("signature", 60), ("initials", 120)] {
        ok(&mut a, "fill_sign_add", json!({ "doc": doc, "page": 1, "type": kind, "at": [20, y], "path": "signature.png" }));
    }
    let render = |a: &mut Automation, doc| {
        let output = a.call("page_render", &json!({ "doc": doc, "page": 1, "dpi": 72 })).unwrap();
        let Content::Png { data, .. } = &output[0] else { panic!() };
        image::load_from_memory(data).unwrap().to_rgba8()
    };
    let before = render(&mut a, doc);
    assert_eq!(before.get_pixel(25, 60).0, [255, 255, 255, 255], "transparent margin exposes the page");
    assert_eq!(before.get_pixel(65, 60).0, [0, 0, 0, 255], "signature ink is embedded");
    assert_eq!(ok(&mut a, "edit_undo", json!({ "doc": doc }))["undone"], "Add initials");
    assert_eq!(ok(&mut a, "comment_list", json!({ "doc": doc }))["count"], 1);
    ok(&mut a, "edit_redo", json!({ "doc": doc }));
    // The same resize edit the selection handles use must keep the imported appearance.
    for rect in [json!([20, 40, 20, 88]), json!([20, 40, 100000000, 88])] {
        assert!(a.call("comment_edit", &json!({ "doc": doc, "page": 1, "index": 1, "rect": rect })).is_err());
    }
    ok(&mut a, "comment_lock", json!({ "doc": doc, "page": 1, "index": 1 }));
    assert!(a.call("comment_edit", &json!({ "doc": doc, "page": 1, "index": 1, "rect": [20, 40, 164, 88] })).is_err());
    ok(&mut a, "comment_lock", json!({ "doc": doc, "page": 1, "index": 1, "locked": false }));
    assert_eq!(render(&mut a, doc), before, "invalid and locked resizes leave the image intact");
    ok(&mut a, "comment_edit", json!({ "doc": doc, "page": 1, "index": 1, "rect": [20, 40, 164, 88] }));
    let resized = render(&mut a, doc);
    assert_eq!(resized.get_pixel(110, 64).0, [0, 0, 0, 255], "resizing scales the original ink");
    assert_eq!(resized.get_pixel(25, 64).0, [255, 255, 255, 255], "resizing retains alpha");
    assert_eq!(ok(&mut a, "edit_undo", json!({ "doc": doc }))["undone"], "Resize comment");
    assert_eq!(render(&mut a, doc), before);
    ok(&mut a, "edit_redo", json!({ "doc": doc }));
    assert_eq!(render(&mut a, doc), resized);
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "signed.pdf" }));
    let reopened = ok(&mut a, "doc_open", json!({ "path": "signed.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "comment_list", json!({ "doc": reopened }))["count"], 2);
    assert_eq!(render(&mut a, reopened), resized, "resized appearances retain alpha and geometry after save");
    for args in [
        json!({ "type": "signature", "path": "broken.png" }),
        json!({ "type": "signature", "path": "signature.png", "text": "Ada" }),
        json!({ "type": "check", "path": "signature.png" }),
        json!({ "type": "signature", "path": "../outside.png" }),
    ] {
        let mut args = args;
        args["doc"] = json!(doc);
        args["page"] = json!(1);
        args["at"] = json!([20, 80]);
        assert!(a.call("fill_sign_add", &args).is_err(), "{args}");
    }
    assert_eq!(ok(&mut a, "comment_list", json!({ "doc": doc }))["count"], 2, "errors leave the PDF intact");
}

#[test]
fn image_signature_preview_layers_are_read_only_and_survive_encrypted_save() {
    let dir = workdir("signature-preview");
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 120, 40);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let rgba: Vec<u8> = (0..40)
            .flat_map(|y| (0..120).flat_map(move |x| if (40..80).contains(&x) && (10..30).contains(&y) { [20, 40, 60, 128] } else { [0, 0, 0, 0] }))
            .collect();
        encoder.write_header().unwrap().write_image_data(&rgba).unwrap();
    }
    std::fs::write(dir.join("signature.png"), png).unwrap();
    std::fs::write(dir.join("form.pdf"), fixture(1)).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "form.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "page_rotate", json!({ "doc": doc, "pages": [1], "degrees": 90 }));
    ok(&mut a, "fill_sign_add", json!({ "doc": doc, "page": 1, "type": "initials", "at": [20, 40], "path": "signature.png" }));
    let background = a.call("page_render", &json!({ "doc": doc, "page": 1, "dpi": 72 })).unwrap();
    let Content::Png { data: expected, .. } = &background[0] else { panic!() };
    let expected = image::load_from_memory(expected).unwrap().to_rgba8();
    ok(&mut a, "fill_sign_add", json!({ "doc": doc, "page": 1, "type": "signature", "at": [120, 100], "path": "signature.png" }));
    ok(&mut a, "comment_edit", json!({ "doc": doc, "page": 1, "index": 2, "opacity": 0.5 }));
    ok(&mut a, "doc_protect", json!({ "doc": doc, "open_password": "preview-test" }));
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "signed.pdf" }));
    let reopened = ok(&mut a, "doc_open", json!({ "path": "signed.pdf", "password": "preview-test" }))["doc"].as_u64().unwrap();
    for doc in [doc, reopened] {
        let before = ok(&mut a, "doc_info", json!({ "doc": doc }));
        let d = a.session().docs().iter().find(|d| d.id.0 == doc).unwrap();
        let generation = d.edit_generation();
        let bytes = d.bytes.clone();
        let output = a.call("comment_image_preview", &json!({ "doc": doc, "page": 1, "index": 2, "dpi": 72 })).unwrap();
        let [Content::Json(meta), Content::Png { data: background, .. }] = output.as_slice() else { panic!("labeled background layer") };
        let image = a.call("comment_image_preview", &json!({ "doc": doc, "page": 1, "index": 2, "layer": "image" })).unwrap();
        let [Content::Json(image_meta), Content::Png { data: signature, width, height }] = image.as_slice() else { panic!("labeled image layer") };
        assert_eq!(meta["layer"], "background");
        assert_eq!(image_meta["layer"], "image");
        assert_eq!(meta["rotation"], 90);
        assert_eq!(meta["image_rotation"], 0, "placed upright as displayed on the turned page");
        assert_eq!(meta["opacity"], 0.5);
        assert_eq!((*width, *height), (120, 40));
        assert_eq!(image::load_from_memory(background).unwrap().to_rgba8(), expected, "page text and the other signature remain");
        let signature = image::load_from_memory(signature).unwrap().to_rgba8();
        assert_eq!(signature.get_pixel(5, 20).0[3], 0);
        assert_eq!(signature.get_pixel(60, 20).0, [20, 40, 60, 128], "embedded alpha and colour survive reopen");
        assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc })), before);
        let d = a.session().docs().iter().find(|d| d.id.0 == doc).unwrap();
        assert_eq!(d.edit_generation(), generation);
        assert!(std::sync::Arc::ptr_eq(&bytes, &d.bytes));
        assert!(d.image_signature_preview(0, 1).unwrap().unwrap().render_background(f32::NAN).is_err());
        assert!(d.image_signature_preview(0, 999).is_err());
    }
    assert!(tools().iter().find(|t| t.name == "comment_image_preview").unwrap().read_only);
    for dpi in [0, 601] {
        assert!(matches!(
            a.call("comment_image_preview", &json!({ "doc": reopened, "page": 1, "index": 2, "dpi": dpi })),
            Err(ToolError::InvalidArgs(_))
        ));
    }
    ok(&mut a, "fill_sign_add", json!({ "doc": reopened, "page": 1, "type": "check", "at": [10, 10] }));
    assert!(a.call("comment_image_preview", &json!({ "doc": reopened, "page": 1, "index": 3 })).is_err());
}

#[test]
fn creating_and_reducing_through_tools() {
    let dir = workdir("create");
    std::fs::write(dir.join("notes.txt"), "Meeting notes\nAction items").unwrap();
    let mut a = auto(&dir);
    let blank = ok(&mut a, "doc_create", json!({ "from": "blank", "pages": 2 }));
    assert_eq!((blank["pages"].as_u64(), blank["dirty"].as_bool()), (Some(2), Some(true)));
    let t = ok(&mut a, "doc_create", json!({ "from": "text", "path": "notes.txt" }))["doc"].as_u64().unwrap();
    assert_eq!(page_text(&mut a, t), ["Meeting notes\nAction items"]);
    ok(&mut a, "doc_save", json!({ "doc": t, "path": "notes.pdf" }));
    let r = ok(&mut a, "doc_reduce", json!({ "doc": t, "path": "notes-small.pdf" }));
    assert!(r["bytes_after"].as_u64().unwrap() > 0 && dir.join("notes-small.pdf").exists());
    assert!(matches!(a.call("doc_create", &json!({ "from": "images", "paths": ["notes.txt"] })), Err(ToolError::Failed(_))));
}

#[test]
fn creating_from_multiple_files_through_tools() {
    let dir = workdir("create-multiple");
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, 4, 2);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&[100; 4 * 2 * 3]).unwrap();
    }
    std::fs::write(dir.join("scan.png"), png).unwrap();
    std::fs::write(dir.join("notes.txt"), "hello").unwrap();
    std::fs::write(dir.join("report.docx"), b"PK\x03\x04").unwrap();
    let mut a = auto(&dir);

    // Combine: every file converted, in the order given, with a bookmark per file.
    let made = ok(
        &mut a,
        "doc_create_multiple",
        json!({ "paths": ["notes.txt", "a.pdf", "scan.png"], "pages": [null, "3", null], "out": "all.pdf", "open": true }),
    );
    let doc = made["document"]["doc"].as_u64().unwrap();
    assert_eq!(page_text(&mut a, doc), ["hello", "Page 3", ""]);
    let titles: Vec<String> = ok(&mut a, "bookmark_list", json!({ "doc": doc }))["bookmarks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(titles, ["notes", "a", "scan"]);
    assert!(dir.join("all.pdf").is_file());
    // A file that can't be converted fails the whole combine, naming it.
    let err = a.call("doc_create_multiple", &json!({ "paths": ["a.pdf", "report.docx"] })).unwrap_err();
    assert!(matches!(&err, ToolError::Failed(m) if m.contains("report.docx") && m.contains("can't be converted")), "{err}");
    assert!(matches!(a.call("doc_create_multiple", &json!({ "paths": [] })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(a.call("doc_create_multiple", &json!({ "paths": ["a.pdf"], "mode": "zip" })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(a.call("doc_create_multiple", &json!({ "paths": ["a.pdf"], "mode": "separate" })), Err(ToolError::InvalidArgs(_))));

    // Separate: one PDF per file; PDFs are skipped, a bad file doesn't stop the rest, and an
    // existing file is never overwritten.
    std::fs::create_dir_all(dir.join("out")).unwrap();
    std::fs::write(dir.join("out/notes.pdf"), b"mine").unwrap();
    let args = json!({ "paths": ["notes.txt", "report.docx", "a.pdf", "scan.png", "missing.txt"], "mode": "separate", "out_dir": "out" });
    let made = ok(&mut a, "doc_create_multiple", args);
    let files = made["files"].as_array().unwrap();
    assert!(files[0]["output"].as_str().unwrap().ends_with("notes (2).pdf"), "{files:?}");
    assert!(files[1]["error"].as_str().unwrap().contains("can't be converted"));
    assert_eq!(files[2]["skipped"], "already a PDF");
    assert!(files[3]["output"].as_str().unwrap().ends_with("scan.pdf"));
    assert!(files[4]["error"].is_string());
    assert_eq!(std::fs::read(dir.join("out/notes.pdf")).unwrap(), b"mine");
    let reopened = ok(&mut a, "doc_open", json!({ "path": "out/notes (2).pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(page_text(&mut a, reopened), ["hello"]);
    assert!(!dir.join("out/a.pdf").exists());
}

#[test]
fn creating_images_with_dpi_through_tools() {
    let dir = workdir("image-dpi");
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, 300, 150);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_pixel_dims(Some(png::PixelDimensions { xppu: 11811, yppu: 5906, unit: png::Unit::Meter }));
        enc.write_header().unwrap().write_image_data(&vec![100; 300 * 150 * 3]).unwrap();
    }
    std::fs::write(dir.join("scan.png"), png).unwrap();
    let mut a = auto(&dir);
    for (dpi, width, height) in [(None, 72.0, 72.0), (Some(72.0), 300.0, 150.0), (Some(300.0), 72.0, 36.0)] {
        let mut args = json!({ "from": "images", "paths": ["scan.png"] });
        if let Some(dpi) = dpi {
            args["dpi"] = json!(dpi);
        }
        let doc = ok(&mut a, "doc_create", args)["doc"].as_u64().unwrap();
        let info = ok(&mut a, "doc_info", json!({ "doc": doc }));
        assert!((info["pages"][0]["width"].as_f64().unwrap() - width).abs() < 0.02);
        assert!((info["pages"][0]["height"].as_f64().unwrap() - height).abs() < 0.02);
        ok(&mut a, "doc_save", json!({ "doc": doc, "path": "made.pdf" }));
        let reopened = ok(&mut a, "doc_open", json!({ "path": "made.pdf" }))["doc"].as_u64().unwrap();
        let render = a.call("page_render", &json!({ "doc": reopened, "page": 1, "dpi": 72 })).unwrap();
        let Content::Png { width: w, height: h, .. } = &render[0] else { panic!("expected PNG") };
        assert!((*w as f64 - width).abs() <= 1.0 && (*h as f64 - height).abs() <= 1.0);
    }
    for dpi in [0.0, -72.0, 1201.0] {
        assert!(a.call("doc_create", &json!({ "from": "images", "paths": ["scan.png"], "dpi": dpi })).is_err());
    }
}

#[test]
fn saving_with_flatten_fill_sign_bakes_marks_and_leaves_other_comments() {
    let dir = workdir("fill-sign-flatten");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "fill_sign_add", json!({ "doc": doc, "page": 1, "type": "text", "at": [40.0, 80.0], "text": "Hello" }));
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "highlight", "find": "Page", "contents": "keep" }));
    ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "text", "rect": [20.0, 120.0, 180.0, 142.0] }));
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "kept.pdf" }));
    let kept = ok(&mut a, "doc_open", json!({ "path": "kept.pdf" }))["doc"].as_u64().unwrap();
    let kept_comments = ok(&mut a, "comment_list", json!({ "doc": kept }))["comments"].as_array().unwrap().clone();
    assert!(kept_comments.iter().any(|c| c["contents"] == "Hello"), "without the flag the typewriter stays: {kept_comments:?}");

    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "flat.pdf", "flatten_fill_sign": true }));
    let flat = ok(&mut a, "doc_open", json!({ "path": "flat.pdf" }))["doc"].as_u64().unwrap();
    let comments = ok(&mut a, "comment_list", json!({ "doc": flat }))["comments"].as_array().unwrap().clone();
    assert!(comments.iter().all(|c| c["contents"] != "Hello"), "the typewriter was baked in: {comments:?}");
    assert!(comments.iter().any(|c| c["contents"] == "keep"), "the highlight stays: {comments:?}");
    let fields = ok(&mut a, "form_fields", json!({ "doc": flat }))["fields"].as_array().unwrap().clone();
    assert_eq!(fields.len(), 1, "the form field stays");
}

#[test]
fn flattening_through_tools() {
    let dir = workdir("flatten");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "textbox", "rect": [10, 200, 190, 240], "contents": "Approved" }));
    ok(&mut a, "doc_flatten", json!({ "doc": doc }));
    assert_eq!(ok(&mut a, "comment_list", json!({ "doc": doc }))["count"], 0);
    assert!(page_text(&mut a, doc)[0].contains("Approved"), "the text box is now page text");
    assert!(matches!(a.call("doc_flatten", &json!({ "doc": doc, "comments": false, "fields": false })), Err(ToolError::InvalidArgs(_))));
}

#[test]
fn replacing_pages_through_tools() {
    let dir = workdir("replace");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "page_replace", json!({ "doc": doc, "pages": [2, 3], "path": "b.pdf", "from_pages": [2, 1] }));
    assert_eq!(page_text(&mut a, doc), ["Page 1", "Page 2", "Page 1"]);
    assert!(
        matches!(a.call("page_replace", &json!({ "doc": doc, "pages": [1, 2, 3], "path": "b.pdf" })), Err(ToolError::Failed(_))),
        "b.pdf has only 2 pages"
    );
}

#[test]
fn preparing_a_form_through_tools() {
    let dir = workdir("prepare");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let add = |a: &mut Automation, args: Value| ok(a, "form_add_field", args)["field"].as_str().unwrap().to_owned();
    assert_eq!(add(&mut a, json!({ "doc": doc, "page": 1, "type": "text", "rect": [20, 20, 180, 42] })), "Text1");
    assert_eq!(add(&mut a, json!({ "doc": doc, "page": 1, "type": "combo", "rect": [20, 60, 180, 82], "options": ["Red", "Green"] })), "Dropdown1");
    assert_eq!(add(&mut a, json!({ "doc": doc, "page": 1, "type": "radio", "rect": [20, 100, 34, 114], "group": "size", "export": "S" })), "size");
    assert_eq!(add(&mut a, json!({ "doc": doc, "page": 1, "type": "radio", "rect": [40, 100, 54, 114], "group": "size", "export": "L" })), "size");
    let r = ok(&mut a, "form_set_props", json!({ "doc": doc, "field": "Text1", "name": "full name", "required": true, "tooltip": "Your name" }));
    assert_eq!(r["field"], "full name");
    ok(&mut a, "form_delete_field", json!({ "doc": doc, "field": "Dropdown1" }));
    ok(&mut a, "form_set_props", json!({ "doc": doc, "field": "full name", "rect": [30, 20, 190, 42] }));
    let fields = ok(&mut a, "form_fields", json!({ "doc": doc }));
    let f = fields["fields"].as_array().unwrap();
    assert_eq!(f.iter().map(|f| f["name"].as_str().unwrap()).collect::<Vec<_>>(), ["full name", "size"]);
    assert_eq!((f[0]["required"].as_bool(), f[0]["tooltip"].as_str()), (Some(true), Some("Your name")));
    assert_eq!(f[0]["rect"], json!([30.0, 20.0, 190.0, 42.0]), "moved; the rect round-trips in view coordinates");
    assert_eq!(f[1]["options"], json!(["S", "L"]));
    // #94: the mark a check box or radio button shows.
    assert_eq!(f[1]["check_style"], "circle", "radio buttons default to a circle");
    ok(&mut a, "form_set_props", json!({ "doc": doc, "field": "size", "check_style": "star" }));
    assert_eq!(ok(&mut a, "form_fields", json!({ "doc": doc }))["fields"][1]["check_style"], "star");
    assert!(matches!(a.call("form_set_props", &json!({ "doc": doc, "field": "size", "check_style": "heart" })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(a.call("form_set_props", &json!({ "doc": doc, "field": "full name", "check_style": "star" })), Err(ToolError::Failed(_))));
    ok(&mut a, "form_fill", json!({ "doc": doc, "values": { "full name": "Ada", "size": "L" } }));
    assert!(page_text(&mut a, doc)[0].contains("Ada"));
    // Options tab: alignment, default, flags (comb needs a limit).
    assert!(matches!(a.call("form_set_props", &json!({ "doc": doc, "field": "full name", "flags": { "comb": true } })), Err(ToolError::Failed(_))));
    ok(
        &mut a,
        "form_set_props",
        json!({ "doc": doc, "field": "full name", "align": "center", "default": "Anon", "max_length": 8, "flags": { "comb": true, "spell_check": false } }),
    );
    assert!(matches!(
        a.call("form_set_props", &json!({ "doc": doc, "field": "full name", "flags": { "sparkles": true } })),
        Err(ToolError::InvalidArgs(_))
    ));
    assert!(matches!(
        a.call("form_add_field", &json!({ "doc": doc, "page": 1, "type": "slider", "rect": [0, 0, 9, 9] })),
        Err(ToolError::InvalidArgs(_))
    ));
    assert!(matches!(a.call("form_delete_field", &json!({ "doc": doc, "field": "nope" })), Err(ToolError::Failed(_))));
    // An image field shows the picture it is given.
    assert_eq!(add(&mut a, json!({ "doc": doc, "page": 2, "type": "image", "rect": [20, 20, 120, 120] })), "Image1");
    let png = |a: &mut Automation| match a.call("page_render", &json!({ "doc": doc, "page": 2, "dpi": 36 })).unwrap().remove(0) {
        Content::Png { data, .. } => data,
        other => panic!("{other:?}"),
    };
    let before = png(&mut a);
    ok(&mut a, "doc_export_images", json!({ "doc": doc, "folder": "pics", "dpi": 18, "pages": [1] }));
    ok(&mut a, "form_set_image", json!({ "doc": doc, "field": "Image1", "path": "pics/a_page_1.png" }));
    let after = png(&mut a);
    assert_ne!(before, after, "the picture is drawn");
    assert_eq!(ok(&mut a, "edit_undo", json!({ "doc": doc }))["undone"], "Set the image of Image1");
    assert!(matches!(a.call("form_set_image", &json!({ "doc": doc, "field": "size", "path": "pics/a_page_1.png" })), Err(ToolError::Failed(_))));
}

#[test]
fn rotating_a_field_through_tools() {
    let dir = workdir("rotate-field");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "text", "rect": [20, 20, 180, 42], "name": "City" }));
    let rect_of = |a: &mut Automation| {
        let f = &ok(a, "form_fields", json!({ "doc": doc }))["fields"][0];
        (f["rotation"].as_i64().unwrap(), f["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>())
    };
    let (rot, before) = rect_of(&mut a);
    assert_eq!(rot, 0);
    assert!(matches!(a.call("form_set_props", &json!({ "doc": doc, "field": "City", "rotation": 45 })), Err(ToolError::InvalidArgs(_))));
    assert_eq!(rect_of(&mut a).1, before, "a rejected rotation changes nothing");
    ok(&mut a, "form_set_props", json!({ "doc": doc, "field": "City", "rotation": 90 }));
    let (rot, turned) = rect_of(&mut a);
    assert_eq!(rot, 90);
    let (bw, bh) = (before[2] - before[0], before[3] - before[1]);
    let (tw, th) = (turned[2] - turned[0], turned[3] - turned[1]);
    assert!((tw - bh).abs() < 0.2 && (th - bw).abs() < 0.2, "swapped {before:?} -> {turned:?}");
    let center = |r: &[f64]| ((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
    let (bc, tc) = (center(&before), center(&turned));
    assert!((bc.0 - tc.0).abs() < 0.2 && (bc.1 - tc.1).abs() < 0.2, "center moved {bc:?} -> {tc:?}");
    assert_eq!(ok(&mut a, "edit_undo", json!({ "doc": doc }))["undone"], "Change field properties");
    let (rot, back) = rect_of(&mut a);
    assert_eq!((rot, back), (0, before));
}

#[test]
fn redacting_through_tools() {
    let dir = workdir("redact");
    std::fs::write(dir.join("memo.txt"), "Call 555-123-4567 today\nSSN 123-45-6789 is private\nPublic line").unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_create", json!({ "from": "text", "path": "memo.txt" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "redact_mark", json!({ "doc": doc, "pattern": "phone" }));
    assert_eq!((r["marked"].as_u64(), r["marks_pending"].as_u64()), (Some(1), Some(1)));
    ok(&mut a, "redact_mark", json!({ "doc": doc, "pattern": "ssn", "overlay": "SSN" }));
    ok(&mut a, "redact_mark", json!({ "doc": doc, "find": "private" }));
    assert_eq!(page_text(&mut a, doc)[0].matches("555-123-4567").count(), 1, "marks alone remove nothing");
    let r = ok(&mut a, "redact_apply", json!({ "doc": doc }));
    assert_eq!((r["applied"].as_u64(), r["marks_pending"].as_u64()), (Some(3), Some(0)));
    let text = page_text(&mut a, doc)[0].clone();
    assert!(!text.contains("555") && !text.contains("6789") && !text.contains("private"), "{text}");
    assert!(text.contains("Call") && text.contains("today") && text.contains("Public line"), "{text}");
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "memo.pdf" }));
    let bytes = std::fs::read(dir.join("memo.pdf")).unwrap();
    assert!(!bytes.windows(4).any(|w| w == b"4567"), "the saved file has no trace of the number");
    assert!(matches!(a.call("redact_apply", &json!({ "doc": doc })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("redact_mark", &json!({ "doc": doc, "find": "nowhere to be found" })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("redact_mark", &json!({ "doc": doc })), Err(ToolError::InvalidArgs(_))));
}

/// Redaction codes and styled overlay text are burned into the page; locale pattern sets mark
/// their formats. All values are synthetic.
#[test]
fn redaction_codes_overlay_styles_and_locale_patterns_through_tools() {
    let dir = workdir("redact-codes");
    std::fs::write(dir.join("memo.txt"), "Informant Zebulon Quarrytree met us\nOffice SW1A 1AA, NI AB 12 34 56 C\nOpen paragraph").unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_create", json!({ "from": "text", "path": "memo.txt" }))["doc"].as_u64().unwrap();
    let sets = ok(&mut a, "redact_codes", json!({}));
    let foia = sets["code_sets"].as_array().unwrap().iter().find(|s| s["id"] == "us-foia").unwrap().clone();
    assert_eq!(foia["codes"].as_array().unwrap().len(), 14);
    assert!(foia["codes"].as_array().unwrap().iter().any(|c| c["code"] == "(b)(7)(C)"));
    let gb = ok(&mut a, "redact_patterns", json!({ "locale": "en-GB" }));
    assert_eq!(gb["locales"][0]["patterns"].as_array().unwrap().len(), 2, "{gb}");

    let r = ok(
        &mut a,
        "redact_mark",
        json!({ "doc": doc, "find": "Zebulon Quarrytree", "code_set": "us-foia", "codes": ["(b)(6)", "(b)(7)(c)"],
                "overlay_font": "courier", "overlay_size": 9, "overlay_color": "#0000FF", "overlay_align": "left", "fill": "black" }),
    );
    assert_eq!(r["overlay"], "(b)(6), (b)(7)(C)", "codes are written in their set's form");
    let r = ok(&mut a, "redact_mark", json!({ "doc": doc, "locale": "en-GB", "codes": ["PRIV"], "overlay_color": "white" }));
    assert_eq!(r["marked"].as_u64(), Some(2), "the postcode and the NI number: {r}");
    // Rejected: unknown codes, both custom text and codes, a set without codes, unknown locales,
    // sizes out of range. Nothing is marked by a rejected call.
    for bad in [
        json!({ "doc": doc, "find": "Open", "code_set": "us-foia", "codes": ["(k)(2)"] }),
        json!({ "doc": doc, "find": "Open", "overlay": "X", "codes": ["(b)(6)"] }),
        json!({ "doc": doc, "find": "Open", "code_set": "us-foia" }),
        json!({ "doc": doc, "find": "Open", "code_set": "nope", "codes": ["1"] }),
        json!({ "doc": doc, "locale": "xx-YY" }),
        json!({ "doc": doc, "find": "Open", "overlay": "X", "overlay_size": 1 }),
        json!({ "doc": doc, "find": "Open", "overlay": "X", "overlay_align": "middle" }),
        json!({ "doc": doc, "pattern": "uk-postcode", "locale": "en-GB" }),
    ] {
        assert!(matches!(a.call("redact_mark", &bad), Err(ToolError::InvalidArgs(_))), "{bad}");
    }
    assert!(matches!(a.call("redact_codes", &json!({ "code_set": "nope" })), Err(ToolError::InvalidArgs(_))));

    let r = ok(&mut a, "redact_apply", json!({ "doc": doc }));
    assert_eq!((r["applied"].as_u64(), r["marks_pending"].as_u64()), (Some(3), Some(0)));
    let text = page_text(&mut a, doc)[0].clone();
    for gone in ["Zebulon", "Quarrytree", "SW1A", "1AA", "AB 12", "56 C"] {
        assert!(!text.contains(gone), "{gone:?} remains: {text}");
    }
    assert!(text.contains("(b)(6), (b)(7)(C)") && text.contains("PRIV"), "overlay text is drawn into the page: {text}");
    assert!(text.contains("Informant") && text.contains("Open paragraph"), "{text}");
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "memo.pdf" }));
    let bytes = std::fs::read(dir.join("memo.pdf")).unwrap();
    assert!(!bytes.windows(10).any(|w| w == b"Quarrytree"), "the saved file has no trace of the name");
}

#[test]
fn removing_hidden_information_through_tools() {
    let dir = workdir("hidden");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "note", "at": [20, 20], "contents": "internal note" }));
    ok(&mut a, "doc_set_info", json!({ "doc": doc, "key": "Title", "value": "Secret plan" }));
    let info = ok(&mut a, "doc_hidden_info", json!({ "doc": doc }));
    let count = |v: &Value, c: &str| v["categories"].as_array().unwrap().iter().find(|x| x["category"] == c).unwrap()["count"].as_u64().unwrap();
    assert!(count(&info, "comments") >= 1 && count(&info, "metadata") >= 1, "{info}");
    let r = ok(&mut a, "doc_remove_hidden", json!({ "doc": doc, "categories": ["comments"] }));
    assert_eq!(r["undo"], "Remove hidden information");
    assert_eq!(ok(&mut a, "comment_list", json!({ "doc": doc }))["count"], 0);
    assert!(count(&ok(&mut a, "doc_hidden_info", json!({ "doc": doc })), "metadata") >= 1, "only comments went");
    let r = ok(&mut a, "doc_remove_hidden", json!({ "doc": doc }));
    assert_eq!(r["undo"], "Sanitize document");
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "clean.pdf" }));
    let bytes = std::fs::read(dir.join("clean.pdf")).unwrap();
    assert!(!bytes.windows(11).any(|w| w == b"Secret plan"), "the old revision is gone");
    assert!(matches!(a.call("doc_remove_hidden", &json!({ "doc": doc, "categories": ["nonsense"] })), Err(ToolError::InvalidArgs(_))));
}

#[test]
fn printing_through_tools() {
    let dir = workdir("print");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    assert!(ok(&mut a, "printers", json!({}))["printers"].is_array());
    let none = ok(&mut a, "printer_options", json!({ "printer": "No_Such_Queue_PdfCraft" }));
    assert_eq!((none["count"].as_u64(), none["options"].as_array().map(Vec::len)), (Some(0), Some(0)));
    assert!(matches!(a.call("printer_options", &json!({})), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(
        a.call("doc_print", &json!({ "doc": doc, "printer": "default", "options": ["InputSlot"] })),
        Err(ToolError::InvalidArgs(m)) if m.contains("printer_options")
    ));
    assert!(matches!(
        a.call("doc_print", &json!({ "doc": doc, "printer": "default", "options": { "InputSlot": 2 } })),
        Err(ToolError::InvalidArgs(m)) if m.contains("options.InputSlot")
    ));
    let n = ok(&mut a, "doc_info", json!({ "doc": doc }))["document"]["pages"].as_u64().unwrap();
    let r = ok(&mut a, "doc_print", json!({ "doc": doc, "layout": "multiple", "per_sheet": 4, "path": "sheets.pdf" }));
    assert_eq!(r["sheets"].as_u64(), Some(n.div_ceil(4)));
    let printed = ok(&mut a, "doc_open", json!({ "path": "sheets.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": printed }))["document"]["pages"].as_u64(), Some(n.div_ceil(4)));
    let r = ok(&mut a, "doc_print", json!({ "doc": doc, "pages": "1", "layout": "poster", "scale": 400, "path": "poster.pdf" }));
    assert!(r["sheets"].as_u64().unwrap() > 1);
    assert!(matches!(a.call("doc_print", &json!({ "doc": doc })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(a.call("doc_print", &json!({ "doc": doc, "pages": "99", "path": "x.pdf" })), Err(ToolError::InvalidArgs(_))));
}

#[test]
fn adding_content_through_tools() {
    let dir = workdir("content");
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, 40, 20);
        enc.set_color(png::ColorType::Rgb);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[200u8; 2400]).unwrap();
    }
    std::fs::write(dir.join("logo.png"), png).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "page_add_text", json!({ "doc": doc, "page": 1, "text": "CONFIDENTIAL", "at": [20, 20], "size": 18, "bold": true, "color": "red" }));
    let r = ok(&mut a, "page_add_image", json!({ "doc": doc, "page": 1, "path": "logo.png", "rect": [100, 200, 180, 240] }));
    assert_eq!(r["rect"], json!([100.0, 200.0, 180.0, 240.0]));
    assert!(page_text(&mut a, doc)[0].contains("CONFIDENTIAL"));
    let list = ok(&mut a, "content_list", json!({ "doc": doc }));
    assert_eq!(list["count"], 2);
    assert_eq!((list["items"][0]["type"].as_str(), list["items"][0]["bold"].as_bool()), (Some("text"), Some(true)));
    ok(&mut a, "content_update", json!({ "doc": doc, "page": 1, "index": 1, "text": "DRAFT", "font": "times" }));
    let text = page_text(&mut a, doc)[0].clone();
    assert!(text.contains("DRAFT") && !text.contains("CONFIDENTIAL"), "{text}");
    // Image tools: rotate, flip, crop, replace.
    ok(&mut a, "content_update", json!({ "doc": doc, "page": 1, "index": 2, "rotate": 90, "flip_h": true, "crop": [0.1, 0, 0.1, 0] }));
    ok(&mut a, "content_update", json!({ "doc": doc, "page": 1, "index": 2, "image": "logo.png" }));
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc }))["document"]["dirty"], true);
    assert!(matches!(a.call("content_update", &json!({ "doc": doc, "page": 1, "index": 2, "rotate": 45 })), Err(ToolError::InvalidArgs(_))));
    assert!(
        matches!(a.call("content_update", &json!({ "doc": doc, "page": 1, "index": 1, "rotate": 90 })), Err(ToolError::InvalidArgs(_))),
        "text doesn't rotate"
    );
    ok(&mut a, "content_delete", json!({ "doc": doc, "page": 1, "index": 2 }));
    assert_eq!(ok(&mut a, "content_list", json!({ "doc": doc }))["count"], 1);
    assert!(matches!(a.call("content_delete", &json!({ "doc": doc, "page": 1, "index": 5 })), Err(ToolError::Failed(_))));
}

#[test]
fn form_formats_and_calculations_through_tools() {
    let dir = workdir("formcalc");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    for (name, y) in [("Price", 20), ("Qty", 60), ("Total", 100)] {
        ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "text", "rect": [20, y, 180, y + 22], "name": name }));
    }
    ok(
        &mut a,
        "form_set_props",
        json!({ "doc": doc, "field": "Price", "format": { "type": "number", "decimals": 2, "currency": "$" }, "validate": { "min": 0 } }),
    );
    ok(
        &mut a,
        "form_set_props",
        json!({ "doc": doc, "field": "Total", "format": { "type": "number", "decimals": 2, "currency": "$" }, "calculate": { "notation": "Price * Qty" } }),
    );
    ok(&mut a, "form_fill", json!({ "doc": doc, "values": { "Price": "19.99", "Qty": "3" } }));
    let f = ok(&mut a, "form_fields", json!({ "doc": doc }));
    let total = f["fields"].as_array().unwrap().iter().find(|x| x["name"] == "Total").unwrap().clone();
    assert_eq!((total["value"].as_str(), total["display"].as_str()), (Some("59.97"), Some("$59.97")), "{total}");
    assert_eq!(total["calculate"]["notation"], "Price * Qty");
    assert!(page_text(&mut a, doc)[0].contains("$59.97"));
    let err = a.call("form_fill", &json!({ "doc": doc, "values": { "Price": "-5" } })).unwrap_err();
    assert!(err.to_string().contains("greater than or equal to 0"), "{err}");
    assert!(matches!(
        a.call("form_set_props", &json!({ "doc": doc, "field": "Qty", "format": { "type": "roman" } })),
        Err(ToolError::InvalidArgs(_))
    ));
}

#[test]
fn tab_order_and_field_appearance_through_tools() {
    let dir = workdir("taborder");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    // Added out of reading order: B (top right), A (top left), C (below).
    for (name, x, y) in [("B", 110, 20), ("A", 10, 20), ("C", 10, 60)] {
        ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "text", "rect": [x, y, x + 80, y + 20], "name": name }));
    }
    let r = ok(&mut a, "form_tab_order", json!({ "doc": doc, "order": "row" }));
    assert_eq!(r["tab_order"], json!(["A", "B", "C"]));
    let r = ok(&mut a, "form_tab_order", json!({ "doc": doc, "order": "column" }));
    assert_eq!(r["tab_order"], json!(["A", "C", "B"]));
    let r = ok(&mut a, "form_tab_order", json!({ "doc": doc, "field": "B", "move": "earlier" }));
    assert_eq!(r["tab_order"], json!(["A", "B", "C"]), "ordered manually");
    let r = ok(&mut a, "form_tab_order", json!({ "doc": doc, "order": "column" }));
    assert_eq!(r["tab_order"], json!(["A", "C", "B"]));
    ok(
        &mut a,
        "form_set_props",
        json!({ "doc": doc, "field": "A", "appearance": { "border": "red", "fill": "none", "style": "underline", "font": "courier" } }),
    );
    ok(&mut a, "form_fill", json!({ "doc": doc, "values": { "A": "typed" } }));
    assert!(page_text(&mut a, doc)[0].contains("typed"));
    assert!(matches!(
        a.call("form_set_props", &json!({ "doc": doc, "field": "A", "appearance": { "style": "wavy" } })),
        Err(ToolError::InvalidArgs(_))
    ));
}

#[test]
fn comments_and_form_data_travel_as_xfdf_fdf_and_text() {
    let dir = workdir("xfdf");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 2, "type": "note", "at": [30, 30], "contents": "Please review", "author": "Ada" }));
    ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "text", "rect": [20, 20, 180, 42], "name": "City" }));
    ok(&mut a, "form_fill", json!({ "doc": doc, "values": { "City": "Paris" } }));
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "reviewed.pdf" }));
    ok(&mut a, "doc_export_data", json!({ "doc": doc, "path": "all.xfdf" }));
    ok(&mut a, "doc_export_data", json!({ "doc": doc, "path": "data.txt", "what": "fields" }));
    assert_eq!(std::fs::read_to_string(dir.join("data.txt")).unwrap(), "City\nParis\n");
    assert!(matches!(a.call("doc_export_data", &json!({ "doc": doc, "path": "c.csv", "what": "comments" })), Err(ToolError::InvalidArgs(_))));
    // A copy without the comment and with the field empty takes both back.
    ok(&mut a, "comment_delete", json!({ "doc": doc, "page": 2, "index": 1 }));
    ok(&mut a, "form_reset", json!({ "doc": doc }));
    let r = ok(&mut a, "doc_import_data", json!({ "doc": doc, "path": "all.xfdf" }));
    assert_eq!(r["filled_fields"], 1);
    let list = ok(&mut a, "comment_list", json!({ "doc": doc }));
    assert_eq!(list["comments"][0]["contents"], "Please review", "{list}");
    assert_eq!(r["undo"], "Import all.xfdf");
}

#[test]
fn natural_image_stamps_through_tools_on_rotated_pages() {
    for degrees in [0, 90, 180, 270] {
        let dir = workdir(&format!("natural-stamps-{degrees}"));
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 80, 40);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let pixels: Vec<u8> = (0..40)
                .flat_map(|y| {
                    (0..80).flat_map(move |x| {
                        [[240, 20, 20], [20, 180, 20], [20, 20, 240], [230, 180, 20]][usize::from(y >= 20) * 2 + usize::from(x >= 40)]
                    })
                })
                .collect();
            encoder.write_header().unwrap().write_image_data(&pixels).unwrap();
        }
        std::fs::write(dir.join("quadrants.png"), bytes).unwrap();
        let mut a = auto(&dir);
        let doc = ok(&mut a, "doc_open", json!({"path":"a.pdf"}))["doc"].as_u64().unwrap();
        ok(&mut a, "page_rotate", json!({"doc":doc,"pages":[1],"degrees":degrees}));
        ok(&mut a, "stamp_custom", json!({"doc":doc,"page":1,"path":"quadrants.png","at":[100,100]}));
        let rendered = a.call("page_render", &json!({"doc":doc,"page":1,"dpi":72})).unwrap();
        let Content::Png { data, .. } = &rendered[0] else { panic!("expected PNG") };
        let mut reader = png::Decoder::new(std::io::Cursor::new(data)).read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!(info.color_type, png::ColorType::Rgba);
        for ((x, y), colour) in [(80, 90), (120, 90), (80, 110), (120, 110)].into_iter().zip([
            [240, 20, 20, 255],
            [20, 180, 20, 255],
            [20, 20, 240, 255],
            [230, 180, 20, 255],
        ]) {
            let offset = ((y * info.width + x) * 4) as usize;
            assert_eq!(&pixels[offset..offset + 4], &colour, "page rotation {degrees}");
        }
    }
}

#[test]
fn stamps_through_tools() {
    let dir = workdir("stamps");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "stamp", "stamp": "approved", "at": [100, 100] }));
    ok(
        &mut a,
        "comment_add",
        json!({ "doc": doc, "page": 1, "type": "stamp", "stamp": "reviewed", "dynamic": true, "at": [100, 200], "author": "Ada" }),
    );
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "stamp", "stamp": "sign here", "at": [100, 250] }));
    let list = ok(&mut a, "comment_list", json!({ "doc": doc }));
    assert_eq!(list["count"], 3, "{list}");
    let text = page_text(&mut a, doc)[0].clone();
    assert!(text.contains("APPROVED") && text.contains("REVIEWED") && text.contains("By Ada at") && text.contains("SIGN HERE"), "{text}");
    assert!(matches!(
        a.call("comment_add", &json!({ "doc": doc, "page": 1, "type": "stamp", "stamp": "nonsense", "at": [1, 1] })),
        Err(ToolError::InvalidArgs(_))
    ));
    // A custom stamp from another PDF's page.
    ok(&mut a, "stamp_custom", json!({ "doc": doc, "page": 2, "path": "b.pdf", "file_page": 2, "at": [100, 150], "name": "Logo" }));
    let list = ok(&mut a, "comment_list", json!({ "doc": doc }));
    assert_eq!(list["count"], 4, "{list}");
    // Its "Page 2" is drawn over the page's own.
    assert_eq!(page_text(&mut a, doc)[1].matches('2').count(), 2);
    assert_eq!(ok(&mut a, "edit_undo", json!({ "doc": doc }))["undone"], "Add stamp");
    assert!(matches!(a.call("stamp_custom", &json!({ "doc": doc, "page": 1, "path": "nope.png", "at": [1, 1] })), Err(ToolError::Failed(_))));
}

#[test]
fn organizing_with_filters_bookmark_splits_and_extract_options() {
    let dir = workdir("organize2");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    // Rotate the even page numbers only (a.pdf has 3 pages: page 2 is the only even one).
    let r = ok(&mut a, "page_rotate", json!({ "doc": doc, "degrees": 90, "subset": "even" }));
    assert_eq!(r["rotated"], 1);
    let info = ok(&mut a, "doc_info", json!({ "doc": doc }));
    assert_eq!(info["pages"][1]["rotation"], 90);
    assert_eq!(info["pages"][0]["rotation"], 0);
    // Split at top-level bookmarks: parts named after them.
    ok(&mut a, "bookmark_add", json!({ "doc": doc, "title": "Start", "page": 1 }));
    ok(&mut a, "bookmark_add", json!({ "doc": doc, "title": "End/Part", "page": 3 }));
    let s = ok(&mut a, "doc_split", json!({ "doc": doc, "bookmarks": true, "out_dir": "parts" }));
    let files: Vec<String> = s["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| std::path::Path::new(f["path"].as_str().unwrap()).file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files, ["a-Start.pdf", "a-End_Part.pdf"]);
    let s = ok(&mut a, "doc_split", json!({ "doc": doc, "max_mb": 0.0001, "out_dir": "sized" }));
    assert_eq!(s["files"].as_array().unwrap().len(), 3, "tiny limit: a page per file");
    // Extract as separate files and delete them from the original.
    let e = ok(&mut a, "page_extract", json!({ "doc": doc, "pages": [1, 2], "separate": true, "out_dir": "pages", "delete": true }));
    assert_eq!(e["files"].as_array().unwrap().len(), 2);
    assert_eq!(e["original"]["pages"], 1);
    assert!(matches!(a.call("page_extract", &json!({ "doc": doc, "pages": [1], "separate": true })), Err(ToolError::InvalidArgs(_))));
}

#[test]
fn bookmark_splits_preserve_parts_with_colliding_names() {
    let dir = workdir("split-name-collisions");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    // Duplicate titles and different titles that sanitize to the same filename must both work.
    for (page, title) in [(1, "Chapter/One"), (2, "Chapter/One"), (3, "Chapter:One")] {
        ok(&mut a, "bookmark_add", json!({ "doc": doc, "title": title, "page": page }));
    }
    std::fs::create_dir(dir.join("parts")).unwrap();
    let existing = dir.join("parts/a-Chapter_One.pdf");
    std::fs::write(&existing, b"KEEP EXISTING FILE").unwrap();
    let split = ok(&mut a, "doc_split", json!({ "doc": doc, "bookmarks": true, "out_dir": "parts" }));
    let files = split["files"].as_array().unwrap();
    assert_eq!(files.len(), 3);
    let mut paths = std::collections::HashSet::new();
    for (i, file) in files.iter().enumerate() {
        let path = file["path"].as_str().unwrap();
        assert!(paths.insert(path), "split parts reused {path}");
        let part = ok(&mut a, "doc_open", json!({ "path": path }))["doc"].as_u64().unwrap();
        assert_eq!(page_text(&mut a, part), [format!("Page {}", i + 1)]);
    }
    assert_eq!(std::fs::read(existing).unwrap(), b"KEEP EXISTING FILE");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn content_edits_reject_hostile_indexes_without_changing_the_document() {
    let dir = workdir("content-hostile-indexes");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "page_add_text", json!({ "doc": doc, "page": 1, "at": [10, 10], "text": "Keep me" }));
    let before = ok(&mut a, "content_list", json!({ "doc": doc }));
    for tool in ["content_update", "content_delete"] {
        for index in [i64::MIN, -1, 0, (1_i64 << 32) + 1, i64::MAX] {
            assert!(a.call(tool, &json!({ "doc": doc, "page": 1, "index": index })).is_err(), "{tool} accepted {index}");
            assert_eq!(ok(&mut a, "content_list", json!({ "doc": doc })), before);
        }
    }
    ok(&mut a, "content_delete", json!({ "doc": doc, "page": 1, "index": 1 }));
    assert_eq!(ok(&mut a, "content_list", json!({ "doc": doc }))["count"], 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn links_through_tools() {
    let dir = workdir("links");
    std::fs::write(dir.join("notes.txt"), "Docs at https://example.org/docs and www.rust-lang.org").unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_create", json!({ "from": "text", "path": "notes.txt" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "links_from_urls", json!({ "doc": doc }));
    assert_eq!(r["created"], json!(["https://example.org/docs", "http://www.rust-lang.org"]));
    ok(&mut a, "link_add", json!({ "doc": doc, "page": 1, "rect": [72, 300, 200, 320], "to_page": 1, "visible": true, "color": "red" }));
    let list = ok(&mut a, "link_list", json!({ "doc": doc }));
    assert_eq!(list["count"], 3);
    let added = list["links"].as_array().unwrap().iter().find(|l| l["to_page"] == 1).unwrap().clone();
    assert_eq!(added["rect"], json!([72.0, 300.0, 200.0, 320.0]));
    ok(&mut a, "link_edit", json!({ "doc": doc, "page": 1, "index": added["index"], "url": "https://pdfcraft.dev" }));
    let list = ok(&mut a, "link_list", json!({ "doc": doc }));
    assert!(list["links"].as_array().unwrap().iter().any(|l| l["url"] == "https://pdfcraft.dev"));
    ok(&mut a, "link_delete", json!({ "doc": doc, "page": 1, "index": added["index"] }));
    let r = ok(&mut a, "links_remove", json!({ "doc": doc }));
    assert_eq!(r["removed"], 2);
    assert_eq!(ok(&mut a, "link_list", json!({ "doc": doc }))["count"], 0);
    assert!(matches!(a.call("link_add", &json!({ "doc": doc, "page": 1, "rect": [0, 0, 50, 20] })), Err(ToolError::InvalidArgs(_))));
}

/// doc_info reports annotation and link rectangles in the tools' displayed-page convention,
/// the same values comment_list and link_list give, so they can be fed back to link_edit (#129).
#[test]
fn doc_info_rects_are_displayed_page_coordinates() {
    let dir = workdir("info-rects");
    let mut a = auto(&dir);
    // Page 2 is rotated so an unrotated-only y flip cannot pass.
    let doc = ok(&mut a, "doc_create", json!({ "from": "blank", "width": 612, "height": 792, "pages": 2 }))["doc"].as_u64().unwrap();
    ok(&mut a, "page_rotate", json!({ "doc": doc, "pages": [2], "degrees": 90 }));
    for page in 1..=2 {
        ok(
            &mut a,
            "comment_add",
            json!({ "doc": doc, "page": page, "type": "note", "at": [72, 72], "author": "Example", "contents": "Fixture note" }),
        );
        ok(&mut a, "link_add", json!({ "doc": doc, "page": page, "rect": [72, 100, 200, 120], "url": "https://example.com" }));
    }
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "fixture.pdf" }));
    let re = ok(&mut a, "doc_open", json!({ "path": "fixture.pdf" }))["doc"].as_u64().unwrap();
    let info = ok(&mut a, "doc_info", json!({ "doc": re }));
    let comments = ok(&mut a, "comment_list", json!({ "doc": re }));
    let links = ok(&mut a, "link_list", json!({ "doc": re }));
    let close = |a: &Value, b: &Value| {
        let (a, b) = (a.as_array().unwrap(), b.as_array().unwrap());
        a.len() == 4 && a.iter().zip(b).all(|(x, y)| (x.as_f64().unwrap() - y.as_f64().unwrap()).abs() < 0.01)
    };
    for page in 1..=2 {
        let page = json!(page);
        let find = |items: &Value| items.as_array().unwrap().iter().find(|x| x["page"] == page).unwrap()["rect"].clone();
        let (info_note, note) = (find(&info["annotations"]), find(&comments["comments"]));
        assert!(close(&info_note, &note), "page {page}: doc_info note {info_note} vs comment_list {note}");
        if page == 1 {
            assert!(close(&note, &json!([72.0, 72.0, 92.0, 92.0])), "note {note}");
        }
        let (info_link, link) = (find(&info["links"]), find(&links["links"]));
        assert!(close(&info_link, &link), "page {page}: doc_info link {info_link} vs link_list {link}");
        assert!(close(&link, &json!([72.0, 100.0, 200.0, 120.0])), "page {page}: link {link}");
    }
    // Reusing the doc_info rectangle in a geometry-taking edit leaves the link where it is.
    let rotated = links["links"].as_array().unwrap().iter().find(|l| l["page"] == 2).unwrap().clone();
    let from_info = info["links"].as_array().unwrap().iter().find(|l| l["page"] == 2).unwrap()["rect"].clone();
    ok(&mut a, "link_edit", json!({ "doc": re, "page": 2, "index": rotated["index"], "rect": from_info }));
    let after = ok(&mut a, "link_list", json!({ "doc": re }));
    let moved = after["links"].as_array().unwrap().iter().find(|l| l["page"] == 2).unwrap()["rect"].clone();
    assert!(close(&moved, &rotated["rect"]), "link moved: {moved} vs {}", rotated["rect"]);
}

/// `comment_add` places a note's or an attachment's icon with its displayed top-left corner at
/// `at`, on rotated pages too. The engine anchors the icon at the user-space top-left of its
/// `/Rect`, which after `/Rotate` is another corner of the square as displayed; converting the
/// point alone put the icon one icon-width off.
#[test]
fn note_icons_anchor_at_the_requested_corner_on_rotated_pages() {
    let dir = workdir("note-anchor");
    std::fs::write(dir.join("note.txt"), b"attached").unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_create", json!({ "from": "blank", "width": 612, "height": 792, "pages": 4 }))["doc"].as_u64().unwrap();
    for (page, degrees) in [(2, 90), (3, 180), (4, 270)] {
        ok(&mut a, "page_rotate", json!({ "doc": doc, "pages": [page], "degrees": degrees }));
    }
    for page in 1..=4 {
        ok(&mut a, "comment_add", json!({ "doc": doc, "page": page, "type": "note", "at": [72, 72], "contents": "Fixture note" }));
        ok(&mut a, "comment_add", json!({ "doc": doc, "page": page, "type": "attachment", "at": [200, 300], "path": "note.txt" }));
    }
    let comments = ok(&mut a, "comment_list", json!({ "doc": doc }));
    let comments = comments["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 8);
    for c in comments {
        let want = if c["type"] == "Text" { [72.0, 72.0, 92.0, 92.0] } else { [200.0, 300.0, 220.0, 320.0] };
        let rect: Vec<f64> = c["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        assert!(rect.iter().zip(want).all(|(x, y)| (x - y).abs() < 0.01), "page {} {}: rect {rect:?}, want {want:?}", c["page"], c["type"]);
    }
}

/// An image signature is drawn upright as displayed, and anchored at the displayed point asked
/// for, on every `/Rotate`: its picture is placed in user space, which the page turns, so the
/// appearance is counter-rotated and its box is found in user space.
#[test]
fn image_signatures_stay_upright_on_rotated_pages() {
    let dir = workdir("image-upright");
    // 80 x 40 px, one colour per quadrant: red and green above, blue and yellow below.
    let quadrants = image::RgbaImage::from_fn(80, 40, |x, y| match (x < 40, y < 20) {
        (true, true) => image::Rgba([255, 0, 0, 255]),
        (false, true) => image::Rgba([0, 255, 0, 255]),
        (true, false) => image::Rgba([0, 0, 255, 255]),
        (false, false) => image::Rgba([255, 255, 0, 255]),
    });
    quadrants.save(dir.join("quadrants.png")).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_create", json!({ "from": "blank", "width": 200, "height": 300, "pages": 4 }))["doc"].as_u64().unwrap();
    for (page, degrees) in [(2, 90), (3, 180), (4, 270)] {
        ok(&mut a, "page_rotate", json!({ "doc": doc, "pages": [page], "degrees": degrees }));
    }
    for page in 1..=4 {
        // 64 x 32 pt from x = 60 as displayed, centred on y = 140.
        ok(&mut a, "fill_sign_add", json!({ "doc": doc, "page": page, "type": "signature", "at": [60, 140], "path": "quadrants.png" }));
    }
    let (white, red, green, blue, yellow) = ([255, 255, 255], [255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]);
    let check = |a: &mut Automation, doc: u64| {
        for page in 1..=4 {
            let output = a.call("page_render", &json!({ "doc": doc, "page": page, "dpi": 72 })).unwrap();
            let Content::Png { data, .. } = &output[0] else { panic!() };
            let pixels = image::load_from_memory(data).unwrap().to_rgba8();
            for (x, y, want, what) in [
                (76, 132, red, "signature top-left"),
                (108, 132, green, "signature top-right"),
                (76, 148, blue, "signature bottom-left"),
                (108, 148, yellow, "signature bottom-right"),
                (56, 140, white, "left of the signature"),
                (128, 140, white, "right of the signature"),
                (92, 120, white, "above the signature"),
                (92, 160, white, "below the signature"),
            ] {
                let got = pixels.get_pixel(x, y).0;
                assert!(got[..3].iter().zip(want).all(|(g, w)| g.abs_diff(w) < 12), "page {page}, {what} at ({x}, {y}): {got:?}, want {want:?}");
            }
        }
    };
    check(&mut a, doc);
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "placed.pdf" }));
    let reopened = ok(&mut a, "doc_open", json!({ "path": "placed.pdf" }))["doc"].as_u64().unwrap();
    check(&mut a, reopened);
}

/// Issue #299: a typed signature and initials read across, as displayed, on every `/Rotate`,
/// anchored at the displayed point, and look the same as on an unturned page.
#[test]
fn typed_signatures_stay_upright_on_rotated_pages() {
    let dir = workdir("typed-upright");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_create", json!({ "from": "blank", "width": 200, "height": 300, "pages": 4 }))["doc"].as_u64().unwrap();
    for (page, degrees) in [(2, 90), (3, 180), (4, 270)] {
        ok(&mut a, "page_rotate", json!({ "doc": doc, "pages": [page], "degrees": degrees }));
    }
    for page in 1..=4 {
        ok(&mut a, "fill_sign_add", json!({ "doc": doc, "page": page, "type": "signature", "at": [30, 60], "text": "Ada Lovelace" }));
        ok(&mut a, "fill_sign_add", json!({ "doc": doc, "page": page, "type": "initials", "at": [30, 140], "text": "AL" }));
    }
    let check = |a: &mut Automation, doc: u64| {
        let mut upright = Vec::new();
        for page in 1..=4 {
            let output = a.call("page_render", &json!({ "doc": doc, "page": page, "dpi": 72 })).unwrap();
            let Content::Png { data, .. } = &output[0] else { panic!() };
            let pixels = image::load_from_memory(data).unwrap().to_rgba8();
            assert_eq!(pixels.dimensions(), if page % 2 == 1 { (200, 300) } else { (300, 200) }, "page {page}");
            let dark: Vec<bool> = pixels.pixels().map(|p| p.0[..3].iter().all(|v| *v < 128)).collect();
            for y in [60, 140] {
                let mut b = [u32::MAX, u32::MAX, 0, 0];
                for (x, py, p) in pixels.enumerate_pixels() {
                    if py.abs_diff(y) < 40 && p.0[..3].iter().all(|v| *v < 128) {
                        b = [b[0].min(x), b[1].min(py), b[2].max(x), b[3].max(py)];
                    }
                }
                assert!(b[0] <= b[2], "page {page}: no ink near y = {y}");
                assert!(b[2] - b[0] > b[3] - b[1], "page {page}: the name reads across: {b:?}");
                assert!(b[0].abs_diff(30) <= 2 && b[1] < y && b[3] > y, "page {page}: anchored at (30, {y}): {b:?}");
            }
            if page == 1 {
                upright = dark;
                continue;
            }
            // Same pixels as the unturned page where both pages are (pages 2 and 4 are wider).
            let width = pixels.width();
            let (mut differ, mut ink) = (0, 0);
            for y in 0..200 {
                for x in 0..200 {
                    let (want, got) = (upright[(y * 200 + x) as usize], dark[(y * width + x) as usize]);
                    ink += usize::from(want);
                    differ += usize::from(want != got);
                }
            }
            assert!(differ * 10 < ink, "page {page}: {differ} of {ink} ink pixels differ from the unturned page");
        }
    };
    check(&mut a, doc);
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "placed.pdf" }));
    let reopened = ok(&mut a, "doc_open", json!({ "path": "placed.pdf" }))["doc"].as_u64().unwrap();
    check(&mut a, reopened);
}

#[test]
fn comment_checkmarks_locks_hiding_and_summaries_through_tools() {
    let dir = workdir("comment-polish");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let c = ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "note", "at": [20, 20], "contents": "Sticky", "author": "Ada" }));
    let id = c["comment"]["id"].as_str().unwrap().to_string();
    ok(&mut a, "comment_set_status", json!({ "doc": doc, "id": id, "status": "accepted" }));
    ok(&mut a, "comment_mark", json!({ "doc": doc, "id": id }));
    ok(&mut a, "comment_lock", json!({ "doc": doc, "id": id }));
    let list = ok(&mut a, "comment_list", json!({ "doc": doc }));
    assert_eq!(list["count"], 1, "{list}");
    let c = &list["comments"][0];
    assert_eq!((c["status"].as_str(), c["marked"].as_bool(), c["locked"].as_bool()), (Some("Accepted"), Some(true), Some(true)));
    assert!(matches!(a.call("comment_delete", &json!({ "doc": doc, "id": id })), Err(ToolError::Failed(_))), "locked");
    ok(&mut a, "comment_mark", json!({ "doc": doc, "id": id, "marked": false }));
    ok(&mut a, "comment_lock", json!({ "doc": doc, "id": id, "locked": false }));
    let list = ok(&mut a, "comment_list", json!({ "doc": doc }));
    assert_eq!((list["comments"][0]["marked"].as_bool(), list["comments"][0]["locked"].as_bool()), (Some(false), Some(false)));

    assert_eq!(ok(&mut a, "comments_hide", json!({ "doc": doc }))["hidden"], true);
    ok(&mut a, "comments_hide", json!({ "doc": doc, "hidden": false }));

    let r = ok(&mut a, "comments_summarize", json!({ "doc": doc, "sort": "author", "out": "summary.pdf" }));
    assert!(r["bytes"].as_u64().unwrap() > 500);
    let text = ok(&mut a, "doc_open", json!({ "path": "summary.pdf" }))["doc"].as_u64().unwrap();
    let found = ok(&mut a, "text_find", json!({ "doc": text, "query": "Sticky" }));
    assert!(found["count"].as_u64().unwrap() >= 1, "{found}");
    assert!(matches!(a.call("comments_summarize", &json!({ "doc": doc, "sort": "colour" })), Err(ToolError::InvalidArgs(_))));
}

#[test]
fn line_endings_through_the_tool() {
    let dir = workdir("endings");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "line", "from": [20, 40], "to": [120, 80], "endings": ["None", "Diamond"] }));
    ok(
        &mut a,
        "comment_add",
        json!({ "doc": doc, "page": 1, "type": "polyline", "points": [[20, 120], [60, 100], [100, 140]], "endings": ["Circle", "Slash"] }),
    );
    ok(
        &mut a,
        "comment_add",
        json!({ "doc": doc, "page": 1, "type": "callout", "rect": [110, 200, 190, 240], "to": [40, 160], "contents": "Look", "endings": ["Square"] }),
    );
    assert!(matches!(
        a.call("comment_add", &json!({ "doc": doc, "page": 1, "type": "line", "from": [20, 40], "to": [120, 80], "endings": ["Sparkle", "None"] })),
        Err(ToolError::InvalidArgs(_))
    ));
    assert!(matches!(
        a.call(
            "comment_add",
            &json!({ "doc": doc, "page": 1, "type": "callout", "rect": [10, 10, 80, 40], "to": [90, 80], "endings": ["None", "None"] })
        ),
        Err(ToolError::InvalidArgs(_))
    ));
    assert!(matches!(
        a.call("comment_add", &json!({ "doc": doc, "page": 1, "type": "rectangle", "rect": [10, 10, 40, 40], "endings": ["Circle"] })),
        Err(ToolError::InvalidArgs(_))
    ));
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "ended.pdf" }));
    let saved = pdfcraft_cos::Document::open(std::sync::Arc::new(std::fs::read(dir.join("ended.pdf")).unwrap())).unwrap();
    for name in [b"Diamond".as_slice(), b"Circle", b"Slash", b"Square"] {
        assert!(
            saved.object_numbers().iter().any(|n| saved.try_get(*n).ok().is_some_and(|o| object_has_name(&o, name))),
            "missing /{}",
            String::from_utf8_lossy(name)
        );
    }
    let drawn: Vec<String> = saved
        .object_numbers()
        .into_iter()
        .filter_map(|n| {
            let obj = saved.try_get(n).ok()?;
            let pdfcraft_cos::Object::Stream(s) = &*obj else { return None };
            if s.dict.name(b"Subtype") != Some(b"Form") {
                return None;
            }
            s.decoded().ok().map(|b| String::from_utf8_lossy(&b).into_owned())
        })
        .collect();
    assert!(drawn.iter().any(|s| s.contains("h B")), "a filled ending was drawn: {drawn:?}");
    assert!(drawn.iter().any(|s| s.contains(" l S")), "an open ending was drawn: {drawn:?}");
}

fn object_has_name(obj: &pdfcraft_cos::Object, name: &[u8]) -> bool {
    match obj {
        pdfcraft_cos::Object::Name(n) => n.as_slice() == name,
        pdfcraft_cos::Object::Array(items) => items.iter().any(|o| object_has_name(o, name)),
        pdfcraft_cos::Object::Dict(d) => d.iter().any(|(_, o)| object_has_name(o, name)),
        pdfcraft_cos::Object::Stream(s) => s.dict.iter().any(|(_, o)| object_has_name(o, name)),
        _ => false,
    }
}

#[test]
fn drawing_comments_through_tools() {
    let dir = workdir("drawing");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    for (ty, extra) in [
        ("polygon", json!({ "points": [[20, 20], [80, 20], [50, 70]] })),
        ("cloud", json!({ "points": [[100, 20], [180, 20], [180, 80], [100, 80]], "color": "red" })),
        ("polyline", json!({ "points": [[20, 120], [60, 100], [100, 120]] })),
        ("callout", json!({ "rect": [110, 200, 190, 240], "to": [40, 160], "contents": "Look" })),
        ("caret", json!({ "at": [60, 150], "contents": "insert this" })),
    ] {
        let mut args = json!({ "doc": doc, "page": 1, "type": ty });
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        ok(&mut a, "comment_add", args);
    }
    let list = ok(&mut a, "comment_list", json!({ "doc": doc }));
    let types: Vec<&str> = list["comments"].as_array().unwrap().iter().map(|c| c["type"].as_str().unwrap()).collect();
    for t in ["Polygon", "PolyLine", "FreeText", "Caret"] {
        assert!(types.contains(&t), "{types:?}");
    }
    assert_eq!(types.iter().filter(|t| **t == "Polygon").count(), 2);
    let callout = list["comments"].as_array().unwrap().iter().find(|c| c["type"] == "FreeText").unwrap();
    let r: Vec<f64> = callout["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert!(r[0] < 40.0 && r[3] > 159.0, "the rect holds the leader line: {r:?}");
    assert!(matches!(
        a.call("comment_add", &json!({ "doc": doc, "page": 1, "type": "polygon", "points": [[1, 1]] })),
        Err(ToolError::Failed(_) | ToolError::InvalidArgs(_))
    ));
    // Replace Text: a strikeout over the found text and a grouped caret.
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 2, "type": "replace", "find": "page 2", "contents": "second page" }));
    let list = ok(&mut a, "comment_list", json!({ "doc": doc, "page": 2 }));
    let strike = list["comments"].as_array().unwrap().iter().find(|c| c["type"] == "StrikeOut").unwrap().clone();
    assert_eq!(strike["replies"][0]["contents"], "second page", "the caret threads under the strikeout");
    assert!(matches!(a.call("comment_add", &json!({ "doc": doc, "page": 2, "type": "replace", "find": "page 2" })), Err(ToolError::InvalidArgs(_))));
    // A file attached as a comment.
    std::fs::write(dir.join("notes.txt"), b"remember").unwrap();
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "attachment", "path": "notes.txt", "at": [250, 20], "icon": "Paperclip" }));
    let info = ok(&mut a, "doc_info", json!({ "doc": doc }));
    assert!(info.to_string().contains("notes.txt"), "{info}");
    let png = a.call("page_render", &json!({ "doc": doc, "page": 1, "dpi": 72 })).unwrap();
    assert!(matches!(png[0], Content::Png { .. }));
}

#[test]
fn digital_ids_signing_and_validation_through_tools() {
    let dir = workdir("signing");
    let mut a = auto(&dir);
    let id = ok(
        &mut a,
        "sign_id_create",
        json!({ "name": "Ada Lovelace", "organization": "Analytical Engines", "email": "ada@example.com", "country": "GB", "key": "p256", "password": "secret1", "path": "ada.p12" }),
    );
    assert_eq!(id["certificate"]["subject"], "C=GB, O=Analytical Engines, CN=Ada Lovelace, E=ada@example.com");
    assert_eq!(id["certificate"]["self_signed"], true);
    assert!(matches!(a.call("sign_id_create", &json!({ "name": "X", "password": "short", "path": "x.p12" })), Err(ToolError::InvalidArgs(_))));

    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "sign_list", json!({ "doc": doc }))["count"], 0);
    // A Keychain identity that doesn't exist (macOS) or Keychains at all (elsewhere).
    assert!(matches!(a.call("sign_document", &json!({ "doc": doc, "id": "keychain:No Such Signer", "out": "k.pdf" })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("sign_document", &json!({ "doc": doc, "id": "windows:No Such Signer", "out": "w.pdf" })), Err(ToolError::Failed(_))));
    let store = ok(&mut a, "sign_windows_ids", json!({}));
    let ids = store["ids"].as_array().unwrap();
    assert_eq!(store["count"].as_u64().unwrap(), ids.len() as u64);
    assert!(ids.iter().all(|id| id["id"].as_str().unwrap().starts_with("windows:")));
    #[cfg(not(windows))]
    assert!(ids.is_empty());
    assert!(matches!(
        a.call("sign_document", &json!({ "doc": doc, "id": "ada.p12", "password": "wrong!", "out": "signed.pdf" })),
        Err(ToolError::InvalidArgs(_))
    ));
    let r = ok(
        &mut a,
        "sign_document",
        json!({ "doc": doc, "id": "ada.p12", "password": "secret1", "page": 2, "rect": [20, 200, 180, 250], "reason": "Approved", "location": "London", "out": "signed.pdf" }),
    );
    assert_eq!(r["signature"]["status"], "unknown");
    assert_eq!(r["signature"]["signer"], "Ada Lovelace");
    assert_eq!(r["signature"]["page"], 2);
    let rect: Vec<f64> = r["signature"]["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert!((rect[0] - 20.0).abs() < 0.5 && (rect[3] - 250.0).abs() < 0.5, "{rect:?}");
    assert!(dir.join("signed.pdf").exists());

    // Trust the ID: valid; comment afterwards: still valid, change allowed.
    let t = ok(&mut a, "sign_trust", json!({ "paths": ["ada.p12"], "password": "secret1" }));
    assert_eq!(t["trusted"].as_array().unwrap().len(), 1);
    let list = ok(&mut a, "sign_list", json!({ "doc": doc }));
    assert_eq!((list["all_valid"].as_bool(), list["signatures"][0]["status"].as_str()), (Some(true), Some("valid")));
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "note", "at": [20, 20], "contents": "ok" }));
    let s = &ok(&mut a, "sign_list", json!({ "doc": doc }))["signatures"][0];
    assert_eq!((s["status"].as_str(), s["modification"].as_str()), (Some("valid"), Some("allowed")));
    // A full rewrite is refused for a signed document.
    assert!(matches!(a.call("doc_save", &json!({ "doc": doc, "path": "copy.pdf", "full": true })), Err(ToolError::Failed(_))));
    ok(&mut a, "doc_save", json!({ "doc": doc }));
    // Three revisions: the original, the signature, the comment; the middle one is signed.
    let revs = ok(&mut a, "doc_revisions", json!({ "doc": doc }))["revisions"].as_array().cloned().unwrap();
    assert_eq!(revs.len(), 3, "{revs:?}");
    assert!(revs[0]["signed_by"].as_array().unwrap().is_empty() && revs[1]["signed_by"].as_array().unwrap().len() == 1, "{revs:?}");
    // The Signatures panel outline: the signing revision, then the comment on page 1.
    let outline = ok(&mut a, "sign_list", json!({ "doc": doc }))["revisions"].as_array().cloned().unwrap();
    assert_eq!(outline.iter().map(|r| r["revision"].as_u64()).collect::<Vec<_>>(), vec![Some(2), Some(3)], "{outline:?}");
    assert_eq!(outline[0]["signature"], list["signatures"][0]["field"], "{outline:?}");
    assert!(outline[0]["changes"].as_array().unwrap().contains(&json!({ "page": 2, "kind": "signature" })), "{outline:?}");
    assert!(outline[1]["signature"].is_null());
    assert!(outline[1]["changes"].as_array().unwrap().contains(&json!({ "page": 1, "kind": "comments" })), "{outline:?}");
    let old = ok(&mut a, "doc_open_revision", json!({ "doc": doc, "revision": 2 }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "comment_list", json!({ "doc": old }))["comments"].as_array().map(Vec::len), Some(0), "before the comment");
    assert_eq!(ok(&mut a, "sign_list", json!({ "doc": old }))["count"], 1);
    assert!(matches!(a.call("doc_open_revision", &json!({ "doc": doc, "revision": 4 })), Err(ToolError::Failed(_))));
    assert_eq!(ok(&mut a, "sign_trust", json!({ "clear": true }))["trusted"].as_array().unwrap().len(), 0);
    assert_eq!(ok(&mut a, "sign_list", json!({ "doc": doc }))["signatures"][0]["status"], "unknown");
}

#[test]
fn optimizing_through_tools() {
    let dir = workdir("optimize");
    // A 2400 × 1600 photo-like JPEG at 600 dpi: a 4 × 2.67 inch page.
    let (w, h) = (2400u32, 1600u32);
    let px: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            let (x, y) = (i % w, i / w);
            [(x * 255 / w) as u8 ^ ((x * y) & 15) as u8, (y * 255 / h) as u8, ((x + y) & 255) as u8]
        })
        .collect();
    let mut jpeg = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95);
    enc.set_pixel_density(image::codecs::jpeg::PixelDensity::dpi(600));
    enc.encode(&px, w, h, image::ExtendedColorType::Rgb8).unwrap();
    std::fs::write(dir.join("photo.jpg"), &jpeg).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_create", json!({ "from": "images", "paths": ["photo.jpg"] }))["doc"].as_u64().unwrap();
    let r = ok(
        &mut a,
        "doc_optimize",
        json!({ "doc": doc, "path": "small.pdf", "color": { "ppi": 100, "above_ppi": 150, "compression": "jpeg", "quality": 45 }, "discard": ["metadata"] }),
    );
    assert_eq!((r["images"].as_u64(), r["images_resampled"].as_u64()), (Some(1), Some(1)), "{r}");
    assert!(r["bytes_after"].as_u64().unwrap() * 5 < r["bytes_before"].as_u64().unwrap(), "{r}");
    assert_eq!(r["discarded"][0]["category"], "metadata");
    // The result opens and shows one page of the same size.
    let small = ok(&mut a, "doc_open", json!({ "path": "small.pdf" }));
    assert_eq!(small["pages"], 1);
    let reduced = ok(&mut a, "doc_reduce", json!({ "doc": doc, "path": "reduced.pdf" }));
    assert!(reduced["bytes_after"].as_u64().unwrap() < reduced["bytes_before"].as_u64().unwrap());
    assert!(matches!(
        a.call("doc_optimize", &json!({ "doc": doc, "path": "x.pdf", "color": { "compression": "gif" } })),
        Err(ToolError::InvalidArgs(_))
    ));
    assert!(matches!(a.call("doc_optimize", &json!({ "doc": doc, "path": "x.pdf", "discard": ["everything"] })), Err(ToolError::InvalidArgs(_))));
}

/// A synthetic "scan": an 800 × 300 one-bit page image (Flate) drawn by an LZW content stream.
fn lzw_scan_pdf() -> Vec<u8> {
    use pdfcraft_cos::{Dict, Document, Object, SaveOptions, Stream, write_full};
    let (w, h) = (800usize, 300usize);
    let stride = w.div_ceil(8);
    let mut bits = vec![0xffu8; stride * h]; // white
    for y in 0..h {
        for x in 0..w {
            // A different pseudo-random glyph in every 20 × 28 cell, so nothing repeats.
            let (cx, cy) = (x % 20, y % 28);
            let mut g = ((x / 20) as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ ((y / 28) as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
            g = (g ^ (g >> 29)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            g ^= g >> 32;
            // Stroke widths and positions vary per glyph, and specks of scanner noise.
            let (sw, bar) = (2 + (g >> 8) as usize % 3, 10 + (g >> 12) as usize % 5);
            let speck = (x as u64).wrapping_mul(0x2545_F491_4F6C_DD1D).wrapping_add((y as u64).wrapping_mul(0x9E37_79B9)).is_multiple_of(211);
            let ink = speck
                || (3..15).contains(&cx)
                    && (4..22).contains(&cy)
                    && ((g & 1 != 0 && cx < 3 + sw)
                        || (g & 2 != 0 && cx >= 15 - sw)
                        || (g & 4 != 0 && cy < 4 + sw)
                        || (g & 8 != 0 && (bar..bar + sw).contains(&cy))
                        || (g & 16 != 0 && cy >= 22 - sw)
                        || (g & 32 != 0 && (cx + 28 - cy) % 13 < sw));
            if ink {
                bits[y * stride + x / 8] &= !(0x80 >> (x % 8));
            }
        }
    }
    let mut doc = Document::new_empty();
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("XObject"));
    d.set(b"Subtype".to_vec(), Object::name("Image"));
    d.set(b"Width".to_vec(), Object::Int(w as i64));
    d.set(b"Height".to_vec(), Object::Int(h as i64));
    d.set(b"BitsPerComponent".to_vec(), Object::Int(1));
    d.set(b"ColorSpace".to_vec(), Object::name("DeviceGray"));
    let img = doc.add(Object::Stream(Stream::flate(d, &bits)));
    let content = format!("q {w} 0 0 {h} 0 0 cm /Im0 Do Q\n").repeat(3);
    let mut cd = Dict::new();
    cd.set(b"Filter".to_vec(), Object::name("LZWDecode"));
    let lzw = pdfcraft_filters::encode(&pdfcraft_filters::Filter::Lzw, &pdfcraft_filters::Params::default(), content.as_bytes()).unwrap();
    let c = doc.add(Object::Stream(Stream::from_raw(cd, lzw)));
    let pages = doc.root().and_then(|r| doc.get(r).as_dict().and_then(|d| d.reference(b"Pages"))).unwrap();
    let mut x = Dict::new();
    x.set(b"Im0".to_vec(), Object::Ref(img));
    let mut res = Dict::new();
    res.set(b"XObject".to_vec(), Object::Dict(x));
    let mut p = Dict::new();
    p.set(b"Type".to_vec(), Object::name("Page"));
    p.set(b"Parent".to_vec(), Object::Ref(pages));
    p.set(b"MediaBox".to_vec(), Object::Array(vec![0.into(), 0.into(), (w as i64).into(), (h as i64).into()]));
    p.set(b"Resources".to_vec(), Object::Dict(res));
    p.set(b"Contents".to_vec(), Object::Ref(c));
    let page = doc.add(Object::Dict(p));
    doc.update_dict(pages, |d| {
        d.set(b"Kids".to_vec(), Object::Array(vec![Object::Ref(page)]));
        d.set(b"Count".to_vec(), Object::Int(1));
    })
    .unwrap();
    write_full(&doc, &SaveOptions { object_streams: false, ..SaveOptions::default() }).unwrap()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn optimizing_scans_with_ccitt_lzw_to_flate_and_object_streams_through_tools() {
    let dir = workdir("optimize-codecs");
    std::fs::write(dir.join("scan.pdf"), lzw_scan_pdf()).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "scan.pdf" }))["doc"].as_u64().unwrap();
    // Decoded pixels of page 1 at 72 dpi (one image pixel per device pixel).
    let preview = |a: &mut Automation, d: u64| {
        let out = a.call("page_render", &json!({ "doc": d, "page": 1, "dpi": 72 })).unwrap();
        let Content::Png { data, .. } = &out[0] else { panic!("expected an image") };
        let mut reader = png::Decoder::new(std::io::Cursor::new(data.as_slice())).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        (info.width, info.height, buf)
    };
    let original = preview(&mut a, doc);
    assert_eq!((original.0, original.1), (800, 300));
    let r =
        ok(&mut a, "doc_optimize", json!({ "doc": doc, "path": "g4.pdf", "mono": "ccitt-g4", "lzw_to_flate": true, "compress_structure": false }));
    assert_eq!((r["images_ccitt"].as_u64(), r["images_recompressed"].as_u64()), (Some(1), Some(1)), "{r}");
    assert_eq!((r["lzw_reencoded"].as_u64(), r["lzw_skipped"].as_u64(), r["object_streams"].as_bool()), (Some(1), Some(0), Some(false)), "{r}");
    assert!(r["bytes_after"].as_u64().unwrap() < r["bytes_before"].as_u64().unwrap(), "{r}");
    let g4 = std::fs::read(dir.join("g4.pdf")).unwrap();
    assert!(contains(&g4, b"/CCITTFaxDecode") && !contains(&g4, b"/LZWDecode") && !contains(&g4, b"/ObjStm"));
    // The copy draws the same page.
    let copy = ok(&mut a, "doc_open", json!({ "path": "g4.pdf" }))["doc"].as_u64().unwrap();
    assert!(preview(&mut a, copy) == original, "the optimized copy renders differently");
    // Object streams on (the default); LZW kept when asked; mono as an object.
    let r = ok(
        &mut a,
        "doc_optimize",
        json!({ "doc": doc, "path": "packed.pdf", "lzw_to_flate": false, "mono": { "compression": "zip", "downsample": false } }),
    );
    assert_eq!((r["images_ccitt"].as_u64(), r["lzw_reencoded"].as_u64(), r["object_streams"].as_bool()), (Some(0), Some(0), Some(true)), "{r}");
    let packed = std::fs::read(dir.join("packed.pdf")).unwrap();
    assert!(contains(&packed, b"/ObjStm") && !contains(&packed, b"/CCITTFaxDecode"));
    for bad in [json!("jbig2"), json!(7), json!({ "compression": 3 }), json!({ "downsample": "yes" })] {
        assert!(matches!(a.call("doc_optimize", &json!({ "doc": doc, "path": "x.pdf", "mono": bad })), Err(ToolError::InvalidArgs(_))), "{bad}");
    }
}

#[test]
fn initial_view_through_tools() {
    let dir = workdir("initial-view");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "doc_initial_view", json!({ "doc": doc }));
    assert_eq!((r["initial_view"]["layout"].as_str(), r["initial_view"]["page"].as_u64()), (Some("Default"), Some(1)));
    let r = ok(
        &mut a,
        "doc_initial_view",
        json!({ "doc": doc, "navigation": "pages", "layout": "two_up", "magnification": 150, "page": 2, "language": "en-GB", "binding": "right", "display_title": true }),
    );
    let v = &r["initial_view"];
    assert_eq!((v["navigation"].as_str(), v["magnification"].as_str(), v["page"].as_u64()), (Some("Pages"), Some("Percent(150.0)"), Some(2)));
    assert_eq!((v["language"].as_str(), v["binding"].as_str()), (Some("en-GB"), Some("right")));
    assert_eq!(ok(&mut a, "edit_undo", json!({ "doc": doc }))["undone"], "Change initial view");
    assert!(matches!(a.call("doc_initial_view", &json!({ "doc": doc, "layout": "spiral" })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(a.call("doc_initial_view", &json!({ "doc": doc, "page": 9 })), Err(ToolError::Failed(_))));
}

#[test]
fn ocr_tools_make_a_scan_searchable() {
    let dir = workdir("ocr");
    let mut a = auto(&dir);
    let status = ok(&mut a, "ocr_status", json!({}));
    assert_eq!(status["languages"][0]["code"], "en");
    assert_eq!(status["available"] == true, pdfcraft_engine::ocr::Models::find().is_some());
    if !pdfcraft_engine::ocr::Models::required_for_tests().unwrap() {
        eprintln!("skipped: OCR models not installed");
        return;
    }
    let text = ok(&mut a, "doc_create", json!({ "from": "text", "text": "Searchable scans with recognised words" }))["doc"].as_u64().unwrap();
    ok(&mut a, "doc_export_images", json!({ "doc": text, "folder": "scan", "dpi": 150, "pages": [1] }));
    let png = std::fs::read_dir(dir.join("scan")).unwrap().next().unwrap().unwrap().path();
    let scan = ok(&mut a, "doc_create", json!({ "from": "images", "paths": [png.to_string_lossy()] }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "ocr_recognize", json!({ "doc": scan }));
    assert!(r["pages"][0]["text"].as_str().unwrap().to_lowercase().contains("searchable"), "{r}");
    assert!(r["words"].as_u64().unwrap() >= 4);
    let again = ok(&mut a, "ocr_recognize", json!({ "doc": scan, "pages": [1] }));
    assert!(again["pages"][0]["skipped"].is_string(), "{again}");
    assert!(matches!(a.call("ocr_recognize", &json!({ "doc": scan, "language": "xx" })), Err(ToolError::InvalidArgs(_))));
}

#[test]
fn ocr_recognize_files_writes_searchable_copies() {
    let dir = workdir("ocr-files");
    let mut a = auto(&dir);
    if !pdfcraft_engine::ocr::Models::required_for_tests().unwrap() {
        eprintln!("skipped: OCR models not installed");
        return;
    }
    let text = ok(&mut a, "doc_create", json!({ "from": "text", "text": "Batch recognition works" }))["doc"].as_u64().unwrap();
    ok(&mut a, "doc_export_images", json!({ "doc": text, "folder": "scan", "dpi": 150, "pages": [1] }));
    let png = std::fs::read_dir(dir.join("scan")).unwrap().next().unwrap().unwrap().path();
    let scan = ok(&mut a, "doc_create", json!({ "from": "images", "paths": [png.to_string_lossy()] }))["doc"].as_u64().unwrap();
    ok(&mut a, "doc_save", json!({ "doc": scan, "path": "in/scan.pdf" }));
    let r = ok(&mut a, "ocr_recognize_files", json!({ "paths": ["in/scan.pdf", "missing.pdf"], "folder": "out" }));
    assert!(r["files"][0]["words"].as_u64().unwrap() >= 3, "{r}");
    assert!(r["files"][1]["error"].is_string(), "{r}");
    let out = ok(&mut a, "doc_open", json!({ "path": "out/scan.pdf" }))["doc"].as_u64().unwrap();
    let found = ok(&mut a, "text_find", json!({ "doc": out, "query": "recognition" }));
    assert!(found.to_string().contains("\"page\""), "{found}");
}

#[test]
fn javascript_through_tools() {
    let dir = workdir("js");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    for (name, y) in [("Price", 20), ("Qty", 60), ("Total", 100)] {
        ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "text", "rect": [20, y, 180, y + 22], "name": name }));
    }
    ok(
        &mut a,
        "js_set_document_script",
        json!({ "doc": doc, "name": "lib", "script": "function money(v) { return util.printf('EUR %,2.2f', v); }" }),
    );
    assert_eq!(ok(&mut a, "js_document_scripts", json!({ "doc": doc }))["scripts"][0]["name"], "lib");
    ok(
        &mut a,
        "form_set_script",
        json!({ "doc": doc, "field": "Total", "event": "calculate", "script": "event.value = getField('Price').value * getField('Qty').value;" }),
    );
    ok(&mut a, "form_set_script", json!({ "doc": doc, "field": "Total", "event": "format", "script": "event.value = money(event.value);" }));
    ok(
        &mut a,
        "form_set_script",
        json!({ "doc": doc, "field": "Qty", "event": "validate", "script": "if (event.value < 1) { app.alert('Order at least one'); event.rc = false; }" }),
    );
    ok(&mut a, "form_fill", json!({ "doc": doc, "values": { "Price": "1250", "Qty": "2" } }));
    assert!(page_text(&mut a, doc)[0].contains("EUR 2.500,00"), "{:?}", page_text(&mut a, doc));
    let err = a.call("form_fill", &json!({ "doc": doc, "values": { "Qty": "0" } })).unwrap_err();
    assert!(err.to_string().contains("Order at least one"), "{err}");

    let r = ok(
        &mut a,
        "js_run",
        json!({ "doc": doc, "script": "console.println(getField('Total').value); getField('Qty').value = 3; this.pageNum = 1; app.alert('ok');" }),
    );
    assert_eq!(r["console"][0], "2500");
    assert_eq!(r["alerts"][0], "ok");
    assert_eq!(r["requests"][0]["page"], 2);
    let f = ok(&mut a, "form_fields", json!({ "doc": doc }));
    let total = f["fields"].as_array().unwrap().iter().find(|x| x["name"] == "Total").unwrap().clone();
    assert_eq!(total["value"], "3750");
    let r = ok(&mut a, "js_run", json!({ "doc": doc, "script": "nope()" }));
    assert!(r["error"].as_str().unwrap().contains("nope"));

    // Scripts add and read comments, and stop on dialogs until they are answered.
    let script = "if (app.alert({ cMsg: 'Add a note?', nType: 2 }) == 4) \
                  this.addAnnot({ type: 'Text', point: [30, 200], contents: app.response('Note text?', 'Notes', 'Hi') }); \
                  var n = this.getAnnots(); n ? n.length : 0";
    let r = ok(&mut a, "js_run", json!({ "doc": doc, "script": script }));
    assert_eq!((r["question"]["kind"].as_str(), r["question"]["buttons"].clone()), (Some("alert"), json!(["yes", "no"])));
    let r = ok(&mut a, "js_run", json!({ "doc": doc, "script": script, "answers": ["yes"] }));
    assert_eq!((r["question"]["kind"].as_str(), r["question"]["default"].as_str(), r["answered"].as_u64()), (Some("response"), Some("Hi"), Some(1)));
    let r = ok(&mut a, "js_run", json!({ "doc": doc, "script": script, "answers": ["yes", { "text": "Call back" }] }));
    assert_eq!((r["result"].as_str(), r.get("question")), (Some("1"), None));
    let list = ok(&mut a, "comment_list", json!({ "doc": doc }));
    assert!(list["comments"].as_array().unwrap().iter().any(|c| c["contents"] == "Call back"), "{list}");
    assert_eq!(ok(&mut a, "js_run", json!({ "doc": doc, "script": script, "answers": ["no"] }))["result"], "1");
    for bad in [json!(["maybe"]), json!("yes"), json!([3])] {
        assert!(matches!(a.call("js_run", &json!({ "doc": doc, "script": script, "answers": bad })), Err(ToolError::InvalidArgs(_))), "{bad}");
    }
    assert_eq!(ok(&mut a, "js_enabled", json!({ "enabled": false }))["enabled"], false);
    assert!(a.call("js_run", &json!({ "doc": doc, "script": "1" })).is_err());
}

#[test]
fn merging_form_data_into_a_spreadsheet() {
    let dir = workdir("merge-data");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "text", "rect": [20, 20, 180, 42], "name": "City" }));
    for (city, file) in [("Paris", "one.xfdf"), ("Oslo", "two.fdf")] {
        ok(&mut a, "form_fill", json!({ "doc": doc, "values": { "City": city } }));
        ok(&mut a, "doc_export_data", json!({ "doc": doc, "path": file, "what": "fields" }));
    }
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "filled.pdf" }));
    let r = ok(&mut a, "form_merge_data", json!({ "paths": ["one.xfdf", "two.fdf", "filled.pdf"], "path": "report.csv" }));
    assert_eq!(r["rows"], 3);
    assert_eq!(std::fs::read_to_string(dir.join("report.csv")).unwrap(), "City\nParis\nOslo\nOslo\n");
}

#[test]
fn field_actions_through_tools() {
    let dir = workdir("field-actions");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "button", "rect": [20, 20, 120, 42], "name": "Go" }));
    let r = ok(
        &mut a,
        "form_set_actions",
        json!({ "doc": doc, "field": "Go", "actions": [
            { "trigger": "mouse_up", "javascript": "this.pageNum = 2;" },
            { "trigger": "mouse_enter", "hide": ["Go"] },
            { "trigger": "on_focus", "page": 3 }
        ] }),
    );
    assert_eq!(r["actions"].as_array().unwrap().len(), 3, "{r}");
    assert_eq!(r["actions"][0], json!({ "trigger": "mouse_up", "javascript": "this.pageNum = 2;" }));
    assert_eq!(r["actions"][1], json!({ "trigger": "mouse_enter", "hide": ["Go"] }));
    assert_eq!(r["actions"][2], json!({ "trigger": "on_focus", "page": 3 }));
    let run = ok(&mut a, "js_run", json!({ "doc": doc, "script": "this.pageNum = 2;", "field": "Go" }));
    assert_eq!(run["requests"][0]["page"], 3);
    assert!(a.call("form_set_actions", &json!({ "doc": doc, "field": "Go", "actions": [{ "trigger": "wave" }] })).is_err());
}

#[test]
fn field_action_chains_save_reopen_and_run_on_their_triggers() {
    let dir = workdir("field-action-chains");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "button", "rect": [20, 20, 120, 42], "name": "Go" }));
    ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "text", "rect": [20, 60, 180, 82], "name": "Note" }));
    let actions = json!([
        { "trigger": "mouse_up", "javascript": "getField('Note').value = 'ran ' + event.name;" },
        { "trigger": "mouse_up", "page": 2 },
        { "trigger": "mouse_up", "menu": "NextPage" },
        { "trigger": "mouse_enter", "hide": ["Note"] },
        { "trigger": "mouse_exit", "show": ["Note"] },
        { "trigger": "on_focus", "javascript": "app.alert('focus: ' + event.name);" },
        { "trigger": "on_blur", "url": "https://example.org/bye" }
    ]);
    let r = ok(&mut a, "form_set_actions", json!({ "doc": doc, "field": "Go", "actions": actions }));
    assert_eq!(r["actions"], actions, "{r}");

    // Saved and reopened, the chains read back in order.
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "actions.pdf" }));
    ok(&mut a, "doc_close", json!({ "doc": doc }));
    let mut b = auto(&dir);
    let doc = ok(&mut b, "doc_open", json!({ "path": "actions.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut b, "form_actions", json!({ "doc": doc, "field": "Go" }))["actions"], actions);

    // The JavaScript engine runs the configured scripts on their triggers, with the event name.
    let up = ok(&mut b, "form_run_trigger", json!({ "doc": doc, "field": "Go", "trigger": "mouse_up" }));
    assert_eq!(up["ran"].as_array().unwrap().len(), 3, "{up}");
    assert_eq!(up["requests"], json!([{ "page": 2 }]), "{up}");
    assert_eq!(up["menu"], json!(["NextPage"]));
    let note = |b: &mut Automation| {
        ok(b, "form_fields", json!({ "doc": doc }))["fields"].as_array().unwrap().iter().find(|f| f["name"] == "Note").unwrap()["value"].clone()
    };
    assert_eq!(note(&mut b), "ran Mouse Up");
    let focus = ok(&mut b, "form_run_trigger", json!({ "doc": doc, "field": "Go", "trigger": "on_focus" }));
    assert_eq!(focus["alerts"], json!(["focus: Focus"]), "{focus}");
    let display = |b: &mut Automation| ok(b, "js_run", json!({ "doc": doc, "script": "getField('Note').display" }))["result"].clone();
    ok(&mut b, "form_run_trigger", json!({ "doc": doc, "field": "Go", "trigger": "mouse_enter" }));
    assert_eq!(display(&mut b), "1", "hidden");
    ok(&mut b, "form_run_trigger", json!({ "doc": doc, "field": "Go", "trigger": "mouse_exit" }));
    assert_eq!(display(&mut b), "0", "visible");
    let blur = ok(&mut b, "form_run_trigger", json!({ "doc": doc, "field": "Go", "trigger": "on_blur" }));
    assert_eq!(blur["requests"], json!([{ "url": "https://example.org/bye" }]), "{blur}");
    let none = ok(&mut b, "form_run_trigger", json!({ "doc": doc, "field": "Go", "trigger": "mouse_down" }));
    assert_eq!(none["ran"], json!([]));

    // Reorder Mouse Up and edit its script; the other triggers are untouched.
    let mut reordered = actions.as_array().unwrap().clone();
    reordered.swap(0, 2);
    reordered[2] = json!({ "trigger": "mouse_up", "javascript": "getField('Note').value = 'second';" });
    let r = ok(&mut b, "form_set_actions", json!({ "doc": doc, "field": "Go", "actions": reordered }));
    assert_eq!(r["actions"], json!(reordered));
    let up = ok(&mut b, "form_run_trigger", json!({ "doc": doc, "field": "Go", "trigger": "mouse_up" }));
    assert_eq!(up["ran"][0], "Execute a menu item: NextPage", "{up}");
    assert_eq!(note(&mut b), "second");

    // With JavaScript off, scripts are skipped and the rest still runs.
    ok(&mut b, "js_enabled", json!({ "enabled": false }));
    let up = ok(&mut b, "form_run_trigger", json!({ "doc": doc, "field": "Go", "trigger": "mouse_up" }));
    assert_eq!(up["ran"].as_array().unwrap().len(), 2, "{up}");
    assert_eq!(up["skipped"].as_array().unwrap().len(), 1, "{up}");
    ok(&mut b, "js_enabled", json!({ "enabled": true }));

    // Refused without changes: an unsafe menu item, an unknown action that isn't there, a bad
    // trigger; running a missing field or trigger fails.
    for bad in [
        json!([{ "trigger": "mouse_up", "menu": "Quit" }]),
        json!([{ "trigger": "mouse_up", "other": "Launch" }]),
        json!([{ "trigger": "wave", "url": "https://example.org" }]),
    ] {
        assert!(b.call("form_set_actions", &json!({ "doc": doc, "field": "Go", "actions": bad })).is_err(), "{bad}");
    }
    assert_eq!(ok(&mut b, "form_actions", json!({ "doc": doc, "field": "Go" }))["actions"], json!(reordered));
    assert!(b.call("form_run_trigger", &json!({ "doc": doc, "field": "Nope", "trigger": "mouse_up" })).is_err());
    assert!(b.call("form_run_trigger", &json!({ "doc": doc, "field": "Go", "trigger": "wave" })).is_err());

    // Removing all actions.
    let r = ok(&mut b, "form_set_actions", json!({ "doc": doc, "field": "Go", "actions": [] }));
    assert_eq!(r["actions"], json!([]));
}

#[test]
fn detecting_form_fields_through_tools() {
    let dir = workdir("detect-fields");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_create", json!({ "from": "text", "text": "Name: ____________________\n\nCity: ____________________" }))["doc"]
        .as_u64()
        .unwrap();
    let r = ok(&mut a, "form_detect_fields", json!({ "doc": doc, "add": false }));
    assert_eq!(r["fields"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(r["fields"][1]["name"], "City");
    assert_eq!(ok(&mut a, "form_fields", json!({ "doc": doc }))["fields"].as_array().unwrap().len(), 0);
    ok(&mut a, "form_detect_fields", json!({ "doc": doc }));
    ok(&mut a, "form_fill", json!({ "doc": doc, "values": { "City": "Lisbon" } }));
    let f = ok(&mut a, "form_fields", json!({ "doc": doc }));
    assert_eq!(f["fields"][1]["value"], "Lisbon", "{f}");
}

#[test]
fn comparing_documents_through_tools() {
    let dir = workdir("compare");
    let mut a = auto(&dir);
    let v1 = ok(&mut a, "doc_create", json!({ "from": "text", "text": "Rent is 900 per month. Pets are not allowed." }))["doc"].as_u64().unwrap();
    let v2 = ok(&mut a, "doc_create", json!({ "from": "text", "text": "Rent is 950 per month. Pets are allowed. Parking included." }))["doc"]
        .as_u64()
        .unwrap();
    let r = ok(&mut a, "doc_compare", json!({ "doc": v2, "other": v1, "visual": true }));
    assert!(!r["visual"].as_array().unwrap().is_empty(), "{r}");
    assert_eq!((r["replaced"].as_u64(), r["inserted"].as_u64(), r["deleted"].as_u64()), (Some(1), Some(1), Some(1)), "{r}");
    assert_eq!(r["changes"][0]["old"]["text"], "900");
    assert_eq!(r["changes"][0]["new"]["text"], "950");
    assert_eq!(r["changes"][0]["new"]["page"], 1);
    ok(&mut a, "doc_compare_report", json!({ "doc": v2, "other": v1, "path": "report.pdf" }));
    assert!(std::fs::read(dir.join("report.pdf")).unwrap().starts_with(b"%PDF"));
    assert_eq!(ok(&mut a, "doc_compare_mark", json!({ "doc": v2, "other": v1 }))["comments"], 3);
    assert!(a.call("doc_compare", &json!({ "doc": v2, "other": 999 })).is_err());
    // The page map comes with it.
    assert_eq!(r["pages"][0]["status"], "modified", "{r}");
    assert!(!r["visual_regions"].as_array().unwrap().is_empty(), "{r}");
}

/// Two versions of a four-page file saved to disk: in the newer one the third page moved to
/// the front with one word changed, the second page was deleted, a new page was inserted and
/// the last page's text was pushed down (same words, different look).
fn save_text_pdf(a: &mut Automation, pages: &[&str], path: &str) {
    let d = ok(a, "doc_create", json!({ "from": "text", "text": pages.join("\u{c}") }))["doc"].as_u64().unwrap();
    ok(a, "doc_save", json!({ "doc": d, "path": path }));
    ok(a, "doc_close", json!({ "doc": d }));
}

#[test]
fn comparing_files_on_disk_through_tools() {
    let dir = workdir("compare-files");
    let mut a = auto(&dir);
    let pa = "Alpha page about the lease terms, the monthly rent, the deposit and the dates it is due.";
    let pb = "Bravo page about parking spaces, bicycle storage, the shared laundry room and its hours.";
    let pc = "Charlie page about pets, noise after ten at night, smoking and guests staying over weekends.";
    let pd = "Delta page with the signatures of both parties and the date the agreement was signed.";
    let pe = "Echo page about the new garden behind the building and who may use it in summer.";
    save_text_pdf(&mut a, &[pa, pb, pc, pd], "v1.pdf");
    let pc2 = pc.replace("pets", "cats");
    let pd2 = format!("\n\n\n\n{pd}");
    save_text_pdf(&mut a, &[&pc2, pa, pe, &pd2], "v2.pdf");

    let r = ok(
        &mut a,
        "compare_files",
        json!({ "old": "v1.pdf", "new": "v2.pdf", "json_path": "out/report.json", "report_path": "out/report.pdf", "overlay_path": "out/marked.pdf", "limit": 1 }),
    );
    let s = &r["summary"];
    assert_eq!(
        (s["pages_inserted"].as_u64(), s["pages_deleted"].as_u64(), s["pages_moved"].as_u64(), s["pages_same"].as_u64(), s["visual_pages"].as_u64()),
        (Some(1), Some(1), Some(1), Some(2), Some(2)),
        "{r}"
    );
    let map: Vec<(String, Value, Value)> =
        r["pages"].as_array().unwrap().iter().map(|p| (p["status"].as_str().unwrap().to_string(), p["old"].clone(), p["new"].clone())).collect();
    assert_eq!(
        map,
        [
            ("moved".to_string(), json!(3), json!(1)),
            ("same".to_string(), json!(1), json!(2)),
            ("deleted".to_string(), json!(2), Value::Null),
            ("inserted".to_string(), Value::Null, json!(3)),
            ("same".to_string(), json!(4), json!(4)),
        ]
    );
    let total = r["changes_total"].as_u64().unwrap();
    assert!(total > 1 && r["truncated"] == true && r["changes"].as_array().unwrap().len() == 1, "{r}");
    assert_eq!((r["changes"][0]["old"]["text"].as_str(), r["changes"][0]["new"]["text"].as_str()), (Some("pets,"), Some("cats,")));
    assert_eq!(r["changes"][0]["new"]["page"], 1);
    let looks: Vec<(u64, u64)> =
        r["visual"].as_array().unwrap().iter().map(|v| (v["old_page"].as_u64().unwrap(), v["new_page"].as_u64().unwrap())).collect();
    assert_eq!(looks, [(3, 1), (4, 4)], "{r}");
    assert_eq!((r["old"]["pages"].as_u64(), r["new"]["name"].as_str()), (Some(4), Some("v2.pdf")));

    // The JSON file has every change; the report and the marked-up copy are PDFs.
    let full: Value = serde_json::from_slice(&std::fs::read(dir.join("out/report.json")).unwrap()).unwrap();
    assert_eq!(full["changes"].as_array().unwrap().len() as u64, total);
    assert_eq!(full["pages"], r["pages"]);
    let report = ok(&mut a, "doc_open", json!({ "path": "out/report.pdf" }))["doc"].as_u64().unwrap();
    let text = page_text(&mut a, report).join(" ");
    assert!(text.contains("1 inserted, 1 deleted, 1 moved"), "{text}");
    let marked = ok(&mut a, "doc_open", json!({ "path": "out/marked.pdf" }))["doc"].as_u64().unwrap();
    let list = ok(&mut a, "comment_list", json!({ "doc": marked }));
    let contents: Vec<&str> = list["comments"].as_array().unwrap().iter().filter_map(|c| c["contents"].as_str()).collect();
    for want in ["Moved page: page 3", "Deleted page: page 2", "Inserted page", "Replaced: \"pets,\"", "Looks different"] {
        assert!(contents.iter().any(|c| c.starts_with(want)), "{want} missing from {contents:?}");
    }
    // The compared files were closed again; v2.pdf on disk is unchanged.
    let open = ok(&mut a, "doc_list", json!({}))["documents"].as_array().unwrap().len();
    assert_eq!(open, 2, "only the report and the marked copy are open");

    // Threshold, page ranges, text-only.
    let r = ok(&mut a, "compare_files", json!({ "old": "v1.pdf", "new": "v2.pdf", "threshold": 0.5 }));
    assert_eq!(r["summary"]["visual_pages"], 0);
    let r = ok(&mut a, "compare_files", json!({ "old": "v1.pdf", "new": "v2.pdf", "old_pages": [1, 1], "new_pages": [2, 2] }));
    assert_eq!(r["identical"], true, "{r}");
    let r = ok(&mut a, "compare_files", json!({ "old": "v1.pdf", "new": "v1.pdf", "visual": false }));
    assert_eq!(r["identical"], true, "{r}");

    // Bad arguments are refused with a message, and nothing stays open.
    for bad in [
        json!({ "old": "v1.pdf", "new": "missing.pdf" }),
        json!({ "old": "v1.pdf", "new": "v2.pdf", "old_pages": [3, 1] }),
        json!({ "old": "v1.pdf", "new": "v2.pdf", "old_pages": [2, 9] }),
        json!({ "old": "v1.pdf", "new": "v2.pdf", "threshold": 2 }),
        json!({ "old": "v1.pdf", "new": "v2.pdf", "tolerance": 300 }),
        json!({ "old": "v1.pdf", "new": "v2.pdf", "dpi": 1000 }),
    ] {
        assert!(a.call("compare_files", &bad).is_err(), "{bad}");
    }
    assert_eq!(ok(&mut a, "doc_list", json!({}))["documents"].as_array().unwrap().len(), 2);
}

#[test]
fn actions_through_tools() {
    let dir = workdir("actions");
    let mut a = auto(&dir);
    let list = ok(&mut a, "action_list", json!({}));
    assert!(list["actions"].as_array().unwrap().iter().any(|x| x["name"] == "Prepare for Distribution"));
    assert!(list["steps"].as_array().unwrap().iter().any(|x| x["step"] == "add_watermark" && x["takes_arg"] == true));
    let r = ok(&mut a, "action_run", json!({ "action": "add page numbers", "paths": ["a.pdf", "b.pdf", "missing.pdf"], "folder": "out" }));
    assert_eq!(r["files"][0]["log"][0], "Add footer");
    assert!(r["files"][2]["error"].is_string());
    let doc = ok(&mut a, "doc_open", json!({ "path": "out/b.pdf" }))["doc"].as_u64().unwrap();
    assert!(page_text(&mut a, doc)[1].contains("Page 2 of 2"), "{:?}", page_text(&mut a, doc));
    let r = ok(&mut a, "action_run", json!({ "steps": [{ "step": "set_title", "arg": "Hello" }], "paths": ["a.pdf"], "folder": "out2" }));
    assert_eq!(r["files"][0]["log"][0], "Set document title");
    assert!(a.call("action_run", &json!({ "steps": [{ "step": "fly" }], "paths": ["a.pdf"], "folder": "x" })).is_err());
}

#[test]
fn pdfa_through_tools() {
    let dir = workdir("pdfa");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_create", json!({ "from": "text", "text": "Archive me" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "pdfa_verify", json!({ "doc": doc }));
    assert_eq!(r["compliant"], false);
    assert!(r["issues"].as_array().unwrap().iter().any(|i| i["clause"] == "6.6.2.1"), "{r}");
    let r = ok(&mut a, "pdfa_convert", json!({ "doc": doc, "level": "3b" }));
    assert_eq!(r["declared"]["pdfa"], "PDF/A-3b", "{r}");
    assert!(r["issues"].as_array().unwrap().iter().all(|i| i["fixable"] == false), "{r}");
    assert!(a.call("pdfa_verify", &json!({ "doc": doc, "level": "9z" })).is_err());
}

#[test]
fn exporting_to_word_html_and_rtf() {
    let dir = workdir("office");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    for ext in ["docx", "html", "rtf"] {
        let r = ok(&mut a, "doc_export_office", json!({ "doc": doc, "path": format!("a.{ext}") }));
        assert_eq!(r["format"], ext);
    }
    let html = std::fs::read_to_string(dir.join("a.html")).unwrap();
    assert!(html.contains("Page 1") && html.contains("Page 3") && html.matches("<hr>").count() == 2, "{html}");
    assert!(std::fs::read(dir.join("a.docx")).unwrap().starts_with(b"PK"));
    assert!(std::fs::read_to_string(dir.join("a.rtf")).unwrap().contains("Page 2"));
    assert!(a.call("doc_export_office", &json!({ "doc": doc, "path": "a.xyz" })).is_err());
}

/// The inflated parts of a zip archive written with local headers that carry sizes (no data
/// descriptors), by name.
fn unzip(zip: &[u8]) -> Vec<(String, String)> {
    let at = |i: usize, n: usize| zip[i..i + n].iter().rev().fold(0usize, |v, b| v << 8 | *b as usize);
    let mut parts = Vec::new();
    let mut i = 0;
    while zip[i..].starts_with(b"PK\x03\x04") {
        let (method, size, name_len, extra) = (at(i + 8, 2), at(i + 18, 4), at(i + 26, 2), at(i + 28, 2));
        let name = String::from_utf8(zip[i + 30..i + 30 + name_len].to_vec()).unwrap();
        let data = i + 30 + name_len + extra;
        let raw = &zip[data..data + size];
        let mut text = String::new();
        if method == 8 {
            std::io::Read::read_to_string(&mut flate2::read::DeflateDecoder::new(raw), &mut text).unwrap();
        } else {
            text = String::from_utf8_lossy(raw).into_owned();
        }
        parts.push((name, text));
        i = data + size;
    }
    parts
}

#[test]
fn exporting_tables_to_excel() {
    // Page 1: a title, then a three-column table drawn as separately positioned text; page 2: prose.
    let mut page1 = String::from("BT /F1 16 Tf 300 740 Td (Statement) Tj ET\n");
    let rows = [["Item", "Qty", "Amount"], ["Widgets", "007", "1,234.50"], ["Refund", "2", "(12.00)"], ["Tax", "1", "12.5%"]];
    for (r, row) in rows.iter().enumerate() {
        let y = 680 - 24 * r;
        for (c, text) in row.iter().enumerate() {
            // Parentheses inside a PDF string are escaped.
            let text = text.replace('(', "\\(").replace(')', "\\)");
            page1.push_str(&format!("BT /F1 11 Tf {} {y} Td ({text}) Tj ET\n", 72 + 160 * c));
        }
    }
    let page2 = "BT /F1 11 Tf 72 700 Td (Just a sentence on the second page.) Tj ET";
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [4 0 R 6 0 R] /Count 2 /MediaBox [0 0 612 792] >>".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        "<< /Type /Page /Parent 2 0 R /Contents 5 0 R /Resources << /Font << /F1 3 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{page1}\nendstream", page1.len()),
        "<< /Type /Page /Parent 2 0 R /Contents 7 0 R /Resources << /Font << /F1 3 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{page2}\nendstream", page2.len()),
    ];
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());

    let dir = workdir("xlsx");
    std::fs::write(dir.join("table.pdf"), pdf).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "table.pdf" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "doc_export_office", json!({ "doc": doc, "path": "table.xlsx" }));
    assert_eq!(r["format"], "xlsx", "{r}");
    let bytes = std::fs::read(dir.join("table.xlsx")).unwrap();
    assert_eq!(r["bytes"], bytes.len());
    let parts = unzip(&bytes);
    let part = |name: &str| parts.iter().find(|p| p.0 == name).map(|p| p.1.clone()).unwrap_or_else(|| panic!("{name} missing: {parts:?}"));
    let names: Vec<&str> = parts.iter().map(|p| p.0.as_str()).collect();
    for n in [
        "[Content_Types].xml",
        "_rels/.rels",
        "xl/workbook.xml",
        "xl/_rels/workbook.xml.rels",
        "xl/styles.xml",
        "xl/worksheets/sheet1.xml",
        "xl/worksheets/sheet2.xml",
    ] {
        assert!(names.contains(&n), "{n}: {names:?}");
    }
    assert!(
        part("xl/workbook.xml").contains("<sheet name=\"Page 1\" sheetId=\"1\" r:id=\"rId1\"/><sheet name=\"Page 2\" sheetId=\"2\" r:id=\"rId2\"/>")
    );

    // Every cell of sheet 1 as (reference, inline text or "=number").
    let sheet = part("xl/worksheets/sheet1.xml");
    let cells: Vec<(String, String)> = sheet
        .split("<c r=\"")
        .skip(1)
        .map(|c| {
            let r = c.split('"').next().unwrap().to_string();
            let v = if c.contains("t=\"inlineStr\"") {
                c.split("xml:space=\"preserve\">").nth(1).unwrap().split("</t>").next().unwrap().to_string()
            } else {
                format!("={}", c.split("<v>").nth(1).unwrap().split("</v>").next().unwrap())
            };
            (r, v)
        })
        .collect();
    let expected = [
        ("A1", "Statement"),
        ("A2", "Item"),
        ("B2", "Qty"),
        ("C2", "Amount"),
        ("A3", "Widgets"),
        ("B3", "007"),
        ("C3", "=1234.5"),
        ("A4", "Refund"),
        ("B4", "=2"),
        ("C4", "=-12"),
        ("A5", "Tax"),
        ("B5", "=1"),
        ("C5", "=0.125"),
    ];
    let got: Vec<(&str, &str)> = cells.iter().map(|(r, v)| (r.as_str(), v.as_str())).collect();
    assert_eq!(got, expected, "{sheet}");
    let styles = part("xl/styles.xml");
    assert!(styles.contains("formatCode=\"#,##0.00\"") && styles.contains("formatCode=\"0.0%\""), "{styles}");
    assert!(part("xl/worksheets/sheet2.xml").contains(">Just a sentence on the second page.</t>"));

    // Python's standard library opens the package and parses every part, when it is installed.
    if std::process::Command::new("python3").arg("--version").output().is_ok_and(|o| o.status.success()) {
        let script = "import sys, zipfile, xml.dom.minidom as m\nz = zipfile.ZipFile(sys.argv[1])\nassert z.testzip() is None\n\
for n in z.namelist(): m.parseString(z.read(n))\nprint(len(z.namelist()))";
        let out = std::process::Command::new("python3").arg("-c").arg(script).arg(dir.join("table.xlsx")).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "8", "content types, two rels, core, workbook, styles and two sheets");
    }
}

#[test]
fn dynamic_xfa_forms_open_render_fill_and_save_through_tools() {
    let dir = workdir("xfa");
    std::fs::write(dir.join("xfa.pdf"), pdfcraft_xfa::fixtures::shell(&pdfcraft_xfa::fixtures::template(2))).unwrap();
    let mut a = auto(&dir);
    let opened = ok(&mut a, "doc_open", json!({ "path": "xfa.pdf" }));
    assert_eq!(opened["pages"], 2, "laid out from the template, not the placeholder page");
    let doc = opened["doc"].as_u64().unwrap();
    let info = ok(&mut a, "doc_info", json!({ "doc": doc }));
    assert_eq!(info["xfa"], "dynamic");
    assert_eq!(info["xfa_layout"]["pages"], 2);
    assert_eq!(info["xfa_layout"]["fields"], 11);
    let fields = ok(&mut a, "form_fields", json!({ "doc": doc }));
    let family = fields["fields"].as_array().unwrap().iter().find(|f| f["name"] == "familyName").expect("familyName");
    assert_eq!((family["type"].as_str(), family["tooltip"].as_str(), family["page"].as_u64()), (Some("text"), Some("Your family name"), Some(1)));
    let answer = fields["fields"].as_array().unwrap().iter().find(|f| f["name"] == "answer").expect("radio group");
    assert_eq!(answer["options"], json!(["Y", "N"]));
    // The page renders with the widgets' own appearances: the check box's border is drawn.
    let png = a.call("page_render", &json!({ "doc": doc, "page": 1, "dpi": 36 })).unwrap();
    let Content::Png { data, .. } = &png[0] else { panic!("expected an image") };
    let decoder = png::Decoder::new(std::io::Cursor::new(data.as_slice()));
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut buf).unwrap();
    assert!(buf.iter().filter(|b| **b < 128).count() > 200, "the page is not blank");
    ok(&mut a, "form_fill", json!({ "doc": doc, "values": { "familyName": "Singh", "agree": true, "answer": "N", "born": "2001-02-03" } }));
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "out.pdf" }));
    ok(&mut a, "doc_close", json!({ "doc": doc }));
    let reopened = ok(&mut a, "doc_open", json!({ "path": "out.pdf" }));
    assert_eq!(reopened["pages"], 2, "not laid out twice");
    let doc2 = reopened["doc"].as_u64().unwrap();
    let fields = ok(&mut a, "form_fields", json!({ "doc": doc2 }));
    let by = |n: &str| fields["fields"].as_array().unwrap().iter().find(|f| f["name"] == n).unwrap()["value"].clone();
    assert_eq!((by("familyName"), by("agree"), by("answer"), by("born")), (json!("Singh"), json!(true), json!("N"), json!("2001-02-03")));
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc2 }))["xfa_layout"]["pages"], 2);
}

#[test]
fn cut_stack_printing_through_tools() {
    let dir = workdir("cut-stack");
    std::fs::write(dir.join("numbered.pdf"), fixture(10)).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({"path": "numbered.pdf"}))["doc"].as_u64().unwrap();
    let before = page_text(&mut a, doc);
    for reverse in [false, true] {
        let r = ok(
            &mut a,
            "doc_print",
            json!({
                "doc": doc, "layout": "multiple", "order": "cut-stack", "per_sheet": 4,
                "orientation": "portrait", "auto_rotate": false, "reverse": reverse, "path": "cut.pdf"
            }),
        );
        assert_eq!(r["sheets"], 3);
        let printed = ok(&mut a, "doc_open", json!({"path": "cut.pdf"}))["doc"].as_u64().unwrap();
        let expected =
            if reverse { vec![vec![10, 7, 4, 1], vec![9, 6, 3], vec![8, 5, 2]] } else { vec![vec![1, 4, 7, 10], vec![2, 5, 8], vec![3, 6, 9]] };
        for (text, expected) in page_text(&mut a, printed).iter().zip(&expected) {
            let actual: Vec<usize> = text.split_whitespace().filter_map(|t| t.parse().ok()).collect();
            assert_eq!(&actual, expected, "saved PDF must contain the imposed order: {text}");
        }
        ok(&mut a, "doc_close", json!({"doc": printed}));
    }
    let r = ok(
        &mut a,
        "doc_print",
        json!({
            "doc": doc, "pages": "2-10", "subset": "odd", "reverse": true,
            "layout": "multiple", "order": "cut-stack", "per_sheet": 2, "auto_rotate": false,
            "orientation": "landscape", "path": "range.pdf"
        }),
    );
    assert_eq!(r["pages"], 5);
    let printed = ok(&mut a, "doc_open", json!({"path": "range.pdf"}))["doc"].as_u64().unwrap();
    assert_eq!(page_text(&mut a, printed), ["Page 10\nPage 4", "Page 8\nPage 2", "Page 6"]);
    assert_eq!(page_text(&mut a, doc), before);
    assert_eq!(ok(&mut a, "doc_info", json!({"doc": doc}))["document"]["dirty"], false);
    for duplex in ["long-edge", "short-edge"] {
        assert!(matches!(
            a.call(
                "doc_print",
                &json!({
                    "doc": doc, "layout": "multiple", "order": "cut-stack", "duplex": duplex, "path": "refused.pdf"
                })
            ),
            Err(ToolError::InvalidArgs(_))
        ));
    }
    assert!(matches!(a.call("doc_print", &json!({"doc": doc, "order": "cut-stack", "path": "refused.pdf"})), Err(ToolError::InvalidArgs(_))));
    assert!(!dir.join("refused.pdf").exists());
    assert!(a.call("doc_print", &json!({"doc": doc, "layout": "multiple", "order": "cut-stack", "path": "../escaped.pdf"})).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn measurements_calibrate_draw_save_reopen_and_export() {
    let dir = workdir("measurements");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({"path":"a.pdf"}))["doc"].as_u64().unwrap();
    let scale = ok(&mut a, "measure_scale", json!({"doc":doc,"page":1,"points":[[10,10],[70,10]],"distance":6,"unit":"m","precision":3}));
    assert!((scale["scale"]["x"].as_f64().unwrap() - 0.1).abs() < 1e-12);
    for (tool, points) in [
        ("measure_distance", json!([[10, 20], [70, 100]])),
        ("measure_perimeter", json!([[10, 20], [70, 20], [70, 100]])),
        ("measure_area", json!([[10, 20], [70, 20], [70, 100], [10, 100]])),
    ] {
        ok(&mut a, tool, json!({"doc":doc,"page":1,"points":points,"label":"Room, \"A\"","author":"Tester"}));
    }
    let all = ok(&mut a, "measure_list", json!({"doc":doc}));
    assert_eq!(all["count"], 3);
    assert_eq!(all["unsupported"], json!([]));
    assert_eq!(all["truncated"], false);
    for (m, value) in all["measurements"].as_array().unwrap().iter().zip([10.0, 14.0, 48.0]) {
        assert!((m["reading"]["value"].as_f64().unwrap() - value).abs() < 1e-6);
        assert_eq!(m["page"], 1);
        assert_eq!(m["label"], "Room, \"A\"");
    }
    let preview = ok(&mut a, "measure_info", json!({"doc":doc,"page":1,"type":"area","points":[[10,20],[70,20],[70,100],[10,100]]}));
    assert!((preview["reading"]["value"].as_f64().unwrap() - 48.0).abs() < 1e-6);
    let rendered = a.call("page_render", &json!({"doc":doc,"page":1,"dpi":72})).unwrap();
    assert!(matches!(rendered.first(), Some(Content::Png { .. })));
    ok(&mut a, "edit_undo", json!({"doc":doc}));
    assert_eq!(ok(&mut a, "measure_list", json!({"doc":doc}))["count"], 2);
    ok(&mut a, "edit_redo", json!({"doc":doc}));
    ok(&mut a, "doc_save", json!({"doc":doc,"path":"measured.pdf"}));
    let reopened = ok(&mut a, "doc_open", json!({"path":"measured.pdf"}))["doc"].as_u64().unwrap();
    let after = ok(&mut a, "measure_list", json!({"doc":reopened}));
    assert_eq!(after["measurements"], all["measurements"]);
    let exported = ok(&mut a, "measure_export", json!({"doc":reopened,"out":"measurements.csv"}));
    assert_eq!((exported["count"].as_u64(), exported["unsupported"].as_u64()), (Some(3), Some(0)));
    let csv = std::fs::read_to_string(dir.join("measurements.csv")).unwrap();
    assert!(csv.contains("area,48,\"m^2\",\"Room, \"\"A\"\"\""), "{csv}");
    assert!(a.call("measure_export", &json!({"doc":doc,"out":"../outside.csv"})).is_err());
    // A new viewport changes future readings, without recalibrating saved measurements.
    ok(&mut a, "measure_scale", json!({"doc":doc,"page":1,"units_per_point":1,"rect":[0,0,50,50],"unit":"cm"}));
    assert_eq!(ok(&mut a, "measure_scale", json!({"doc":doc,"page":1,"at":[20,20]}))["scale"]["unit"], "cm");
    assert_eq!(ok(&mut a, "measure_scale", json!({"doc":doc,"page":1,"at":[80,80]}))["scale"]["unit"], "m");
    assert_eq!(ok(&mut a, "measure_list", json!({"doc":doc}))["measurements"], all["measurements"]);
    // Rotate the page, then measure in its displayed coordinates.
    ok(&mut a, "page_rotate", json!({"doc":doc,"pages":[1],"degrees":90}));
    ok(&mut a, "measure_distance", json!({"doc":doc,"page":1,"points":[[100,100],[180,160]]}));
    let all = ok(&mut a, "measure_list", json!({"doc":doc}));
    let last = all["measurements"].as_array().unwrap().last().unwrap();
    assert!((last["reading"]["value"].as_f64().unwrap() - 10.0).abs() < 1e-6);
    assert_eq!(last["points"], json!([[100.0, 100.0], [180.0, 160.0]]));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn measurements_bad_arguments_leave_document_and_history_unchanged() {
    let dir = workdir("measurement-errors");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({"path":"a.pdf"}))["doc"].as_u64().unwrap();
    let before = ok(&mut a, "doc_list", json!({}));
    for (tool, args) in [
        ("measure_distance", json!({"doc":doc,"page":1,"points":[[0,0]]})),
        ("measure_distance", json!({"doc":doc,"page":1,"points":[[0,0],[0,0]]})),
        ("measure_area", json!({"doc":doc,"page":1,"points":[[0,0],[20,20],[0,20],[20,0]]})),
        ("measure_scale", json!({"doc":doc,"page":1,"points":[[0,0],[0,0]],"distance":10})),
        ("measure_snap", json!({"doc":doc,"page":1,"at":[1e100,0]})),
        ("measure_scale", json!({"doc":doc,"page":1,"units_per_point":1,"precision":8})),
    ] {
        assert!(a.call(tool, &args).is_err(), "{tool} {args}");
        assert_eq!(ok(&mut a, "doc_list", json!({})), before);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn measurement_snap_tool_covers_all_targets() {
    let dir = workdir("measurement-snap");
    let mut a = auto(&dir);
    let source = String::from_utf8(fixture(1)).unwrap();
    let old = "BT /F1 24 Tf 20 150 Td (Page 1) Tj ET";
    let drawing = "10 20 m 110 20 l S 60 0 m 60 80 l S";
    assert!(drawing.len() <= old.len());
    let source = source.replace(old, &format!("{drawing:<width$}", width = old.len()));
    std::fs::write(dir.join("drawing.pdf"), source).unwrap();
    let doc = ok(&mut a, "doc_open", json!({"path":"drawing.pdf"}))["doc"].as_u64().unwrap();
    for (at, kind, point, midpoints) in [
        ([11, 280], "endpoint", [10, 280], true),
        ([60, 259], "midpoint", [60, 260], true),
        ([59, 279], "intersection", [60, 280], false),
        ([32, 278], "path", [32, 280], true),
    ] {
        let snap = ok(&mut a, "measure_snap", json!({"doc":doc,"page":1,"at":at,"tolerance":3,"midpoints":midpoints}));
        assert_eq!(snap["snap"]["kind"], kind);
        assert_eq!(snap["snap"]["point"], json!(point.map(f64::from)));
        assert_eq!(snap["truncated"], false);
    }
    assert!(ok(&mut a, "measure_snap", json!({"doc":doc,"page":1,"at":[180,180]}))["snap"].is_null());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn xfa_scripts_run_for_buttons_and_field_changes_through_tools() {
    let dir = workdir("xfa-scripts");
    std::fs::write(dir.join("scripted.pdf"), pdfcraft_xfa::fixtures::shell(&pdfcraft_xfa::fixtures::scripted_template())).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "scripted.pdf" }))["doc"].as_u64().unwrap();
    let field = |a: &mut Automation, n: &str| {
        let f = ok(a, "form_fields", json!({ "doc": doc }));
        f["fields"].as_array().unwrap().iter().find(|f| f["name"] == n).cloned()
    };
    // Opening ran the initialize and calculate scripts.
    assert_eq!(field(&mut a, "qty").unwrap()["value"], "2");
    assert_eq!(field(&mut a, "total").unwrap()["value"], "10");
    // Filling recalculates; a bad value shows its message (the tool reports alerts in js output? no: it is applied, the value stays).
    ok(&mut a, "form_fill", json!({ "doc": doc, "values": { "qty": "4" } }));
    assert_eq!(field(&mut a, "total").unwrap()["value"], "20");
    // A button's XFA click script runs through js_run, like any button.
    let before = ok(&mut a, "doc_info", json!({ "doc": doc }))["xfa_layout"]["fields"].as_u64().unwrap();
    let r = ok(&mut a, "js_run", json!({ "doc": doc, "script": "", "field": "addRow" }));
    assert!(r["error"].is_null(), "{r}");
    assert!(field(&mut a, "amount_2").is_some());
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc }))["xfa_layout"]["fields"].as_u64().unwrap(), before + 2);
    // Undo takes the row away again.
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert!(field(&mut a, "amount_2").is_none());
    let r = ok(&mut a, "js_run", json!({ "doc": doc, "script": "", "field": "hello" }));
    assert_eq!(r["alerts"], json!(["Hello 4"]));
}

/// doc_info describes a link's set-layer-visibility action by layer name.
#[test]
fn doc_info_describes_set_layer_links() {
    let dir = workdir("layer-links");
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [4 0 R 5 0 R] /D << >> >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [6 0 R] >>",
        "<< /Type /OCG /Name (Red) >>",
        "<< /Type /OCG /Name (Green) >>",
        "<< /Type /Annot /Subtype /Link /Rect [10 10 90 30] /A << /S /SetOCGState /State [/Toggle 5 0 R /OFF 4 0 R 9 0 R] /PreserveRB false >> >>",
    ];
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    std::fs::write(dir.join("layers.pdf"), pdf).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "layers.pdf" }))["doc"].as_u64().unwrap();
    let info = ok(&mut a, "doc_info", json!({ "doc": doc }));
    // A group that isn't a layer has no name.
    assert_eq!(
        info["links"][0]["target"],
        json!({
            "layers": [{ "layer": "Green", "state": "toggle" }, { "layer": "Red", "state": "off" }, { "layer": null, "state": "off" }],
            "preserve_rb": false,
        })
    );
}

mod close_argument_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Create exclusively and remove only this test's directory, never a pre-existing one.
    struct CloseDir(PathBuf);

    impl CloseDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let parent = std::env::temp_dir();
            for _ in 0..128 {
                let serial = NEXT.fetch_add(1, Ordering::Relaxed);
                let path = parent.join(format!("pdfcraft-close-{}-{serial}", std::process::id()));
                match std::fs::create_dir(&path) {
                    Ok(()) => {
                        let dir = Self(path);
                        std::fs::write(dir.0.join("a.pdf"), fixture(3)).unwrap();
                        std::fs::write(dir.0.join("b.pdf"), fixture(2)).unwrap();
                        return dir;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("creating close test directory: {error}"),
                }
            }
            panic!("no unused close test directory after 128 attempts");
        }
    }

    impl Drop for CloseDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn close_rejects_invalid_discard_types_without_changing_clean_or_dirty_documents() {
        let dir = CloseDir::new();
        let source = std::fs::read(dir.0.join("a.pdf")).unwrap();
        let other_source = std::fs::read(dir.0.join("b.pdf")).unwrap();
        let mut a = auto(&dir.0);
        let other = ok(&mut a, "doc_open", json!({ "path": "b.pdf" }))["doc"].as_u64().unwrap();
        for dirty in [false, true] {
            let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
            if dirty {
                ok(&mut a, "doc_set_info", json!({ "doc": doc, "key": "Title", "value": "Unsaved title" }));
            }
            let list = ok(&mut a, "doc_list", json!({}));
            let info = ok(&mut a, "doc_info", json!({ "doc": doc }));
            let text = ok(&mut a, "text_extract", json!({ "doc": doc }));
            let bytes = a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes.clone();
            for invalid in [json!("false"), json!(0), json!([]), json!({})] {
                let error = a.call("doc_close", &json!({ "doc": doc, "discard_changes": invalid })).unwrap_err();
                assert!(matches!(error, ToolError::InvalidArgs(ref message) if message == "discard_changes must be true or false"));
                assert_eq!(ok(&mut a, "doc_list", json!({})), list, "all document identities, order, paths and history stay unchanged");
                assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc })), info);
                assert_eq!(ok(&mut a, "text_extract", json!({ "doc": doc })), text);
                let current = a.session().get(pdfcraft_engine::DocId(doc)).unwrap();
                assert_eq!(current.bytes, bytes);
                assert_eq!(current.dirty, dirty);
                assert_eq!(std::fs::read(dir.0.join("a.pdf")).unwrap(), source);
                assert_eq!(std::fs::read(dir.0.join("b.pdf")).unwrap(), other_source);
            }
            assert_eq!(ok(&mut a, "doc_close", json!({ "doc": doc, "discard_changes": true }))["closed"], doc);
            assert!(a.session().get(pdfcraft_engine::DocId(doc)).is_none());
            assert!(a.session().get(pdfcraft_engine::DocId(other)).is_some());
        }
    }

    #[test]
    fn close_keeps_default_and_boolean_discard_controls() {
        let dir = CloseDir::new();
        let source = std::fs::read(dir.0.join("a.pdf")).unwrap();
        let mut a = auto(&dir.0);
        let other = ok(&mut a, "doc_open", json!({ "path": "b.pdf" }))["doc"].as_u64().unwrap();
        for discard in [None, Some(Value::Null), Some(json!(false)), Some(json!(true))] {
            let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
            let mut args = json!({ "doc": doc });
            if let Some(discard) = discard {
                args["discard_changes"] = discard;
            }
            assert_eq!(ok(&mut a, "doc_close", args)["closed"], doc);
            assert!(a.session().get(pdfcraft_engine::DocId(doc)).is_none());
            assert!(a.session().get(pdfcraft_engine::DocId(other)).is_some());
        }
        let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
        ok(&mut a, "doc_set_info", json!({ "doc": doc, "key": "Title", "value": "Unsaved title" }));
        let before = ok(&mut a, "doc_list", json!({}));
        let info = ok(&mut a, "doc_info", json!({ "doc": doc }));
        let bytes = a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes.clone();
        for args in [json!({ "doc": doc }), json!({ "doc": doc, "discard_changes": null }), json!({ "doc": doc, "discard_changes": false })] {
            let error = a.call("doc_close", &args).unwrap_err();
            assert!(matches!(error, ToolError::Failed(ref message) if message.contains("unsaved changes")));
            assert_eq!(ok(&mut a, "doc_list", json!({})), before);
            assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc })), info);
            assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes, bytes);
        }
        assert_eq!(ok(&mut a, "doc_close", json!({ "doc": doc, "discard_changes": true }))["closed"], doc);
        assert!(a.session().get(pdfcraft_engine::DocId(doc)).is_none());
        assert!(a.session().get(pdfcraft_engine::DocId(other)).is_some());
        assert_eq!(std::fs::read(dir.0.join("a.pdf")).unwrap(), source);
    }
}

mod combine_argument_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Own only a newly created directory; the bounded runner provides project-local TMPDIR.
    struct CombineDir(PathBuf);

    impl CombineDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let parent = std::env::temp_dir();
            for _ in 0..128 {
                let serial = NEXT.fetch_add(1, Ordering::Relaxed);
                let path = parent.join(format!("pdfcraft-combine-{}-{serial}", std::process::id()));
                match std::fs::create_dir(&path) {
                    Ok(()) => {
                        let dir = Self(path);
                        std::fs::write(dir.0.join("a.pdf"), fixture(3)).unwrap();
                        std::fs::write(dir.0.join("b.pdf"), fixture(2)).unwrap();
                        return dir;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("creating combine test directory: {error}"),
                }
            }
            panic!("no unused combine test directory after 128 attempts");
        }
    }

    impl Drop for CombineDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn invalid_combine_selectors_preserve_outputs_inputs_and_open_documents() {
        let dir = CombineDir::new();
        let first = std::fs::read(dir.0.join("a.pdf")).unwrap();
        let second = std::fs::read(dir.0.join("b.pdf")).unwrap();
        let sentinel = b"existing destination";
        std::fs::write(dir.0.join("combined.pdf"), sentinel).unwrap();
        let mut a = auto(&dir.0);
        let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
        ok(&mut a, "doc_set_info", json!({ "doc": doc, "key": "Title", "value": "Unsaved title" }));
        ok(&mut a, "doc_open", json!({ "path": "b.pdf" }));
        let list = ok(&mut a, "doc_list", json!({}));
        let info = ok(&mut a, "doc_info", json!({ "doc": doc }));
        let text = page_text(&mut a, doc);
        let bytes = a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes.clone();
        for invalid in [json!(1), json!(true), json!({ "page": 1 }), json!(["1"])] {
            for (index, pages) in [json!([invalid, null]), json!([null, invalid])].into_iter().enumerate() {
                for out in ["combined.pdf", "not-created.pdf"] {
                    let error = a.call("doc_combine", &json!({ "paths": ["a.pdf", "b.pdf"], "pages": pages, "out": out, "open": true })).unwrap_err();
                    assert!(
                        matches!(error, ToolError::InvalidArgs(ref message) if message == &format!("pages[{index}] must be a range string or null"))
                    );
                    assert_eq!(std::fs::read(dir.0.join("combined.pdf")).unwrap(), sentinel);
                    assert!(!dir.0.join("not-created.pdf").exists());
                    assert_eq!(ok(&mut a, "doc_list", json!({})), list);
                    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc })), info);
                    assert_eq!(page_text(&mut a, doc), text);
                    assert_eq!(a.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes, bytes);
                    assert_eq!(std::fs::read(dir.0.join("a.pdf")).unwrap(), first);
                    assert_eq!(std::fs::read(dir.0.join("b.pdf")).unwrap(), second);
                }
            }
        }
        let error = a
            .call("doc_combine", &json!({ "paths": ["missing-a.pdf", "missing-b.pdf"], "pages": [null, false], "out": "not-created.pdf" }))
            .unwrap_err();
        assert!(matches!(error, ToolError::InvalidArgs(ref message) if message == "pages[1] must be a range string or null"));
        assert_eq!(ok(&mut a, "doc_list", json!({})), list);
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 3, "no output or staging files are created");
    }

    #[test]
    fn valid_combine_selectors_keep_all_pages_and_selected_order_after_reopen() {
        let dir = CombineDir::new();
        let first = std::fs::read(dir.0.join("a.pdf")).unwrap();
        let second = std::fs::read(dir.0.join("b.pdf")).unwrap();
        let mut a = auto(&dir.0);
        let all = vec!["Page 1", "Page 2", "Page 3", "Page 1", "Page 2"];
        for (pages, expected) in [
            (None, all.clone()),
            (Some(Value::Null), all.clone()),
            (Some(json!([null, null])), all.clone()),
            (Some(json!(["", null])), all),
            (Some(json!(["3, 1", null])), vec!["Page 3", "Page 1", "Page 1", "Page 2"]),
            (Some(json!([null, "2"])), vec!["Page 1", "Page 2", "Page 3", "Page 2"]),
        ] {
            let mut args = json!({ "paths": ["a.pdf", "b.pdf"], "out": "combined.pdf", "open": false });
            if let Some(pages) = pages {
                args["pages"] = pages;
            }
            let result = ok(&mut a, "doc_combine", args);
            assert!(result["bytes"].as_u64().unwrap() > 0);
            assert!(result.get("document").is_none());
            assert!(a.session().docs().is_empty());
            let mut fresh = auto(&dir.0);
            let opened = ok(&mut fresh, "doc_open", json!({ "path": "combined.pdf" }));
            assert_eq!(opened["pages"].as_u64().unwrap(), expected.len() as u64);
            assert_eq!(page_text(&mut fresh, opened["doc"].as_u64().unwrap()), expected);
            assert_eq!(std::fs::read(dir.0.join("a.pdf")).unwrap(), first);
            assert_eq!(std::fs::read(dir.0.join("b.pdf")).unwrap(), second);
        }
    }
}

#[cfg(feature = "mcp")]
#[test]
fn conventions_core_tools_preserve_root_and_batch_errors() {
    let dir = workdir("conventions-core");
    let mut s = McpServer::new(auto(&dir));
    let run = |s: &mut McpServer, name: &str, args: Value| rpc(s, 1, "tools/call", json!({"name":name,"arguments":args}));
    assert_eq!(run(&mut s, "doc_inspect", json!({}))["result"]["structuredContent"]["documents"], json!([]));
    let opened = run(&mut s, "command_run", json!({"id":"file.open","params":{"path":"a.pdf"}}));
    assert_eq!(opened["result"]["isError"], false, "{opened}");
    let doc = opened["result"]["structuredContent"]["doc"].as_u64().unwrap();
    let filtered = run(&mut s, "command_list", json!({"doc":doc,"filter":"rotate","enabled_only":true}));
    let commands = filtered["result"]["structuredContent"]["commands"].as_array().unwrap();
    assert!(!commands.is_empty());
    assert!(commands.iter().all(|c| c["enabled"] == true));
    assert!(commands.iter().find(|c| c["id"] == "page.rotate").unwrap()["params"].is_object());
    for stop in [true, false] {
        let r = run(
            &mut s,
            "command_batch",
            json!({"steps":[{"id":"page.rotate","params":{"doc":doc,"degrees":90}},{"id":"no.such.command"},{"id":"page.rotate","params":{"doc":doc,"degrees":90}}],"stop_on_error":stop}),
        );
        assert_eq!(r["result"]["isError"], true, "{r}");
        assert_eq!(r["result"]["structuredContent"]["completed"], if stop { 1 } else { 2 });
        assert_eq!(r["result"]["structuredContent"]["failed"], 1);
    }
    let r = run(&mut s, "command_run", json!({"id":"file.save","params":{"doc":doc,"path":"../escaped.pdf"}}));
    assert_eq!(r["result"]["isError"], true);
    assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("outside the allowed directory"));
    let r = run(&mut s, "command_run", json!({"id":"page.rotate","params":{"doc":doc,"degrees":90,"typo":1}}));
    assert_eq!(r["result"]["isError"], false);
    assert!(r["result"]["structuredContent"]["warnings"][0].as_str().unwrap().contains("typo"));
    let before = run(&mut s, "doc_info", json!({"doc":doc}));
    let r = run(&mut s, "render_preview", json!({"doc":doc,"page":1,"max_side":64}));
    assert_eq!(r["result"]["isError"], false, "{r}");
    use base64::Engine as _;
    let png = base64::engine::general_purpose::STANDARD.decode(r["result"]["content"][0]["data"].as_str().unwrap()).unwrap();
    let image = image::load_from_memory(&png).unwrap();
    assert!(image.width().max(image.height()) <= 64);
    assert_eq!(before["result"], run(&mut s, "doc_info", json!({"doc":doc}))["result"]);
}

#[cfg(feature = "mcp")]
#[test]
fn conventions_annotations_and_strict_keys_in_both_modes() {
    for compact in [false, true] {
        let mut s = McpServer::new(Automation::new()).with_compact(compact);
        let r = rpc(&mut s, 1, "tools/list", json!({}));
        let tools = r["result"]["tools"].as_array().unwrap();
        for name in ["command_list", "command_run", "command_batch", "doc_inspect", "render_preview"] {
            assert!(tools.iter().any(|t| t["name"] == name), "missing {name}");
        }
        for tool in tools {
            for hint in ["readOnlyHint", "destructiveHint", "idempotentHint", "openWorldHint"] {
                assert!(tool["annotations"][hint].is_boolean(), "{}: {hint}", tool["name"]);
            }
            let r = rpc(&mut s, 2, "tools/call", json!({"name":tool["name"],"arguments":{"bogus_arg":1}}));
            assert_eq!(r["error"]["code"], -32602, "{}", tool["name"]);
            let message = r["error"]["message"].as_str().unwrap();
            assert!(message.contains("bogus_arg") && message.contains("expected:"));
        }
    }
}

#[cfg(feature = "mcp")]
#[test]
fn conventions_resources() {
    let dir = workdir("conventions-resources");
    let mut s = McpServer::new(auto(&dir));
    let read = |s: &mut McpServer, uri: &str| {
        let r = rpc(s, 1, "resources/read", json!({"uri":uri}));
        serde_json::from_str::<Value>(r["result"]["contents"][0]["text"].as_str().expect("JSON resource")).unwrap()
    };
    assert_eq!(read(&mut s, "pdfcraft://document"), json!({"documents":[]}));
    rpc(&mut s, 2, "tools/call", json!({"name":"doc_open","arguments":{"path":"a.pdf"}}));
    let doc = rpc(&mut s, 3, "tools/call", json!({"name":"doc_inspect","arguments":{}}));
    assert_eq!(read(&mut s, "pdfcraft://document"), doc["result"]["structuredContent"]);
    let commands = rpc(&mut s, 4, "tools/call", json!({"name":"command_list","arguments":{}}));
    assert_eq!(read(&mut s, "pdfcraft://commands"), commands["result"]["structuredContent"]);
    // Responses keep the shape every supported protocol revision expects: no cache hints.
    for (method, params) in [("tools/list", json!({})), ("resources/list", json!({})), ("resources/read", json!({"uri":"pdfcraft://document"}))] {
        assert!(rpc(&mut s, 5, method, params)["result"].get("resultType").is_none());
    }
    assert_eq!(rpc(&mut s, 7, "initialize", json!({"protocolVersion":"2026-07-28"}))["result"]["protocolVersion"], "2025-06-18");
    assert!(s.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":999}}"#).is_none());
    assert_eq!(
        rpc(&mut s, 9, "tools/call", json!({"name":"doc_inspect","arguments":{},"_meta":{"progressToken":"quick"}}))["result"]["isError"],
        false
    );
}

#[test]
fn command_batch_refuses_more_than_a_thousand_steps() {
    let dir = workdir("batch-cap");
    let mut a = auto(&dir);
    let steps: Vec<Value> = (0..1001).map(|_| json!({"id":"no.such.command"})).collect();
    let err = a.call("command_batch", &json!({"steps": steps})).unwrap_err();
    assert!(err.to_string().contains("at most 1000 steps"), "{err}");
}

#[test]
fn page_transitions_through_tools() {
    let dir = workdir("transitions");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "page_transitions", json!({ "doc": doc }));
    assert!(r["pages"].as_array().unwrap().iter().all(|p| p["style"] == "none" && p["advance"].is_null()), "{r}");
    // Fly in from nothing on every page, advancing after 5 s.
    let r = ok(
        &mut a,
        "page_set_transition",
        json!({ "doc": doc, "style": "Fly", "duration": 2, "motion": "outward", "direction": "none", "scale": 0.25, "opaque": true, "advance": 5 }),
    );
    assert_eq!(r["pages"].as_array().unwrap().len(), 3);
    assert_eq!(r["pages"][2]["direction"], "none");
    // Page 2 gets a vertical Blinds instead and advances manually.
    ok(&mut a, "page_set_transition", json!({ "doc": doc, "pages": [2], "style": "Blinds", "dimension": "vertical" }));
    let r = ok(&mut a, "page_transitions", json!({ "doc": doc }));
    assert_eq!(
        (r["pages"][0]["style"].as_str(), r["pages"][0]["advance"].as_f64(), r["pages"][0]["scale"].as_f64()),
        (Some("Fly"), Some(5.0), Some(0.25))
    );
    assert_eq!((r["pages"][1]["style"].as_str(), r["pages"][1]["dimension"].as_str()), (Some("Blinds"), Some("vertical")));
    assert!(r["pages"][1]["advance"].is_null());
    // Invalid combinations are refused without changing anything.
    let before = ok(&mut a, "page_transitions", json!({ "doc": doc }));
    for bad in [
        json!({ "doc": doc, "style": "Push", "direction": 90 }),
        json!({ "doc": doc, "style": "Fly", "direction": "none" }),
        json!({ "doc": doc, "style": "Dissolve", "duration": 0 }),
        json!({ "doc": doc, "style": "Wipe", "advance": -1 }),
    ] {
        assert!(matches!(a.call("page_set_transition", &bad), Err(ToolError::Failed(_))), "{bad}");
    }
    assert!(matches!(a.call("page_set_transition", &json!({ "doc": doc, "style": "Spin" })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(a.call("page_set_transition", &json!({ "doc": doc, "style": "none", "duration": 2 })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(a.call("page_set_transition", &json!({ "doc": doc, "pages": [4], "style": "Fade" })), Err(ToolError::InvalidArgs(_))));
    assert_eq!(ok(&mut a, "page_transitions", json!({ "doc": doc })), before);
    // Saved and reopened, the settings and the page text survive; undo is one step.
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "transitions.pdf" }));
    let back = ok(&mut a, "doc_open", json!({ "path": "transitions.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "page_transitions", json!({ "doc": back }))["pages"], before["pages"]);
    assert_eq!(page_text(&mut a, back), ["Page 1", "Page 2", "Page 3"]);
    ok(&mut a, "page_set_transition", json!({ "doc": back, "pages": [1], "style": "none" }));
    let r = ok(&mut a, "page_transitions", json!({ "doc": back }));
    assert_eq!((r["pages"][0]["style"].as_str(), r["pages"][0]["advance"].is_null()), (Some("none"), true));
    ok(&mut a, "edit_undo", json!({ "doc": back }));
    assert_eq!(ok(&mut a, "page_transitions", json!({ "doc": back }))["pages"], before["pages"]);
}

/// A one-page form: `person.name` (text under a non-terminal parent) and `agree` (check box), with
/// `/Helv` in `/DR` being `font`; the page reads `label`.
fn conflict_form(value: &str, font: &str, label: &str) -> Vec<u8> {
    let content = format!("BT /F1 18 Tf 20 250 Td ({label}) Tj ET");
    let objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R 7 0 R] /DR << /Font << /Helv 4 0 R >> >> /DA (/Helv 0 Tf 0 g) >> >>".into(), // 1
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>".into(), // 2
        "<< /Type /Page /Parent 2 0 R /Contents 8 0 R /Resources << /Font << /F1 4 0 R >> >> /Annots [6 0 R 7 0 R] >>".into(), // 3
        format!("<< /Type /Font /Subtype /Type1 /BaseFont /{font} /Encoding /WinAnsiEncoding >>"), // 4
        "<< /T (person) /Kids [6 0 R] >>".into(), // 5
        format!("<< /Type /Annot /Subtype /Widget /Parent 5 0 R /T (name) /FT /Tx /V ({value}) /DA (/Helv 12 Tf 0 g) /Rect [20 180 180 200] /P 3 0 R >>"), // 6
        "<< /Type /Annot /Subtype /Widget /T (agree) /FT /Btn /V /Off /AS /Off /Rect [20 140 40 160] /P 3 0 R /AP << /N << /Yes 9 0 R /Off 10 0 R >> >> >>".into(), // 7
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()), // 8
        "<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 18 >>\nstream\n0 g 0 0 20 20 re f\nendstream".into(), // 9
        "<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 0 >>\nstream\n\nendstream".into(), // 10
    ];
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

/// name → value of every field of `doc`.
fn field_values(a: &mut Automation, doc: u64) -> Vec<(String, Value)> {
    let list = ok(a, "form_fields", json!({ "doc": doc }));
    list["fields"].as_array().unwrap().iter().map(|f| (f["name"].as_str().unwrap().to_string(), f["value"].clone())).collect()
}

/// Combining two copies of a form keeps their values apart (colliding names are renamed), the
/// merge option joins compatible fields instead, and inserting pages from a form behaves the same.
#[test]
fn combining_forms_keeps_colliding_fields_independent() {
    let dir = workdir("field-conflicts");
    std::fs::write(dir.join("form-a.pdf"), conflict_form("Ada", "Helvetica", "Form A")).unwrap();
    std::fs::write(dir.join("form-b.pdf"), conflict_form("Bob", "Courier", "Form B")).unwrap();
    let mut a = auto(&dir);
    let r = ok(&mut a, "doc_combine", json!({ "paths": ["form-a.pdf", "form-b.pdf"], "open": true }));
    assert_eq!(r["fields"]["renamed"], json!([{ "from": "person", "to": "person#2" }, { "from": "agree", "to": "agree#2" }]), "{r}");
    assert_eq!(r["fields"]["resources_renamed"], json!([{ "from": "Font/Helv", "to": "Font/Helv_2" }]));
    assert_eq!(r["fields"]["merged"], json!([]));
    let doc = r["document"]["doc"].as_u64().unwrap();
    let values = field_values(&mut a, doc);
    assert_eq!(
        values,
        [
            ("person.name".to_string(), json!("Ada")),
            ("agree".to_string(), json!(false)),
            ("person#2.name".to_string(), json!("Bob")),
            ("agree#2".to_string(), json!(false)),
        ]
    );
    // Filling the second file's fields leaves the first file's alone.
    ok(&mut a, "form_fill", json!({ "doc": doc, "values": { "person#2.name": "Carol", "agree#2": true } }));
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "combined-filled.pdf" }));
    let mut b = auto(&dir);
    let re = ok(&mut b, "doc_open", json!({ "path": "combined-filled.pdf" }))["doc"].as_u64().unwrap();
    let values = field_values(&mut b, re);
    assert_eq!(
        values,
        [
            ("person.name".to_string(), json!("Ada")),
            ("agree".to_string(), json!(false)),
            ("person#2.name".to_string(), json!("Carol")),
            ("agree#2".to_string(), json!(true)),
        ]
    );
    // The new value is drawn on page 2 only, with the second file's font found under its new name.
    let found = ok(&mut b, "text_find", json!({ "doc": re, "query": "Carol" }));
    assert_eq!(found["count"], 1, "{found}");

    // Merge: the check boxes become one field with two widgets; the hierarchy is still renamed.
    let r = ok(&mut a, "doc_combine", json!({ "paths": ["form-a.pdf", "form-b.pdf"], "field_conflicts": "merge", "open": true }));
    assert_eq!(r["fields"]["merged"], json!(["agree"]), "{r}");
    assert_eq!(r["fields"]["renamed"], json!([{ "from": "person", "to": "person#2" }]));
    let merged = r["document"]["doc"].as_u64().unwrap();
    ok(&mut a, "form_fill", json!({ "doc": merged, "values": { "agree": true } }));
    let names: Vec<String> = field_values(&mut a, merged).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["person.name", "agree", "person#2.name"]);
    assert_eq!(field_values(&mut a, merged)[1].1, json!(true));
    assert!(matches!(
        a.call("doc_combine", &json!({ "paths": ["form-a.pdf", "form-b.pdf"], "field_conflicts": "share" })),
        Err(ToolError::InvalidArgs(m)) if m.contains("field_conflicts")
    ));

    // Create PDF from multiple files shares the code and the option.
    let r = ok(&mut a, "doc_create_multiple", json!({ "paths": ["form-a.pdf", "form-b.pdf"], "open": true }));
    assert_eq!(r["fields"]["renamed"].as_array().map(Vec::len), Some(2), "{r}");

    // Insert Pages from File renames too, and the result saves and reopens.
    let target = ok(&mut a, "doc_open", json!({ "path": "form-a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "page_insert_file", json!({ "doc": target, "path": "form-b.pdf", "at": 2 }));
    ok(&mut a, "form_fill", json!({ "doc": target, "values": { "person.name": "Dee" } }));
    ok(&mut a, "doc_save", json!({ "doc": target, "path": "inserted.pdf" }));
    let mut c = auto(&dir);
    let re = ok(&mut c, "doc_open", json!({ "path": "inserted.pdf" }))["doc"].as_u64().unwrap();
    let values = field_values(&mut c, re);
    assert_eq!(values[0], ("person.name".to_string(), json!("Dee")));
    assert_eq!(values[2], ("person#2.name".to_string(), json!("Bob")));

    // The schema documents the option.
    for tool in ["doc_combine", "doc_create_multiple"] {
        let def = tools().into_iter().find(|t| t.name == tool).unwrap();
        assert_eq!(def.input_schema["properties"]["field_conflicts"]["enum"], json!(["rename", "merge"]), "{tool}");
    }
}

/// Named destinations (`dest_*`), links and bookmarks that go to views with zoom or to named
/// destinations, and bookmark actions, through save and reopen; `pdfinfo -dests` (Poppler, run
/// only as an external oracle process) must read the same destinations when it is installed.
#[test]
fn named_destinations_views_and_bookmark_actions_through_tools() {
    let dir = workdir("dests");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "dest_list", json!({ "doc": doc }))["count"], 0);
    let r = ok(&mut a, "dest_set", json!({ "doc": doc, "name": "intro", "page": 2, "top": 250, "zoom": 150 }));
    assert_eq!(r["created"], true);
    assert_eq!(r["destination"]["view"], json!({ "page": 2, "fit": "XYZ", "top": 250.0, "zoom": 150.0 }));
    ok(&mut a, "dest_set", json!({ "doc": doc, "name": "wide", "page": 3, "fit": "FitH", "top": 100 }));
    let r = ok(&mut a, "dest_set", json!({ "doc": doc, "name": "box", "page": 1, "rect": [110, 20, 10, 220] }));
    assert_eq!(r["destination"]["view"], json!({ "page": 1, "fit": "FitR", "rect": [10.0, 20.0, 110.0, 220.0] }));
    // Re-target an existing destination.
    let r = ok(&mut a, "dest_set", json!({ "doc": doc, "name": "intro", "page": 3, "fit": "XYZ", "left": 0, "top": 300, "zoom": 125 }));
    assert_eq!(r["created"], false);
    // Refusals change nothing.
    for bad in [
        json!({ "doc": doc, "name": "x", "page": 1, "fit": "FitH", "zoom": 200 }),
        json!({ "doc": doc, "name": "x", "page": 1, "fit": "Sideways" }),
        json!({ "doc": doc, "name": "x", "page": 9 }),
        json!({ "doc": doc, "name": "x", "page": 1, "fit": "FitR" }),
    ] {
        assert!(matches!(a.call("dest_set", &bad), Err(ToolError::InvalidArgs(_))), "{bad}");
    }
    for bad in [json!({ "doc": doc, "name": "x", "page": 1, "zoom": 100000 }), json!({ "doc": doc, "name": "x", "page": 1, "rect": [0, 0, 0, 10] })] {
        assert!(matches!(a.call("dest_set", &bad), Err(ToolError::Failed(_))), "{bad}");
    }
    assert!(matches!(a.call("dest_rename", &json!({ "doc": doc, "name": "box", "new_name": "wide" })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("dest_delete", &json!({ "doc": doc, "name": "nope" })), Err(ToolError::Failed(_))));
    assert_eq!(ok(&mut a, "dest_list", json!({ "doc": doc }))["count"], 3);

    // Links: a view with zoom, and a named destination.
    ok(&mut a, "link_add", json!({ "doc": doc, "page": 1, "rect": [10, 10, 100, 30], "to_page": 2, "view": { "left": 0, "top": 300, "zoom": 200 } }));
    ok(&mut a, "link_add", json!({ "doc": doc, "page": 1, "rect": [10, 40, 100, 60], "dest": "wide" }));
    ok(&mut a, "link_add", json!({ "doc": doc, "page": 1, "rect": [10, 70, 100, 90], "to_page": 3 }));
    assert!(matches!(a.call("link_add", &json!({ "doc": doc, "page": 1, "rect": [10, 70, 100, 90], "dest": "missing" })), Err(ToolError::Failed(_))));
    assert!(matches!(
        a.call("link_add", &json!({ "doc": doc, "page": 1, "rect": [10, 70, 100, 90], "dest": "wide", "to_page": 1 })),
        Err(ToolError::InvalidArgs(_))
    ));
    assert!(matches!(
        a.call("link_add", &json!({ "doc": doc, "page": 1, "rect": [10, 70, 100, 90], "view": { "fit": "Fit" } })),
        Err(ToolError::InvalidArgs(_))
    ));
    assert!(matches!(
        a.call("link_add", &json!({ "doc": doc, "page": 1, "rect": [10, 70, 100, 90], "to_page": 1, "view": { "fit": "Fit", "zoom": 2 } })),
        Err(ToolError::InvalidArgs(_))
    ));
    assert!(matches!(
        a.call("link_add", &json!({ "doc": doc, "page": 1, "rect": [10, 70, 100, 90], "to_page": 1, "view": { "scale": 2 } })),
        Err(ToolError::InvalidArgs(_))
    ));

    // Bookmarks: a named destination, a web page, a view.
    ok(&mut a, "bookmark_add", json!({ "doc": doc, "title": "Intro", "page": 1 }));
    ok(&mut a, "bookmark_add", json!({ "doc": doc, "title": "Web", "page": 1 }));
    ok(&mut a, "bookmark_add", json!({ "doc": doc, "title": "Zoomed", "page": 1 }));
    ok(&mut a, "bookmark_set_action", json!({ "doc": doc, "path": [1], "dest": "intro" }));
    ok(&mut a, "bookmark_set_action", json!({ "doc": doc, "path": [2], "url": "https://example.org/" }));
    ok(&mut a, "bookmark_set_action", json!({ "doc": doc, "path": [3], "to_page": 2, "view": { "fit": "FitV", "left": 50 } }));
    assert!(matches!(a.call("bookmark_set_action", &json!({ "doc": doc, "path": [1] })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(
        a.call("bookmark_set_action", &json!({ "doc": doc, "path": [1], "url": "https://x.org", "dest": "intro" })),
        Err(ToolError::InvalidArgs(_))
    ));
    assert!(matches!(a.call("bookmark_set_action", &json!({ "doc": doc, "path": [9], "url": "https://x.org" })), Err(ToolError::Failed(_))));

    // Rename follows references; delete reports what still points at the name; undo restores it.
    let r = ok(&mut a, "dest_rename", json!({ "doc": doc, "name": "wide", "new_name": "Wide view" }));
    assert_eq!(r["references_updated"], 1);
    let r = ok(&mut a, "dest_delete", json!({ "doc": doc, "name": "box" }));
    assert_eq!(r["still_referenced"], 0);
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(ok(&mut a, "dest_list", json!({ "doc": doc }))["count"], 3);
    ok(&mut a, "dest_delete", json!({ "doc": doc, "name": "box" }));

    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "dests.pdf" }));
    let mut b = auto(&dir);
    let re = ok(&mut b, "doc_open", json!({ "path": "dests.pdf" }))["doc"].as_u64().unwrap();
    let dests = ok(&mut b, "dest_list", json!({ "doc": re }));
    let by_name = |n: &str| dests["destinations"].as_array().unwrap().iter().find(|d| d["name"] == n).cloned().unwrap_or(Value::Null);
    assert_eq!(dests["count"], 2);
    assert_eq!(by_name("intro")["view"], json!({ "page": 3, "fit": "XYZ", "left": 0.0, "top": 300.0, "zoom": 125.0 }));
    assert_eq!(by_name("intro")["references"], 1);
    assert_eq!(by_name("Wide view")["view"], json!({ "page": 3, "fit": "FitH", "top": 100.0 }));
    assert_eq!(by_name("Wide view")["references"], 1);
    assert_eq!(by_name("Wide view")["stored_in"], "names");

    let links = ok(&mut b, "link_list", json!({ "doc": re }))["links"].clone();
    let link = |i: u64| links.as_array().unwrap().iter().find(|l| l["index"] == i).cloned().unwrap();
    assert_eq!(link(1)["to_page"], 2);
    assert_eq!(link(1)["view"], json!({ "page": 2, "fit": "XYZ", "left": 0.0, "top": 300.0, "zoom": 200.0 }));
    assert_eq!(link(2)["dest"], "Wide view");
    assert_eq!(link(2)["to_page"], 3);
    assert_eq!(link(3)["to_page"], 3);
    assert!(link(3).get("view").is_none(), "a plain page link stays Fit");

    let marks = ok(&mut b, "bookmark_list", json!({ "doc": re }))["bookmarks"].clone();
    assert_eq!(marks[0]["action"]["type"], "goto");
    assert_eq!(marks[0]["action"]["dest"], "intro");
    assert_eq!(marks[0]["action"]["to_page"], 3);
    assert_eq!(marks[1]["action"], json!({ "type": "uri", "url": "https://example.org/" }));
    assert_eq!(marks[2]["action"]["view"], json!({ "page": 2, "fit": "FitV", "left": 50.0 }));

    // Re-target a link from its view to a named destination after reopening.
    ok(&mut b, "link_edit", json!({ "doc": re, "page": 1, "index": 1, "dest": "intro" }));
    let links = ok(&mut b, "link_list", json!({ "doc": re }))["links"].clone();
    assert_eq!(links[0]["dest"], "intro");

    // External oracle: Poppler reads the same named destinations.
    match std::process::Command::new("pdfinfo").arg("-dests").arg(dir.join("dests.pdf")).output() {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let line = |n: &str| text.lines().find(|l| l.contains(&format!("\"{n}\""))).map(str::to_string).unwrap_or_default();
            let intro = line("intro");
            assert!(intro.trim_start().starts_with('3') && intro.contains("XYZ"), "pdfinfo -dests: {text}");
            let wide = line("Wide view");
            assert!(wide.trim_start().starts_with('3') && wide.contains("FitH"), "pdfinfo -dests: {text}");
            assert!(!text.contains("\"box\""), "pdfinfo -dests: {text}");
        }
        _ => eprintln!("pdfinfo not available: external destination check skipped"),
    }
}

/// A replace-text proposal made from found text survives an XFDF round trip into another copy
/// of the file, and summaries lay its page out beside (connector lines) or before (sequence
/// numbers) the proposal and its replacement.
#[test]
fn replace_text_round_trips_and_summaries_show_the_page() {
    let dir = workdir("replace-summary");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 2, "type": "replace", "find": "page 2", "contents": "second page", "author": "Ada" }));
    ok(&mut a, "doc_export_data", json!({ "doc": doc, "path": "proposal.xfdf", "what": "comments" }));
    let xfdf = std::fs::read_to_string(dir.join("proposal.xfdf")).unwrap();
    assert!(xfdf.contains("intent=\"StrikeOutTextEdit\"") && xfdf.contains("replyType=\"Group\""), "{xfdf}");

    std::fs::copy(dir.join("a.pdf"), dir.join("copy.pdf")).unwrap();
    let copy = ok(&mut a, "doc_open", json!({ "path": "copy.pdf" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "doc_import_data", json!({ "doc": copy, "path": "proposal.xfdf" }));
    assert_eq!(r["comments"], 2, "{r}");
    let list = ok(&mut a, "comment_list", json!({ "doc": copy, "page": 2 }));
    assert_eq!(list["count"], 1, "the caret threads under the strikeout: {list}");
    let strike = &list["comments"][0];
    assert_eq!((strike["type"].as_str(), strike["replies"][0]["contents"].as_str()), (Some("StrikeOut"), Some("second page")), "{list}");

    for (layout, pages) in [("connector-lines", 1), ("sequence-numbers", 2)] {
        let out = format!("{layout}.pdf");
        ok(&mut a, "comments_summarize", json!({ "doc": copy, "layout": layout, "out": out }));
        let s = ok(&mut a, "doc_open", json!({ "path": out }))["doc"].as_u64().unwrap();
        let text = page_text(&mut a, s);
        assert_eq!(text.len(), pages, "{layout}: {text:?}");
        let all = text.join("\n");
        for want in ["Replace Text", "Replacement Text", "second page", "Page 2"] {
            assert!(all.contains(want), "{layout}: {want:?} in {all:?}");
        }
    }
    assert!(matches!(a.call("comments_summarize", &json!({ "doc": copy, "layout": "poster" })), Err(ToolError::InvalidArgs(_))));
}

/// Certificate (public-key) security end to end through the tools: digital IDs made with
/// sign_id_create (throwaway, generated at run time), protect for two recipients with their own
/// permissions, save, then open in fresh sessions with each recipient's digital ID.
#[test]
fn certificate_security_through_tools() {
    let dir = workdir("certsec");
    let mut a = auto(&dir);
    for (name, file) in [("Boss Test", "boss.p12"), ("Reader Test", "reader.p12"), ("Stranger Test", "stranger.p12")] {
        ok(&mut a, "sign_id_create", json!({ "name": name, "password": "id-secret", "path": file }));
    }
    // One recipient as a PEM certificate file (no private key needed to encrypt for it).
    let reader = pdfcraft_engine::sign::pkcs12::open(&std::fs::read(dir.join("reader.p12")).unwrap(), "id-secret").unwrap();
    std::fs::write(dir.join("reader.pem"), pdfcraft_engine::sign::x509::to_pem(&reader.certificate)).unwrap();
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    // Refused: no recipients, a wrong digital ID password, an unknown option.
    assert!(a.call("security_protect_certificates", &json!({ "doc": doc, "recipients": [] })).is_err());
    let wrong = a.call("security_protect_certificates", &json!({ "doc": doc, "recipients": [{ "certificate": "boss.p12", "password": "nope" }] }));
    assert!(matches!(wrong, Err(ToolError::InvalidArgs(m)) if m.contains("recipients[0]")));
    assert!(
        a.call("security_protect_certificates", &json!({ "doc": doc, "recipients": [{ "certificate": "reader.pem", "printing": "sometimes" }] }))
            .is_err()
    );
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc }))["security"]["protected"], false, "refused calls change nothing");
    let r = ok(
        &mut a,
        "security_protect_certificates",
        json!({ "doc": doc, "recipients": [
            { "certificate": "boss.p12", "password": "id-secret", "change_security": true },
            { "certificate": "reader.pem", "printing": "low", "changes": "comment-fill-sign" },
        ] }),
    );
    assert!(!r.to_string().contains("id-secret"), "passwords are never echoed");
    assert_eq!((r["security"]["handler"].as_str(), r["security"]["pending"].as_bool()), (Some("certificate"), Some(true)));
    assert_eq!(r["security"]["method"], "Certificate security (AES, 256-bit)");
    assert_eq!(r["recipients"][0]["name"], "Boss Test");
    assert_eq!(r["recipients"][1]["permissions"]["printing"], "low");
    assert_eq!(r["recipients"][1]["permissions"]["change_security"], false);
    assert_eq!(page_text(&mut a, doc), ["Page 1", "Page 2", "Page 3"], "still viewable in this session");
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "cert.pdf" }));
    let raw = std::fs::read(dir.join("cert.pdf")).unwrap();
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("/Adobe.PubSec") && text.contains("/adbe.pkcs7.s5") && !text.contains("(Page 1)"));
    // A fresh session: passwords and strangers can't open it.
    let mut b = auto(&dir);
    let fail = |r: Result<Vec<pdfcraft_automation::Content>, ToolError>| match r {
        Err(e) => e.to_string(),
        Ok(_) => panic!("expected an error"),
    };
    assert!(fail(b.call("doc_open", &json!({ "path": "cert.pdf" }))).contains("digital ID"));
    assert!(
        fail(b.call("doc_open", &json!({ "path": "cert.pdf", "certificate": "stranger.p12", "certificate_password": "id-secret" })))
            .contains("not one of the recipients")
    );
    assert!(
        fail(b.call("doc_open", &json!({ "path": "cert.pdf", "certificate": "reader.p12", "certificate_password": "nope" })))
            .contains("password is incorrect")
    );
    assert!(b.call("doc_open", &json!({ "path": "cert.pdf", "certificate": "reader.p12", "password": "x" })).is_err());
    // The reader: its permissions, nothing more.
    let re = ok(&mut b, "doc_open", json!({ "path": "cert.pdf", "certificate": "reader.p12", "certificate_password": "id-secret" }));
    assert!(!re.to_string().contains("id-secret"));
    let s = &re["security"];
    assert_eq!(
        (s["handler"].as_str(), s["printing"].as_str(), s["annotate"].as_bool(), s["copy"].as_bool(), s["assemble"].as_bool()),
        (Some("certificate"), Some("low"), Some(true), Some(false), Some(false))
    );
    let re = re["doc"].as_u64().unwrap();
    // Copying is not among the reader's permissions, so its page text is withheld.
    assert!(fail(b.call("text_extract", &json!({ "doc": re }))).contains("does not allow copying"));
    assert!(b.call("page_delete", &json!({ "doc": re, "pages": [1] })).is_err());
    assert!(b.call("doc_unprotect", &json!({ "doc": re })).is_err());
    ok(&mut b, "comment_add", json!({ "doc": re, "page": 1, "type": "note", "at": [10, 10], "contents": "reviewed by reader" }));
    ok(&mut b, "doc_save", json!({ "doc": re, "path": "commented.pdf" }));
    assert!(
        !String::from_utf8_lossy(&std::fs::read(dir.join("commented.pdf")).unwrap()).contains("reviewed by reader"),
        "the comment is encrypted too"
    );
    // The boss is owner-equivalent: sees the comment and may remove the security.
    let boss = ok(&mut b, "doc_open", json!({ "path": "commented.pdf", "certificate": "boss.p12", "certificate_password": "id-secret" }))["doc"]
        .as_u64()
        .unwrap();
    assert!(ok(&mut b, "comment_list", json!({ "doc": boss })).to_string().contains("reviewed by reader"));
    assert_eq!(ok(&mut b, "doc_unprotect", json!({ "doc": boss }))["security"]["protected"], false);
    ok(&mut b, "doc_save", json!({ "doc": boss, "path": "open.pdf" }));
    let mut c = auto(&dir);
    let plain = ok(&mut c, "doc_open", json!({ "path": "open.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(page_text(&mut c, plain), ["Page 1", "Page 2", "Page 3"]);
    // An ordinary document opened with a digital ID just opens.
    ok(&mut c, "doc_open", json!({ "path": "b.pdf", "certificate": "boss.p12", "certificate_password": "id-secret" }));
}

#[test]
fn ocr_output_choice_is_validated_before_recognition() {
    let dir = workdir("ocr-output");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    for tool in ["ocr_recognize", "ocr_prepare"] {
        assert!(matches!(a.call(tool, &json!({ "doc": doc, "output": "resampled" })), Err(ToolError::InvalidArgs(_))), "{tool}");
    }
    assert!(matches!(
        a.call("ocr_recognize_files", &json!({ "paths": ["a.pdf"], "folder": "out", "output": "resampled" })),
        Err(ToolError::InvalidArgs(_))
    ));
}

#[test]
fn formatting_a_text_box_through_tools() {
    let dir = workdir("rich-text");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let added = ok(
        &mut a,
        "comment_add",
        json!({ "doc": doc, "page": 1, "type": "textbox", "rect": [10, 200, 190, 240], "contents": "Hello world", "font_size": 10 }),
    );
    let id = added["comment"]["id"].as_str().unwrap().to_string();
    let r =
        ok(&mut a, "comment_format_text", json!({ "doc": doc, "id": id, "start": 6, "end": 11, "bold": true, "color": "#FF0000", "font": "Times" }));
    assert_eq!(r["runs"][0]["text"], "Hello ");
    assert_eq!(
        (r["runs"][1]["text"].as_str(), r["runs"][1]["bold"].as_bool(), r["runs"][1]["font"].as_str()),
        (Some("world"), Some(true), Some("Times"))
    );
    assert_eq!(r["runs"][1]["color"], "#FF0000");
    let r = ok(&mut a, "comment_format_text", json!({ "doc": doc, "id": id, "size": 18, "align": "center" }));
    assert_eq!((r["align"].as_str(), r["runs"][0]["size"].as_f64(), r["runs"][1]["size"].as_f64()), (Some("center"), Some(18.0), Some(18.0)));
    let list = ok(&mut a, "comment_list", json!({ "doc": doc }));
    let c = list["comments"].as_array().unwrap().iter().find(|c| c["id"] == id.as_str()).unwrap();
    assert_eq!(c["runs"].as_array().map(Vec::len), Some(2));
    // Ranges past the text, nothing to change and other comments are refused.
    for bad in [json!({ "doc": doc, "id": id, "end": 99, "bold": true }), json!({ "doc": doc, "id": id }), json!({ "doc": doc, "id": id, "size": 0 })]
    {
        assert!(matches!(a.call("comment_format_text", &bad), Err(ToolError::InvalidArgs(_))), "{bad}");
    }
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "rectangle", "rect": [10, 10, 60, 40] }));
    assert!(matches!(a.call("comment_format_text", &json!({ "doc": doc, "page": 1, "index": 2, "bold": true })), Err(ToolError::InvalidArgs(_))));
    // Undo takes the formatting back in one step.
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    let list = ok(&mut a, "comment_list", json!({ "doc": doc }));
    let c = list["comments"].as_array().unwrap().iter().find(|c| c["id"] == id.as_str()).unwrap();
    assert_eq!((c["align"].as_str(), c["runs"][0]["size"].as_f64()), (Some("left"), Some(10.0)));
}

/// A one-request-at-a-time HTTP server on loopback: every request it gets (head and body) comes
/// out of the receiver, answered with `status`.
fn form_server(status: u16) -> (String, std::sync::mpsc::Receiver<(String, Vec<u8>)>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { return };
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
            let _ =
                s.write_all(format!("HTTP/1.1 {status} X\r\nContent-Type: text/html\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").as_bytes());
            if tx.send((head, body)).is_err() {
                return;
            }
        }
    });
    (addr, rx)
}

#[test]
fn submitting_a_form_saves_or_sends_its_data() {
    let dir = workdir("form-submit");
    let mut a = auto(&dir);
    let (server, requests) = form_server(200);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "text", "rect": [20, 60, 180, 82], "name": "Name" }));
    ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "text", "rect": [20, 100, 180, 122], "name": "Town" }));
    ok(&mut a, "form_fill", json!({ "doc": doc, "values": { "Name": "Ada Lovelace", "Town": "London & Co" } }));
    ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "button", "rect": [20, 20, 120, 42], "name": "Send" }));
    let url = format!("{server}/forms/in");
    ok(&mut a, "form_set_actions", json!({ "doc": doc, "field": "Send", "actions": [{ "trigger": "mouse_up", "submit": url }] }));

    // Saved instead of sent: the button's HTML format, every filled field, nothing on the wire.
    let r = ok(&mut a, "form_submit", json!({ "doc": doc, "field": "Send", "path": "data.txt" }));
    assert_eq!((r["format"].as_str(), r["method"].as_str(), r["fields"].clone()), (Some("HTML"), Some("POST"), json!(["Name", "Town"])), "{r}");
    assert_eq!(std::fs::read_to_string(dir.join("data.txt")).unwrap(), "Name=Ada+Lovelace&Town=London+%26+Co");
    assert!(requests.try_recv().is_err(), "saving sends nothing");
    // As XFDF, to a file.
    ok(&mut a, "form_submit", json!({ "doc": doc, "field": "Send", "format": "xfdf", "path": "data.xfdf" }));
    let xfdf = std::fs::read_to_string(dir.join("data.xfdf")).unwrap();
    assert!(xfdf.contains("<field name=\"Name\"><value>Ada Lovelace</value></field>") && !xfdf.contains("Send"), "{xfdf}");

    // Sent: a POST of the form body to the button's address.
    let r = ok(&mut a, "form_submit", json!({ "doc": doc, "field": "Send", "send": true }));
    assert_eq!(r["status"], 200, "{r}");
    let (head, body) = requests.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    assert!(head.starts_with("POST /forms/in HTTP/1.1"), "{head}");
    assert!(head.to_ascii_lowercase().contains("content-type: application/x-www-form-urlencoded"), "{head}");
    assert_eq!(body, b"Name=Ada+Lovelace&Town=London+%26+Co");
    // FDF to a script's address.
    let r = ok(&mut a, "form_submit", json!({ "doc": doc, "url": format!("{server}/fdf"), "send": true }));
    assert_eq!((r["format"].as_str(), r["status"].as_u64()), (Some("FDF"), Some(200)));
    let (head, body) = requests.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    assert!(head.to_ascii_lowercase().contains("content-type: application/vnd.fdf") && body.starts_with(b"%FDF-1.2"), "{head}");

    // Addresses that can't be sent to are refused before anything is built or sent.
    for bad in ["mailto:forms@example.org", "forms/in", "file:///tmp/x"] {
        let e = a.call("form_submit", &json!({ "doc": doc, "url": bad, "send": true }));
        assert!(matches!(e, Err(ToolError::InvalidArgs(_))), "{bad}: {e:?}");
    }
    assert!(requests.try_recv().is_err());
    // Exactly one of path and send; a button that doesn't submit is refused.
    assert!(matches!(a.call("form_submit", &json!({ "doc": doc, "field": "Send" })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(a.call("form_submit", &json!({ "doc": doc, "field": "Name", "path": "x.txt" })), Err(ToolError::InvalidArgs(_))));
}

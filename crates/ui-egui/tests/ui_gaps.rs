//! Comment, organize and search gaps in the real shell (egui_kittest): the snapped Insert Text
//! caret, comments in the find bar, hiding comments by type, typed page ranges, Bookmark
//! Properties and bookmarks from structure, Advanced Search over open documents and a folder,
//! associated files, Action Wizard folder runs and custom commands, with hostile input.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_engine::Session;
use pdfcraft_engine::tool_host::{ToolHost, ToolInfo};
use pdfcraft_ui_egui::search_ui::Scope;
use pdfcraft_ui_egui::{Dialog, PdfCraftApp};
use serde_json::{Value, json};

/// Three pages (300×400, 400×200, 300×400) ("The quick brown fox", "A second brown animal", "Third page"), the
/// second landscape-wide; page 1 has a sticky note ("fox note") and a square ("box"); one
/// bookmark to page 2; tagged with an H1 heading on page 1.
fn fixture() -> Vec<u8> {
    let page_text = ["The quick brown fox", "A second brown animal", "Third page"];
    let mut objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R /Outlines 12 0 R /MarkInfo << /Marked true >> /StructTreeRoot 14 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".into(),
    ];
    for i in 0..3 {
        let media = if i == 1 { "[0 0 400 200]" } else { "[0 0 300 400]" };
        let annots = if i == 0 { " /Annots [10 0 R 11 0 R] /StructParents 0" } else { "" };
        objs.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox {media} /Contents {} 0 R /Resources << /Font << /F1 9 0 R >> >>{annots} >>",
            6 + i
        ));
    }
    for (i, text) in page_text.iter().enumerate() {
        let body = if i == 0 {
            format!("/H1 << /MCID 0 >> BDC BT /F1 14 Tf 20 150 Td ({text}) Tj ET EMC")
        } else {
            format!("BT /F1 14 Tf 20 150 Td ({text}) Tj ET")
        };
        objs.push(format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()));
    }
    objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    objs.push("<< /Type /Annot /Subtype /Text /Rect [250 20 270 40] /Contents (fox note) /T (Ann) >>".into());
    objs.push("<< /Type /Annot /Subtype /Square /Rect [200 60 240 100] /Contents (box) /T (Ann) >>".into());
    objs.push("<< /Type /Outlines /First 13 0 R /Last 13 0 R /Count 1 >>".into());
    objs.push("<< /Title (Chapter) /Parent 12 0 R /Dest [4 0 R /XYZ null null null] >>".into());
    objs.push("<< /Type /StructTreeRoot /K [15 0 R] /ParentTree << /Nums [0 [15 0 R]] >> >>".into());
    objs.push("<< /Type /StructElem /S /H1 /P 14 0 R /Pg 3 0 R /K 0 >>".into());
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn harness(setup: impl FnOnce(&mut PdfCraftApp) + 'static) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.run_inline = true;
        app.open_bytes("doc.pdf", None, fixture()).expect("opens");
        setup(&mut app);
        app
    });
    settle(&mut h);
    h
}

fn settle(h: &mut Harness<'static, PdfCraftApp>) {
    for _ in 0..200 {
        h.run_steps(2);
        if !h.state().render_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn doc<'a>(h: &'a Harness<'static, PdfCraftApp>) -> &'a pdfcraft_engine::Document {
    let id = h.state().views[0].id;
    h.state().session.get(id).unwrap()
}

fn temp(test: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pdfcraft-ui-gaps-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn hide_comments_by_type_and_find_them() {
    let mut h = harness(|app| app.set_option("panel", "comments").unwrap());
    h.state_mut().set_hidden_comment_types(&["Square".to_string()]);
    settle(&mut h);
    assert_eq!(doc(&h).hidden_comment_types(), vec!["Square".to_string()]);
    assert!(doc(&h).comment_type_hidden("Square") && !doc(&h).comment_type_hidden("Text"));
    // Hostile names are refused and change nothing.
    h.state_mut().set_hidden_comment_types(&["Widget".to_string(), "../x".to_string()]);
    assert_eq!(doc(&h).hidden_comment_types(), vec!["Square".to_string()]);
    assert!(h.state_mut().execute("comment.show_all_types"));
    settle(&mut h);
    assert!(doc(&h).hidden_comment_types().is_empty());

    // Find ▸ Include comments: the sticky note's text matches alongside the page text.
    h.state_mut().views[0].open_find();
    {
        let f = h.state_mut().views[0].find.as_mut().unwrap();
        f.query = "fox".into();
        f.comments = true;
    }
    h.state_mut().views[0].rerun_find();
    settle(&mut h);
    let f = h.state().views[0].find.as_ref().unwrap();
    assert_eq!(f.comment_matches, vec![(0, 0)], "the note matches, the square doesn't");
    assert!(!f.matches.is_empty(), "the page text matches too");
    // Stepping reaches the comment and selects it.
    for _ in 0..4 {
        h.state_mut().views[0].find_step(true);
        if h.state().views[0].find.as_ref().unwrap().comment_current.is_some() {
            break;
        }
    }
    assert_eq!(h.state().views[0].comments.selected, Some((0, 0)));
    // Hostile queries: no panic, no matches.
    for q in ["\u{0}", &"x".repeat(100_000), "  "] {
        h.state_mut().views[0].find.as_mut().unwrap().query = q.to_string();
        h.state_mut().views[0].rerun_find();
        h.run_steps(2);
        assert!(h.state().views[0].find.as_ref().unwrap().comment_matches.is_empty());
    }
    assert!(pdfcraft_ui_egui::canvas::comment_text_matches("A fox, here", "FOX", false, true));
    assert!(!pdfcraft_ui_egui::canvas::comment_text_matches("foxes", "fox", false, true));
    assert!(!pdfcraft_ui_egui::canvas::comment_text_matches("fox", "Fox", true, false));
}

#[test]
fn insert_text_caret_snaps_to_the_insertion_point() {
    let mut h = harness(|app| {
        app.set_option("left", "closed").unwrap();
        app.set_option("author", "Tester").unwrap();
        app.set_option("quick", "caret").unwrap();
    });
    // Just above "quick" on the baseline (y = 150): the caret goes between two characters.
    let r = h.state().views[0].page_screen_rect(0).expect("page 1 on screen");
    let p = egui::pos2(r.left() + 52.0 / 300.0 * r.width(), r.top() + (400.0 - 158.0) / 400.0 * r.height());
    h.hover_at(p);
    h.run_steps(1);
    h.drag_at(p);
    h.run_steps(1);
    h.drop_at(p);
    h.run_steps(3);
    h.query_all_by(|n| n.placeholder() == Some("Text to insert")).next().expect("the composer opened").type_text("very ");
    h.run_steps(2);
    h.get_by_label("Post").click();
    settle(&mut h);
    let caret = doc(&h).info.annotations.iter().find(|a| a.subtype == "Caret").expect("a caret").clone();
    // Like Acrobat's insertion mark, it sits just under the line with its tip at the text
    // (between the baseline and the descender), sized to the line rather than fixed.
    assert!((caret.rect[3] - 150.0).abs() < 4.0, "snapped to the line: {:?}", caret.rect);
    assert!(caret.rect[2] - caret.rect[0] <= 14.0, "{:?}", caret.rect);
}

#[test]
fn select_pages_by_a_typed_range() {
    let mut h = harness(|_| {});
    assert!(h.state_mut().execute("page.select_range"));
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::SelectPages));
    h.state_mut().select_pages.range = "2-3".into();
    h.run_steps(1);
    h.get_by_label("Select").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
    assert_eq!(h.state().views[0].selected.iter().copied().collect::<Vec<_>>(), vec![1, 2]);
    // Landscape only narrows "all" to page 2.
    h.state_mut().select_pages.range.clear();
    h.state_mut().select_pages.orientation = pdfcraft_engine::PageOrientation::Landscape;
    assert!(h.state_mut().select_page_range());
    assert_eq!(h.state().views[0].selected.iter().copied().collect::<Vec<_>>(), vec![1]);
    h.state_mut().select_pages.orientation = pdfcraft_engine::PageOrientation::Both;
    // Hostile ranges: an error in the dialog, the selection kept.
    for bad in ["0", "4", "3-1-2", "99999999999999999999999", &"1,".repeat(10_000), "\u{202e}1", "1-2-"] {
        h.state_mut().select_pages.range = bad.to_string();
        assert!(!h.state_mut().select_page_range(), "{bad:?}");
        assert!(h.state().select_pages.error.is_some(), "{bad:?}");
        assert_eq!(h.state().views[0].selected.iter().copied().collect::<Vec<_>>(), vec![1], "{bad:?}");
    }
    // The organize grid's own field does the same.
    let pages = pdfcraft_engine::select_page_range(&doc(&h).info, "1, 3", Default::default(), Default::default()).unwrap();
    assert_eq!(pages, vec![0, 2]);
}

#[test]
fn bookmark_properties_and_bookmarks_from_structure() {
    let mut h = harness(|app| app.set_option("panel", "bookmarks").unwrap());
    assert!(h.state_mut().execute("bookmark.properties"));
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::BookmarkProperties));
    let p = h.state().bookmark_props.clone().expect("the dialog's state");
    assert_eq!((p.title.as_str(), p.page.as_str()), ("Chapter", "2"));
    {
        let p = h.state_mut().bookmark_props.as_mut().unwrap();
        p.title = "Second chapter".into();
        p.page = "3".into();
    }
    h.run_steps(1);
    h.get_by_label("OK").click();
    settle(&mut h);
    assert_eq!(h.state().dialog, None);
    let b = &doc(&h).info.outline[0];
    assert_eq!((b.title.as_str(), b.page), ("Second chapter", Some(2)));
    // Hostile values keep the dialog open with an error.
    h.state_mut().open_bookmark_props(vec![0]);
    for page in ["0", "4", "-1", "1e9", ""] {
        h.state_mut().bookmark_props.as_mut().unwrap().page = page.into();
        assert!(!h.state_mut().apply_bookmark_props(), "{page:?}");
    }
    {
        let p = h.state_mut().bookmark_props.as_mut().unwrap();
        p.page = "1".into();
        p.kind = pdfcraft_ui_egui::bookmark_ui::ActionKind::Web;
        p.url = "javascript:alert(1)".into();
    }
    assert!(!h.state_mut().apply_bookmark_props());
    h.state_mut().bookmark_props = None;
    h.state_mut().open_bookmark_props(vec![7, 3, 9]);
    assert!(h.state().bookmark_props.is_none(), "no such bookmark");
    h.state_mut().dialog = None;

    // New bookmarks from structure: the H1 heading.
    assert!(h.state_mut().execute("bookmark.from_structure"));
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::BookmarksFromStructure));
    h.get_by_label("Make bookmarks").click();
    settle(&mut h);
    assert_eq!(h.state().dialog, None, "{:?}", h.state().from_structure.error);
    assert!(doc(&h).info.outline.len() >= 2, "{:?}", doc(&h).info.outline);
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(doc(&h).info.outline.len(), 1);
}

#[test]
fn advanced_search_open_documents_and_a_folder() {
    let dir = temp("search");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let s = Session::new();
    std::fs::write(dir.join("a.pdf"), &*s.create_from_text("t", "brown bears").unwrap()).unwrap();
    std::fs::write(dir.join("sub/b.pdf"), &*s.create_from_text("t", "more brown").unwrap()).unwrap();
    std::fs::write(dir.join("junk.pdf"), b"%PDF-1.7 not really").unwrap();
    let outside = temp("search-outside");
    std::fs::write(outside.join("secret.pdf"), &*s.create_from_text("t", "brown secret").unwrap()).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.join("secret.pdf"), dir.join("link.pdf")).unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("sub/loop")).unwrap();
    }
    let mut h = harness(|app| {
        app.open_bytes("other.pdf", None, Session::new().create_from_text("t", "brown paper").unwrap().to_vec()).unwrap();
    });
    assert!(h.state_mut().execute("edit.search_folder"));
    h.run_steps(2);
    assert_eq!(h.state().adv_search.scope, Scope::Folder);
    h.state_mut().adv_search.query = "brown".into();
    h.state_mut().adv_search.folder = Some(dir.clone());
    h.state_mut().start_advanced_search();
    let r = h.state().adv_search.result.clone().unwrap().unwrap();
    assert_eq!(r["files_matched"], 1, "subfolders off: {r}");
    h.state_mut().adv_search.recursive = true;
    h.state_mut().start_advanced_search();
    let r = h.state().adv_search.result.clone().unwrap().unwrap();
    assert_eq!(r["files_matched"], 2, "a.pdf and sub/b.pdf, never the link out: {r}");
    h.run_steps(3);
    h.get_by_label_contains("b.pdf");

    // Open documents: both tabs.
    h.state_mut().adv_search.scope = Scope::OpenDocuments;
    h.state_mut().start_advanced_search();
    let r = h.state().adv_search.result.clone().unwrap().unwrap();
    assert_eq!(r["files_matched"], 2, "{r}");

    // Hostile: an empty or huge query, a folder that isn't there.
    for q in ["", "   ", &"q".repeat(100_000)] {
        h.state_mut().adv_search.query = q.to_string();
        h.state_mut().start_advanced_search();
        assert!(h.state().adv_search.result.clone().unwrap().is_err(), "{q:?}");
    }
    h.state_mut().adv_search.scope = Scope::Folder;
    h.state_mut().adv_search.query = "brown".into();
    h.state_mut().adv_search.folder = Some(dir.join("missing"));
    h.state_mut().start_advanced_search();
    assert!(h.state().adv_search.result.is_some());
    h.run_steps(3);
}

#[test]
fn associated_files_add_and_remove() {
    let mut h = harness(|_| {});
    assert!(h.state_mut().execute("file.associated_files"));
    h.run_steps(3);
    h.get_by_label("No associated files.");
    h.state_mut().assoc.file_override = Some(("data.csv".into(), b"a,b\n1,2\n".to_vec()));
    h.state_mut().assoc.relationship = Some(pdfcraft_engine::AfRelationship::Data);
    h.get_by_label("Add file…").click();
    settle(&mut h);
    let all = doc(&h).all_associated_files().unwrap();
    let files: Vec<_> = all.iter().flat_map(|(_, f)| f.files.iter()).collect();
    assert_eq!(files.len(), 1);
    assert_eq!((files[0].name.as_str(), files[0].relationship.as_deref()), ("data.csv", Some("Data")));
    h.run_steps(2);
    // Listed as an associated file (and, being a document file, as an attachment too).
    assert!(h.query_all_by_label("data.csv").count() >= 2);
    // Hostile: an empty name and an oversized file are refused.
    h.state_mut().assoc.file_override = Some((String::new(), b"x".to_vec()));
    h.get_by_label("Add file…").click();
    settle(&mut h);
    let big = vec![0u8; pdfcraft_engine::associated_files::MAX_FILE_BYTES + 1];
    h.state_mut().assoc.file_override = Some(("big.bin".into(), big));
    h.get_by_label("Add file…").click();
    settle(&mut h);
    assert_eq!(doc(&h).all_associated_files().unwrap().iter().map(|(_, f)| f.files.len()).sum::<usize>(), 1);
    h.get_by_label("Remove associated file").click();
    settle(&mut h);
    assert_eq!(doc(&h).all_associated_files().unwrap().iter().map(|(_, f)| f.files.len()).sum::<usize>(), 0);
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(doc(&h).all_associated_files().unwrap().iter().map(|(_, f)| f.files.len()).sum::<usize>(), 1);
}

#[test]
fn action_wizard_runs_on_a_folder_with_subfolders() {
    let dir = temp("batch");
    let s = Session::new();
    std::fs::create_dir_all(dir.join("in/sub/deep")).unwrap();
    std::fs::create_dir_all(dir.join("out")).unwrap();
    std::fs::write(dir.join("in/a.pdf"), &*s.create_from_text("t", "one").unwrap()).unwrap();
    std::fs::write(dir.join("in/sub/deep/b.pdf"), &*s.create_from_text("t", "two").unwrap()).unwrap();
    std::fs::write(dir.join("in/notes.txt"), b"not a pdf").unwrap();
    std::fs::write(dir.join("in/broken.pdf"), b"%PDF-1.7 garbage").unwrap();
    let outside = temp("batch-outside");
    std::fs::write(outside.join("secret.pdf"), &*s.create_from_text("t", "secret").unwrap()).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.join("secret.pdf"), dir.join("in/link.pdf")).unwrap();
        std::os::unix::fs::symlink(dir.join("in"), dir.join("in/sub/loop")).unwrap();
    }
    let d = dir.clone();
    let mut h = harness(move |app| {
        app.export_dir_override = Some(d.join("out").to_string_lossy().into_owned());
        app.action_files_override = Some(vec![d.join("in").to_string_lossy().into_owned()]);
    });
    assert!(h.state_mut().execute("actions.distribution"));
    h.run_steps(2);
    h.get_by_label("A folder…").click();
    h.run_steps(1);
    h.get_by_label("Include subfolders").click();
    h.run_steps(1);
    assert!(h.state().wizard.folder && h.state().wizard.recursive);
    h.get_by_label("Start").click();
    h.run_steps(3);
    let msg = h.state().toast.clone().unwrap().0;
    assert!(msg.starts_with("Prepare for Distribution: 2 files done"), "{msg}");
    assert!(msg.contains("broken.pdf"), "the broken file is reported: {msg}");
    assert!(std::fs::read(dir.join("out/a.pdf")).unwrap().starts_with(b"%PDF"));
    assert!(std::fs::read(dir.join("out/sub/deep/b.pdf")).unwrap().starts_with(b"%PDF"), "subfolders kept");
    assert!(!dir.join("out/link.pdf").exists(), "a link out of the folder is not followed");
    assert!(h.state().action_run.is_none());
    // Result paths stay inside the results folder.
    assert!(pdfcraft_ui_egui::actions_safe_relative("../x.pdf").is_none());
    assert!(pdfcraft_ui_egui::actions_safe_relative("/etc/x.pdf").is_none());
    assert!(pdfcraft_ui_egui::actions_safe_relative("").is_none());
    assert_eq!(pdfcraft_ui_egui::actions_safe_relative("a/./b.pdf"), Some(PathBuf::from("a/b.pdf")));
}

/// A tool host that keeps commands in memory and records runs.
#[derive(Default)]
struct FakeHost {
    commands: Mutex<Vec<Value>>,
    runs: Mutex<Vec<Value>>,
}

impl ToolHost for FakeHost {
    fn call(&self, _session: &mut Session, name: &str, args: &Value) -> Result<Value, String> {
        let mut commands = self.commands.lock().unwrap();
        match name {
            "custom_command_list" => Ok(json!({ "commands": *commands })),
            "custom_command_create" => {
                if args["params"]["degrees"].as_i64().is_some_and(|d| d % 90 != 0) {
                    return Err("degrees must be a multiple of 90".into());
                }
                commands.push(json!({ "id": args["id"], "label": args["label"], "tool": args["tool"], "params": args["params"], "prompt_user": args["prompt_user"] }));
                Ok(json!({ "created": args["id"] }))
            }
            "custom_command_delete" => {
                commands.retain(|c| c["id"] != args["id"]);
                Ok(json!({ "deleted": args["id"] }))
            }
            "custom_command_run" => {
                self.runs.lock().unwrap().push(args.clone());
                Ok(json!({ "ran": args["id"] }))
            }
            other => Err(format!("unexpected tool {other}")),
        }
    }

    fn document_tools(&self) -> Vec<ToolInfo> {
        vec![ToolInfo {
            name: "page_rotate".into(),
            title: "Rotate pages".into(),
            description: "Rotate pages by a multiple of 90°.".into(),
            schema: json!({ "type": "object", "properties": { "doc": { "type": "integer" }, "degrees": { "type": "integer" }, "pages": { "type": "array" } }, "required": ["doc", "degrees"] }),
        }]
    }
}

#[test]
fn custom_commands_build_and_run() {
    let host = Arc::new(FakeHost::default());
    let lib = temp("custom");
    let (hh, l) = (host.clone(), lib.clone());
    let mut h = harness(move |app| {
        app.tool_host = Some(hh);
        app.custom_commands.library_override = Some(l);
    });
    assert!(h.state_mut().execute("actions.custom_commands"));
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::CustomCommands));
    h.get_by_label("New Command…").click();
    h.run_steps(2);
    {
        let d = h.state_mut().custom_commands.editing.as_mut().unwrap();
        d.label = "Turn right".into();
        d.tool = "page_rotate".into();
        let o = d.options.entry("degrees".into()).or_default();
        o.set = true;
        o.text = "90".into();
    }
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert!(h.state().custom_commands.editing.is_none(), "{:?}", h.state().custom_commands.error);
    assert_eq!(host.commands.lock().unwrap()[0]["params"], json!({ "degrees": 90 }));
    h.get_by_label("Run on this document").click();
    h.run_steps(3);
    let runs = host.runs.lock().unwrap().clone();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["id"], "turn-right");
    assert_eq!(h.state().toast.clone().unwrap().0, "Ran Turn right");

    // Hostile options: not a number, and a value the tool refuses.
    h.get_by_label("New Command…").click();
    h.run_steps(2);
    {
        let d = h.state_mut().custom_commands.editing.as_mut().unwrap();
        d.label = "Bad".into();
        d.tool = "page_rotate".into();
        let o = d.options.entry("degrees".into()).or_default();
        o.set = true;
        o.text = "ninety".into();
    }
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert!(h.state().custom_commands.error.is_some());
    h.state_mut().custom_commands.editing.as_mut().unwrap().options.get_mut("degrees").unwrap().text = "45".into();
    h.run_steps(1);
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert!(h.state().custom_commands.error.as_deref().is_some_and(|e| e.contains("multiple of 90")));
    assert_eq!(host.commands.lock().unwrap().len(), 1);
    // An unknown command id runs nothing.
    h.state_mut().run_custom_command("no-such-command", None);
    assert_eq!(host.runs.lock().unwrap().len(), 1);

    // Without a host (the web app) the dialog says so instead of failing.
    h.state_mut().tool_host = None;
    h.state_mut().open_custom_commands();
    h.run_steps(2);
    h.get_by_label_contains("aren't available");
}

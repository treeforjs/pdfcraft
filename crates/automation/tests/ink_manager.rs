//! `print_ink_manager_list`, `print_ink_manager_set` and `print_ink_manager_apply`, and the
//! separations tools honouring the Ink Manager, end to end through the tool table.

use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

fn call(app: &mut Automation, name: &str, args: Value) -> Value {
    match app.call(name, &args).unwrap().remove(0) {
        Content::Json(v) => v,
        _ => panic!("JSON result"),
    }
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// Gold: a Separation with a CMYK alternate; Duo: DeviceN Magenta + Teal through a PostScript
/// tint transform (Teal at t is Cyan t); Lab: a Separation with a Lab alternate.
const RESOURCES: &str = "<< /ColorSpace << \
    /Gold [/Separation /Gold /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.2 1 0] /N 1 >>] \
    /Duo [/DeviceN [/Magenta /Teal] /DeviceCMYK 6 0 R] \
    /Lab [/Separation /Rubine [/Lab << /WhitePoint [0.9642 1 0.8249] >>] << /FunctionType 2 /Domain [0 1] /C0 [100 0 0] /C1 [50 70 0] /N 1 >>] >> \
    /XObject << /Im 7 0 R >> >>";

/// Page 1 (200 × 100 pt): Gold 50 % at x 0–60, Duo Magenta 25 + Teal 75 at x 70–130, a Gold
/// image at x 140–200 (bottom half), Rubine text at the top right; page 2: Gold only declared.
fn open(name: &str) -> (Automation, u64, std::path::PathBuf) {
    open_with(name, b"/Gold cs 0.5 scn 0 0 60 100 re f /Duo cs 0.25 0.75 scn 70 0 60 100 re f q 60 0 0 50 140 0 cm /Im Do Q BT /Lab cs 1 scn /F1 20 Tf 150 70 Td (Hi) Tj ET")
}

fn open_with(name: &str, page1: &[u8]) -> (Automation, u64, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-inkmgr-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R 8 0 R] /Count 2 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources {RESOURCES} /Contents 4 0 R >>").into_bytes(),
        stream("", page1),
        b"<< >>".to_vec(),
        stream("/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1]", b"{ exch 0 0 }"),
        stream(
            "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace [/Separation /Gold /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.2 1 0] /N 1 >>]",
            b"\xff",
        ),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources {RESOURCES} /Contents 9 0 R >>").into_bytes(),
        stream("", b"0 0 0 1 k 0 0 10 10 re f"),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes());
    std::fs::write(dir.join("inks.pdf"), out).unwrap();
    let mut app = Automation::new().with_root(&dir).unwrap();
    let doc = call(&mut app, "doc_open", json!({"path":"inks.pdf"}))["doc"].as_u64().unwrap();
    (app, doc, dir)
}

fn ink<'a>(list: &'a Value, name: &str) -> Option<&'a Value> {
    list["inks"].as_array().unwrap().iter().find(|i| i["name"] == name)
}

fn percent(sample: &Value, name: &str) -> f64 {
    sample["inks"].as_array().unwrap().iter().find(|i| i["name"] == name).map_or(0.0, |i| i["percent"].as_f64().unwrap())
}

/// The ink percentages at (x, y) from the top-left of page 1.
fn at(app: &mut Automation, doc: u64, x: f64, y: f64) -> Value {
    call(app, "print_separations_sample", json!({"doc":doc,"page":1,"x":x,"y":y,"overprint":false}))
}

#[test]
fn the_list_gives_every_ink_with_pages_object_kinds_and_route() {
    let (mut app, doc, dir) = open("list");
    let out = call(&mut app, "print_ink_manager_list", json!({"doc":doc}));
    let names: Vec<&str> = out["inks"].as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Cyan", "Magenta", "Yellow", "Black", "Gold", "Teal", "Rubine"]);
    let gold = ink(&out, "Gold").unwrap();
    assert_eq!(gold["pages"], json!([1]));
    assert_eq!(gold["declared_pages"], json!([1, 2]));
    assert_eq!(gold["objects"], json!(["vector", "image"]));
    assert_eq!((gold["route"].as_str(), gold["prints_on"].as_str()), (Some("own"), Some("Gold")));
    assert_eq!(ink(&out, "Rubine").unwrap()["objects"], json!(["text"]));
    assert_eq!(ink(&out, "Rubine").unwrap()["preview"], "#808080", "Lab spot inks preview gray unless use_lab");
    assert_eq!(ink(&out, "Black").unwrap()["pages"], json!([2]));
    assert_eq!(out["settings"]["default"], true);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn aliases_and_spot_to_process_move_ink_in_the_separations_preview() {
    let (mut app, doc, dir) = open("preview");
    // Gold prints on Teal's plate.
    let set = call(&mut app, "print_ink_manager_set", json!({"doc":doc,"aliases":{"Gold":"Teal"}}));
    assert_eq!(set["spot_inks"]["Gold"]["prints_on"], "Teal");
    assert_eq!(set["spot_inks"]["Teal"]["route"], "own");
    let s = at(&mut app, doc, 30.0, 50.0);
    assert_eq!(percent(&s, "Teal"), 50.0, "{s}");
    assert!(s["inks"].as_array().unwrap().iter().all(|i| i["name"] != "Gold"), "Gold has no plate: {s}");
    assert_eq!(s["routed"]["Gold"], "Teal");
    let list = call(&mut app, "print_separations_list", json!({"doc":doc}));
    assert!(ink(&list, "Gold").is_none());
    assert_eq!(list["ink_manager"]["aliases"]["Gold"], "Teal");
    // Following the alias: Teal to process takes Gold (by its own alternate) with it.
    call(&mut app, "print_ink_manager_set", json!({"doc":doc,"to_process":["Teal"]}));
    let s = at(&mut app, doc, 30.0, 50.0);
    assert_eq!([percent(&s, "Cyan"), percent(&s, "Magenta"), percent(&s, "Yellow"), percent(&s, "Black")], [0.0, 10.0, 50.0, 0.0], "{s}");
    // DeviceN: Teal (Cyan 75 through the PostScript transform) adds to the Magenta 25 beside it.
    let s = at(&mut app, doc, 100.0, 50.0);
    assert_eq!([percent(&s, "Cyan"), percent(&s, "Magenta")], [75.0, 25.0], "{s}");
    assert_eq!(percent(&s, "Teal"), 0.0);
    // Total area coverage honours it too.
    let tac = call(&mut app, "print_total_area_coverage", json!({"doc":doc,"pages":[1],"overprint":false}));
    // The Gold image at 100 % is M 20 + Y 100.
    assert_eq!(tac["pages"][0]["max"], 120.0, "{tac}");
    // The Ink Manager list keeps the document's own inks, with their routes.
    let mine = call(&mut app, "print_ink_manager_list", json!({"doc":doc}));
    let gold = ink(&mine, "Gold").unwrap();
    assert_eq!((gold["route"].as_str(), gold["aliased_to"].as_str()), (Some("process"), Some("Teal")));
    // Back to its own plate; reset clears everything.
    call(&mut app, "print_ink_manager_set", json!({"doc":doc,"own_plate":["Gold"]}));
    assert_eq!(percent(&at(&mut app, doc, 30.0, 50.0), "Gold"), 50.0);
    let reset = call(&mut app, "print_ink_manager_set", json!({"doc":doc,"reset":true}));
    assert_eq!(reset["settings"]["default"], true);
    // All spots to process; use_lab previews Rubine from its Lab values.
    call(&mut app, "print_ink_manager_set", json!({"doc":doc,"all_to_process":true,"use_lab":true}));
    let s = at(&mut app, doc, 30.0, 50.0);
    assert_eq!(percent(&s, "Yellow"), 50.0, "{s}");
    call(&mut app, "print_ink_manager_set", json!({"doc":doc,"all_to_process":false}));
    let list = call(&mut app, "print_separations_list", json!({"doc":doc}));
    let rubine = ink(&list, "Rubine").unwrap();
    assert_eq!(rubine["preview_exact"], true, "{rubine}");
    assert_ne!(rubine["preview"], "#808080");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn applying_rewrites_the_document_as_one_undo_step() {
    let (mut app, doc, dir) = open("apply");
    call(&mut app, "print_ink_manager_set", json!({"doc":doc,"aliases":{"Rubine":"Gold"}}));
    let dry = call(&mut app, "print_ink_manager_apply", json!({"doc":doc,"dry_run":true}));
    assert_eq!(dry["changed"], true);
    assert_eq!(dry["dirty"], false, "{dry}");
    let out = call(&mut app, "print_ink_manager_apply", json!({"doc":doc}));
    assert_eq!(out["changed"], true);
    assert_eq!(out["renamed"]["colorants"]["Rubine"], 2, "both pages' resources: {out}");
    call(&mut app, "print_ink_manager_set", json!({"doc":doc,"reset":true}));
    let list = call(&mut app, "print_ink_manager_list", json!({"doc":doc}));
    assert!(ink(&list, "Rubine").is_none(), "Rubine is gone from the document");
    assert_eq!(ink(&list, "Gold").unwrap()["objects"], json!(["text", "vector", "image"]));
    // Gold to process, permanently: the document has no Gold left; its CMYK stays.
    call(&mut app, "print_ink_manager_set", json!({"doc":doc,"to_process":["Gold"]}));
    let out = call(&mut app, "print_ink_manager_apply", json!({"doc":doc}));
    assert_eq!(out["changed"], true, "{out}");
    assert!(out["destination"].as_str().unwrap().contains("built-in"), "{out}");
    assert_eq!(out["converted"]["pages_changed"], json!([1]), "{out}");
    // Applying again changes nothing (no undo step).
    let again = call(&mut app, "print_ink_manager_apply", json!({"doc":doc}));
    assert_eq!(again["changed"], false, "{again}");
    call(&mut app, "print_ink_manager_set", json!({"doc":doc,"reset":true}));
    let s = at(&mut app, doc, 30.0, 50.0);
    assert_eq!([percent(&s, "Magenta"), percent(&s, "Yellow"), percent(&s, "Gold")], [10.0, 50.0, 0.0], "{s}");
    // Undo twice: Gold and Rubine are back.
    call(&mut app, "edit_undo", json!({"doc":doc}));
    call(&mut app, "edit_undo", json!({"doc":doc}));
    let list = call(&mut app, "print_ink_manager_list", json!({"doc":doc}));
    assert!(ink(&list, "Rubine").is_some() && ink(&list, "Gold").is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn bad_settings_are_refused_with_what_is_allowed() {
    let (mut app, doc, dir) = open("bad");
    for args in [
        json!({"doc":doc}),
        json!({"doc":doc,"aliases":["Gold"]}),
        json!({"doc":doc,"aliases":{"Gold":7}}),
        json!({"doc":doc,"aliases":{"Unobtainium":"Gold"}}),
        json!({"doc":doc,"aliases":{"Gold":"Unobtainium"}}),
        json!({"doc":doc,"aliases":{"Cyan":"Gold"}}),
        json!({"doc":doc,"aliases":{"Gold":"Gold"}}),
        json!({"doc":doc,"aliases":{"Gold":"Teal","Teal":"Gold"}}),
        json!({"doc":doc,"to_process":["Black"]}),
        json!({"doc":doc,"to_process":"Gold"}),
        json!({"doc":doc,"use_lab":"yes"}),
        json!({"doc":99,"reset":true}),
    ] {
        assert!(app.call("print_ink_manager_set", &args).is_err(), "{args}");
    }
    let err = format!("{:?}", app.call("print_ink_manager_set", &json!({"doc":doc,"to_process":["Nope"]})).unwrap_err());
    assert!(err.contains("Gold") && err.contains("Teal"), "the error lists the spot inks: {err}");
    // Nothing set: nothing to apply.
    assert!(app.call("print_ink_manager_apply", &json!({"doc":doc})).is_err());
    let list = call(&mut app, "print_ink_manager_list", json!({"doc":doc}));
    assert_eq!(list["settings"]["default"], true, "refused calls change nothing");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn hostile_pages_with_inks_moved_do_not_panic() {
    let (mut app, doc, dir) = open_with(
        "hostile",
        b"/Gold cs 1e308 scn 0 0 1e308 -1e308 re f /Duo cs -5 nan scn 0 0 10 10 re f /Lab cs inf scn 0 0 5 5 re f BI /W 99999 /H 99999 /CS /Gold /BPC 8 ID \x01 EI",
    );
    call(&mut app, "print_ink_manager_set", json!({"doc":doc,"all_to_process":true,"use_lab":true,"aliases":{"Teal":"Gold"}}));
    for (tool, args) in [
        ("print_separations_list", json!({"doc":doc})),
        ("print_separations_sample", json!({"doc":doc,"page":1,"x":1,"y":99,"dpi":300})),
        ("print_total_area_coverage", json!({"doc":doc,"dpi":300})),
        ("print_ink_manager_apply", json!({"doc":doc,"dry_run":true})),
    ] {
        call(&mut app, tool, args);
    }
    assert!(app.call("print_separations_render", &json!({"doc":doc,"page":1,"dpi":9})).is_ok());
    std::fs::remove_dir_all(dir).unwrap();
}

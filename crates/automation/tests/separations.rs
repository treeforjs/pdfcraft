//! `print_separations_list`, `print_separations_render`, `print_separations_sample` and the
//! TAC location of `print_total_area_coverage`, end to end through the tool table.

use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

fn call(app: &mut Automation, name: &str, args: Value) -> Value {
    match app.call(name, &args).unwrap().remove(0) {
        Content::Json(v) => v,
        _ => panic!("JSON result"),
    }
}

fn stream(data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

const RESOURCES: &str = "<< /ColorSpace << \
    /Gold [/Separation /Gold /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.2 1 0] /N 1 >>] \
    /Duo [/DeviceN [/Magenta /Teal] /DeviceCMYK << /FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1] >>] >> \
    /ExtGState << /OP << /OP true /op true /OPM 1 >> >> >>";

/// A temp root holding `name`.pdf: one 200 × 100 pt page per entry of `pages` (content, Rotate).
fn open(name: &str, pages: &[(&[u8], u32)]) -> (Automation, u64, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-seps-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let kids: Vec<String> = (0..pages.len()).map(|i| format!("{} 0 R", 3 + 2 * i)).collect();
    let mut objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()).into_bytes(),
    ];
    for (i, (content, rotate)) in pages.iter().enumerate() {
        objs.push(
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Rotate {rotate} /Resources {RESOURCES} /Contents {} 0 R >>", 4 + 2 * i)
                .into_bytes(),
        );
        objs.push(stream(content));
    }
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
    std::fs::write(dir.join("seps.pdf"), out).unwrap();
    let mut app = Automation::new().with_root(&dir).unwrap();
    let doc = call(&mut app, "doc_open", json!({"path":"seps.pdf"}))["doc"].as_u64().unwrap();
    (app, doc, dir)
}

/// Page 1: CMYK 10/20/30/40 on the left, Gold 50 % in the middle, DeviceN Magenta 25 + Teal
/// 75 on the right; page 2: RGB red.
fn sample_doc(name: &str) -> (Automation, u64, std::path::PathBuf) {
    open(
        name,
        &[
            (b"0.1 0.2 0.3 0.4 k 0 0 60 100 re f /Gold cs 0.5 scn 70 0 60 100 re f /Duo cs 0.25 0.75 scn 140 0 60 100 re f", 0),
            (b"1 0 0 rg 0 0 200 100 re f", 0),
        ],
    )
}

/// Decode a PNG: width, height, channels, pixels.
fn decode(data: &[u8]) -> (usize, usize, usize, Vec<u8>) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(data.to_vec())).read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    buf.truncate(info.buffer_size());
    (info.width as usize, info.height as usize, info.color_type.samples(), buf)
}

fn pixel(img: &(usize, usize, usize, Vec<u8>), x: usize, y: usize) -> Vec<u8> {
    let (w, _, c, data) = img;
    data[(y * w + x) * c..(y * w + x + 1) * c].to_vec()
}

fn percent(sample: &Value, ink: &str) -> f64 {
    sample["inks"].as_array().unwrap().iter().find(|i| i["name"] == ink).unwrap()["percent"].as_f64().unwrap()
}

#[test]
fn the_list_names_every_ink_with_its_alternate_and_pages() {
    let (mut app, doc, dir) = sample_doc("list");
    let out = call(&mut app, "print_separations_list", json!({"doc":doc}));
    let names: Vec<&str> = out["inks"].as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Cyan", "Magenta", "Yellow", "Black", "Gold", "Teal"]);
    assert_eq!((out["process_inks"].as_u64(), out["spot_inks"].as_u64()), (Some(4), Some(2)));
    let ink = |n: &str| out["inks"].as_array().unwrap().iter().find(|i| i["name"] == n).unwrap().clone();
    assert_eq!(ink("Cyan")["pages"], json!([1]));
    assert_eq!(ink("Magenta")["pages"], json!([1, 2]));
    let gold = ink("Gold");
    assert_eq!(gold["kind"], "spot");
    assert_eq!(gold["alternate"], "DeviceCMYK");
    assert_eq!(gold["tint_transform"], 2);
    assert_eq!(gold["alternate_value"], json!([0.0, 0.2, 1.0, 0.0]));
    assert_eq!(gold["preview"], "#FFCC00");
    assert_eq!(gold["pages"], json!([1]));
    assert_eq!(gold["declared_pages"], json!([1, 2]));
    let teal = ink("Teal");
    assert_eq!(
        (teal["defined_by"].as_str(), teal["tint_transform"].as_i64(), teal["preview_exact"].as_bool()),
        (Some("DeviceN"), Some(4), Some(false))
    );
    assert_eq!(out["converted_pages"], json!([2]));
    assert!(out["conversion"].as_str().unwrap().contains("no output intent"), "{}", out["conversion"]);
    let second = call(&mut app, "print_separations_list", json!({"doc":doc,"pages":[2]}));
    assert_eq!(second["pages_checked"], json!([2]));
    assert_eq!(second["inks"][4]["used"], false);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn samples_give_each_plate_at_a_point() {
    let (mut app, doc, dir) = sample_doc("sample");
    // Tool coordinates are from the top-left: y 50 is the middle of the 100 pt page.
    let cmyk = call(&mut app, "print_separations_sample", json!({"doc":doc,"page":1,"x":30,"y":50}));
    assert_eq!([percent(&cmyk, "Cyan"), percent(&cmyk, "Magenta"), percent(&cmyk, "Yellow"), percent(&cmyk, "Black")], [10.0, 20.0, 30.0, 40.0]);
    assert_eq!((percent(&cmyk, "Gold"), cmyk["total"].as_f64()), (0.0, Some(100.0)));
    let gold = call(&mut app, "print_separations_sample", json!({"doc":doc,"page":1,"x":100,"y":10}));
    assert_eq!((percent(&gold, "Gold"), gold["total"].as_f64()), (50.0, Some(50.0)));
    assert_eq!(gold["pdf_point"], json!([100.0, 90.0]));
    let duo = call(&mut app, "print_separations_sample", json!({"doc":doc,"page":1,"x":170,"y":90}));
    assert_eq!((percent(&duo, "Magenta"), percent(&duo, "Teal"), duo["total"].as_f64()), (25.0, 75.0, Some(100.0)));
    let red = call(&mut app, "print_separations_sample", json!({"doc":doc,"page":2,"x":100,"y":50}));
    assert_eq!((percent(&red, "Magenta"), percent(&red, "Yellow"), red["total"].as_f64()), (100.0, 100.0, Some(200.0)));
    assert!(red["conversion"].as_str().unwrap().contains("built-in"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn plates_render_inline_or_to_files_in_display_orientation() {
    let (mut app, doc, dir) = sample_doc("render");
    let out = app.call("print_separations_render", &json!({"doc":doc,"page":1,"inks":["Gold","Cyan"],"dpi":36})).unwrap();
    assert_eq!(out.len(), 3);
    let Content::Json(meta) = &out[0] else { panic!("JSON first") };
    assert_eq!((meta["width"].as_u64(), meta["height"].as_u64()), (Some(100), Some(50)));
    assert_eq!(meta["plates"][0]["ink"], "Gold");
    assert_eq!(meta["plates"][0]["max"], 50.0);
    let Content::Png { data, width, height } = &out[1] else { panic!("a PNG plate") };
    assert_eq!((*width, *height), (100, 50));
    let img = decode(data);
    assert_eq!((img.0, img.1, img.2), (100, 50, 1));
    // Gold (x 70–130) at 50 %: mid gray; paper white elsewhere.
    assert_eq!(pixel(&img, 50, 25), [128]);
    assert_eq!(pixel(&img, 10, 25), [255]);
    // To files, with a composite showing only gold.
    let written = call(&mut app, "print_separations_render", json!({"doc":doc,"page":1,"path":"p1.png","dpi":18}));
    let plates = written["plates"].as_array().unwrap();
    assert_eq!(plates.len(), 6);
    for p in plates {
        let path = p["path"].as_str().unwrap();
        assert!(path.ends_with(&format!("p1-{}.png", p["ink"].as_str().unwrap())), "{path}");
        assert!(std::path::Path::new(path).exists());
    }
    let comp = call(&mut app, "print_separations_render", json!({"doc":doc,"page":1,"composite":true,"inks":["Gold"],"path":"gold.png"}));
    assert_eq!(comp["composite"]["shown"], json!(["Gold"]));
    let rgb = decode(&std::fs::read(dir.join("gold.png")).unwrap());
    assert_eq!((rgb.0, rgb.1, rgb.2), (200, 100, 3));
    let gold = pixel(&rgb, 100, 50);
    assert!(gold[0] == 255 && gold[1].abs_diff(230) <= 1 && gold[2].abs_diff(128) <= 1, "{gold:?}");
    assert_eq!(pixel(&rgb, 30, 50), [255, 255, 255], "the hidden CMYK plates don't show");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rotated_pages_render_and_sample_as_displayed() {
    let (mut app, doc, dir) = open("rotated", &[(b"0 0 0 1 k 0 0 20 100 re f", 90)]);
    let out = app.call("print_separations_render", &json!({"doc":doc,"page":1,"inks":["Black"]})).unwrap();
    let Content::Png { data, width, height } = &out[1] else { panic!("a PNG plate") };
    // 200 × 100 turned a quarter clockwise: 100 wide, 200 tall, the left strip now on top.
    assert_eq!((*width, *height), (100, 200));
    let img = decode(data);
    assert_eq!(pixel(&img, 50, 5), [0]);
    assert_eq!(pixel(&img, 50, 150), [255]);
    let s = call(&mut app, "print_separations_sample", json!({"doc":doc,"page":1,"x":50,"y":5}));
    assert_eq!(s["total"], 100.0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn total_area_coverage_reports_where_the_maximum_is() {
    let (mut app, doc, dir) =
        open("tac", &[(b"0.5 0.5 0.5 0.5 k 0 0 200 100 re f 1 1 1 0.5 k 150 60 20 20 re f q /OP gs 0 0 0 1 k 0 0 10 10 re f Q", 0)]);
    let out = call(&mut app, "print_total_area_coverage", json!({"doc":doc,"limit":320}));
    let page = &out["pages"][0];
    assert_eq!(page["max"], 350.0);
    assert_eq!(page["within_limit"], false);
    // User (150.5, 79.5) is (150.5, 20.5) from the top-left.
    assert_eq!(page["pdf_max_at"], json!([150.5, 79.5]));
    assert_eq!(page["max_at"], json!([150.5, 20.5]));
    assert_eq!(out["max"], 350.0);
    assert_eq!(out["overprint"], true);
    // Overprinting black in mode 1 keeps the 50 % CMY below: 250 %; knocked out it is 100 %.
    let s = call(&mut app, "print_separations_sample", json!({"doc":doc,"page":1,"x":5,"y":95}));
    assert_eq!(s["total"], 250.0);
    let s = call(&mut app, "print_separations_sample", json!({"doc":doc,"page":1,"x":5,"y":95,"overprint":false}));
    assert_eq!(s["total"], 100.0);
    let within = call(&mut app, "print_total_area_coverage", json!({"doc":doc,"limit":400}));
    assert_eq!(within["pages"][0]["within_limit"], true);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn bad_arguments_are_refused() {
    let (mut app, doc, dir) = sample_doc("bad");
    for (tool, args) in [
        ("print_separations_list", json!({"doc":doc,"pages":[3]})),
        ("print_separations_list", json!({"doc":doc,"pages":[]})),
        ("print_separations_list", json!({"doc":99})),
        ("print_separations_render", json!({"doc":doc})),
        ("print_separations_render", json!({"doc":doc,"page":0})),
        ("print_separations_render", json!({"doc":doc,"page":3})),
        ("print_separations_render", json!({"doc":doc,"page":1,"dpi":1000})),
        ("print_separations_render", json!({"doc":doc,"page":1,"dpi":"72"})),
        ("print_separations_render", json!({"doc":doc,"page":1,"inks":[]})),
        ("print_separations_render", json!({"doc":doc,"page":1,"inks":["Unobtainium"]})),
        ("print_separations_render", json!({"doc":doc,"page":1,"inks":[7]})),
        ("print_separations_render", json!({"doc":doc,"page":1,"path":""})),
        ("print_separations_render", json!({"doc":doc,"page":1,"path":"../escape"})),
        ("print_separations_sample", json!({"doc":doc,"page":1,"x":10})),
        ("print_separations_sample", json!({"doc":doc,"page":1,"x":-1,"y":10})),
        ("print_separations_sample", json!({"doc":doc,"page":1,"x":500,"y":10})),
        ("print_separations_sample", json!({"doc":doc,"page":1,"x":10,"y":10,"dpi":2})),
        ("print_total_area_coverage", json!({"doc":doc,"overprint":"yes"})),
    ] {
        let err = app.call(tool, &args);
        assert!(err.is_err(), "{tool} {args}");
    }
    let err = format!("{:?}", app.call("print_separations_render", &json!({"doc":doc,"page":1,"inks":["Unobtainium"]})).unwrap_err());
    assert!(err.contains("Gold"), "the error lists the page's inks: {err}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn hostile_pages_do_not_panic() {
    let (mut app, doc, dir) = open(
        "hostile",
        &[(b"/Gold cs 1e308 scn 0 0 1e308 -1e308 re f /Duo cs -5 nan scn 0 0 10 10 re f /Nope cs 1 scn q q q /OP gs Q 1 1 1 1 k 0 0 0 0 re f* BI /W 99999 /H 99999 /CS /Gold /BPC 8 ID \x01 EI", 0)],
    );
    for (tool, args) in [
        ("print_separations_list", json!({"doc":doc})),
        ("print_separations_sample", json!({"doc":doc,"page":1,"x":0,"y":0,"dpi":300})),
        ("print_total_area_coverage", json!({"doc":doc,"dpi":300})),
    ] {
        call(&mut app, tool, args);
    }
    assert!(app.call("print_separations_render", &json!({"doc":doc,"page":1,"dpi":9})).is_ok());
    std::fs::remove_dir_all(dir).unwrap();
}

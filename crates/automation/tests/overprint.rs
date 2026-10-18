//! Overprint Preview end to end through the tool table: `page_render` / `render_preview` with
//! `overprint_preview`, the `print_overprint_preview` preference and `print_overprint_objects`.

use pdfcraft_automation::{Automation, Content, ToolError};
use pdfcraft_render::device_cmyk_to_rgb;
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
    /Gold [/Separation /Gold /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.2 1 0] /N 1 >>] >> \
    /ExtGState << /OP << /OP true /op true /OPM 1 >> /OP0 << /OP true /op true /OPM 0 >> >> >>";

/// Cyan over the left 140 pt, yellow overprinting the right 140 pt (overlap x 60–140).
const CYAN_YELLOW: &[u8] = b"1 0 0 0 k 0 0 140 100 re f /OP gs 0 0 1 0 k 60 0 140 100 re f";

/// A temp root holding a one-page 200 × 100 pt document with `content`; `info` goes into its
/// information dictionary.
fn open(name: &str, content: &[u8], info: &str) -> (Automation, u64) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-overprint-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources {RESOURCES} /Contents 4 0 R >>").into_bytes(),
        stream(content),
        format!("<< /Producer (test) {info} >>").into_bytes(),
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
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R /Info 5 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes());
    std::fs::write(dir.join("op.pdf"), out).unwrap();
    let mut app = Automation::new().with_root(&dir).unwrap();
    let doc = call(&mut app, "doc_open", json!({"path":"op.pdf"}))["doc"].as_u64().unwrap();
    (app, doc)
}

/// Render with `tool` and return the RGB at (`x`, `y`) points from the top-left.
fn pixel_with(app: &mut Automation, tool: &str, args: Value, x: f64, y: f64) -> [u8; 3] {
    let Content::Png { data, width, .. } = app.call(tool, &args).unwrap().remove(0) else { panic!("PNG result") };
    let mut reader = png::Decoder::new(std::io::Cursor::new(data)).read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    let channels = info.color_type.samples();
    let scale = f64::from(width) / 200.0;
    let (px, py) = ((x * scale) as usize, (y * scale) as usize);
    let i = (py * width as usize + px) * channels;
    [buf[i], buf[i + 1], buf[i + 2]]
}

fn render(app: &mut Automation, doc: u64, mode: Option<&str>, x: f64, y: f64) -> [u8; 3] {
    let mut args = json!({"doc":doc,"page":1,"dpi":72});
    if let Some(m) = mode {
        args["overprint_preview"] = json!(m);
    }
    pixel_with(app, "page_render", args, x, y)
}

fn cmyk(c: [f64; 4]) -> [u8; 3] {
    let v = device_cmyk_to_rgb(&c.map(|v| (v * 255.0).round() as u8)).unwrap();
    [v[0], v[1], v[2]]
}

fn close(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 3)
}

#[test]
fn page_render_simulates_overprint_as_the_mode_says() {
    let (mut app, doc) = open("modes", CYAN_YELLOW, "");
    let (yellow, green, cyan) = (cmyk([0.0, 0.0, 1.0, 0.0]), cmyk([1.0, 0.0, 1.0, 0.0]), cmyk([1.0, 0.0, 0.0, 0.0]));
    // Not a PDF/X file: the default preference (Only For PDF/X Files) knocks out.
    assert!(close(render(&mut app, doc, None, 100.0, 50.0), yellow));
    assert!(close(render(&mut app, doc, Some("never"), 100.0, 50.0), yellow));
    assert!(close(render(&mut app, doc, Some("pdfx"), 100.0, 50.0), yellow));
    assert!(close(render(&mut app, doc, Some("always"), 100.0, 50.0), green));
    assert!(close(render(&mut app, doc, Some("automatic"), 100.0, 50.0), green));
    // Outside the overlap nothing changes.
    assert!(close(render(&mut app, doc, Some("always"), 20.0, 50.0), cyan));
    assert!(close(render(&mut app, doc, Some("always"), 180.0, 50.0), yellow));
    // The session preference becomes the default of both render tools.
    let pref = call(&mut app, "print_overprint_preview", json!({"mode":"always","doc":doc}));
    assert_eq!((pref["mode"].as_str(), pref["pdfx"].as_bool(), pref["applies"].as_bool()), (Some("always"), Some(false), Some(true)));
    assert_eq!(pref["modes"].as_array().unwrap().len(), 4);
    assert!(close(render(&mut app, doc, None, 100.0, 50.0), green));
    assert!(close(pixel_with(&mut app, "render_preview", json!({"doc":doc,"max_side":200}), 100.0, 50.0), green));
    assert!(close(pixel_with(&mut app, "render_preview", json!({"doc":doc,"max_side":200,"overprint_preview":"never"}), 100.0, 50.0), yellow));
    // A higher resolution than the ink grid's maximum is simulated too.
    let hi = pixel_with(&mut app, "page_render", json!({"doc":doc,"page":1,"dpi":600}), 100.0, 50.0);
    assert!(close(hi, green), "{hi:?}");
    let read = call(&mut app, "print_overprint_preview", json!({}));
    assert_eq!(read["mode"], "always");
    assert!(read.get("pdfx").is_none());
}

#[test]
fn pdfx_files_show_overprint_by_default() {
    let (mut app, doc) = open("pdfx", CYAN_YELLOW, "/GTS_PDFXVersion (PDF/X-4)");
    let pref = call(&mut app, "print_overprint_preview", json!({"doc":doc}));
    assert_eq!((pref["mode"].as_str(), pref["pdfx"].as_bool(), pref["applies"].as_bool()), (Some("pdfx"), Some(true), Some(true)));
    assert!(close(render(&mut app, doc, None, 100.0, 50.0), cmyk([1.0, 0.0, 1.0, 0.0])));
    call(&mut app, "print_overprint_preview", json!({"mode":"never"}));
    assert!(close(render(&mut app, doc, None, 100.0, 50.0), cmyk([0.0, 0.0, 1.0, 0.0])));
}

#[test]
fn white_with_opm_1_disappears_and_spot_inks_multiply() {
    let (mut app, doc) = open("white", b"1 0 0 0 k 0 0 200 100 re f /OP gs 0 0 0 0 k 20 0 60 100 re f /OP0 gs /Gold cs 1 scn 120 0 60 100 re f", "");
    let cyan = cmyk([1.0, 0.0, 0.0, 0.0]);
    assert!(close(render(&mut app, doc, Some("never"), 50.0, 50.0), [255, 255, 255]));
    assert!(close(render(&mut app, doc, Some("always"), 50.0, 50.0), cyan));
    let gold = cmyk([0.0, 0.2, 1.0, 0.0]);
    let want = [0, 1, 2].map(|k| (f32::from(cyan[k]) * f32::from(gold[k]) / 255.0).round() as u8);
    let got = render(&mut app, doc, Some("always"), 150.0, 50.0);
    assert!(close(got, want), "{got:?} vs {want:?}");
}

#[test]
fn print_overprint_objects_lists_what_overprints_and_whether_it_shows() {
    let (mut app, doc) = open("objects", b"1 0 0 0 k 0 0 100 100 re f /OP gs 0 0 1 0 k 50 0 40 100 re f 0 1 0 0 k 150 0 40 100 re f", "");
    let out = call(&mut app, "print_overprint_objects", json!({"doc":doc,"page":1}));
    let objects = out["objects"].as_array().unwrap();
    assert_eq!(objects.len(), 2, "{out}");
    assert_eq!(objects[0]["kind"], "overprint-paint");
    assert_eq!(objects[0]["paint"], "fill");
    assert_eq!(objects[0]["colour"]["space"], "DeviceCMYK");
    assert_eq!(objects[0]["overprint_mode"], 1);
    assert_eq!(objects[0]["rect"], json!([50.0, 0.0, 90.0, 100.0]));
    assert_eq!((objects[0]["changes_appearance"].as_bool(), objects[1]["changes_appearance"].as_bool()), (Some(true), Some(false)));
    assert_eq!(out["visible_objects"], 1);
    assert_eq!(out["states"].as_array().unwrap().len(), 1);
    assert_eq!(out["overprint_paints"], 2);
    let changed = out["changed_percent"].as_f64().unwrap();
    assert!((changed - 20.0).abs() < 2.0, "{changed}");
    assert_eq!((out["preference"].as_str(), out["preview_applies"].as_bool()), (Some("pdfx"), Some(false)));
    assert_eq!(out["complete"], true);
    let one = call(&mut app, "print_overprint_objects", json!({"doc":doc,"page":1,"max_objects":1,"dpi":36}));
    assert_eq!((one["object_count"].as_u64(), one["objects_truncated"].as_bool()), (Some(1), Some(true)));
    // A page without overprint has nothing to list.
    let (mut app, doc) = open("objects-none", b"1 0 0 0 k 0 0 100 100 re f", "");
    let none = call(&mut app, "print_overprint_objects", json!({"doc":doc,"page":1}));
    assert_eq!((none["object_count"].as_u64(), none["overprint_paints"].as_u64(), none["changed_percent"].as_f64()), (Some(0), Some(0), Some(0.0)));
}

#[test]
fn bad_arguments_are_refused() {
    let (mut app, doc) = open("bad", CYAN_YELLOW, "");
    for (tool, args) in [
        ("print_overprint_preview", json!({"mode":"sometimes"})),
        ("print_overprint_preview", json!({"mode":7})),
        ("page_render", json!({"doc":doc,"page":1,"overprint_preview":"maybe"})),
        ("render_preview", json!({"doc":doc,"overprint_preview":""})),
        ("print_overprint_objects", json!({"doc":doc,"page":1,"dpi":2})),
        ("print_overprint_objects", json!({"doc":doc,"page":1,"max_objects":0})),
        ("print_overprint_objects", json!({"doc":doc,"page":9})),
        ("print_overprint_objects", json!({"doc":doc})),
    ] {
        assert!(matches!(app.call(tool, &args), Err(ToolError::InvalidArgs(_))), "{tool} {args}");
    }
    // A refused mode leaves the preference alone.
    assert_eq!(call(&mut app, "print_overprint_preview", json!({}))["mode"], "pdfx");
}

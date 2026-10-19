//! `print_output_preview_get`, `print_output_preview_set` and `print_output_preview_render`,
//! end to end through the tool table: pixels and coverage numbers on synthetic PDFs.

use pdfcraft_automation::{Automation, Content};
use pdfcraft_engine::output_preview::synthetic_cmyk_profile;
use serde_json::{Value, json};

fn call(app: &mut Automation, name: &str, args: Value) -> Value {
    match app.call(name, &args).unwrap().remove(0) {
        Content::Json(v) => v,
        _ => panic!("JSON result"),
    }
}

fn err(app: &mut Automation, name: &str, args: Value) -> String {
    match app.call(name, &args) {
        Err(e) => format!("{e:?}"),
        Ok(_) => panic!("{name} {args} should fail"),
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
    /ExtGState << /OP << /OP true /op true /OPM 1 >> >> >>";

/// Cyan (x 0–40), RGB red (50–90), Gold (100–140) as full-height bars and 400% black in the
/// bottom right quarter (150–200 × 0–50).
const BARS: &[u8] = b"1 0 0 0 k 0 0 40 100 re f 1 0 0 rg 50 0 40 100 re f /Gold cs 1 scn 100 0 40 100 re f 1 1 1 1 k 150 0 50 50 re f";

/// A temp root holding a document of 200 × 100 pt pages (content, Rotate), with an output
/// intent holding `profile` when given.
fn open(name: &str, pages: &[(&[u8], u32)], profile: Option<&[u8]>) -> (Automation, u64, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-outprev-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let kids: Vec<String> = (0..pages.len()).map(|i| format!("{} 0 R", 3 + 2 * i)).collect();
    let profile_obj = 3 + 2 * pages.len();
    let intent = if profile.is_some() {
        format!(
            "/OutputIntents [<< /Type /OutputIntent /S /GTS_PDFX /OutputConditionIdentifier (Test press) /DestOutputProfile {profile_obj} 0 R >>]"
        )
    } else {
        String::new()
    };
    let mut objs: Vec<Vec<u8>> = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {intent} >>").into_bytes(),
        format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()).into_bytes(),
    ];
    for (i, (content, rotate)) in pages.iter().enumerate() {
        objs.push(
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Rotate {rotate} /Resources {RESOURCES} /Contents {} 0 R >>", 4 + 2 * i)
                .into_bytes(),
        );
        objs.push(stream(content));
    }
    if let Some(p) = profile {
        let mut s = format!("<< /N 4 /Length {} >>\nstream\n", p.len()).into_bytes();
        s.extend_from_slice(p);
        s.extend_from_slice(b"\nendstream");
        objs.push(s);
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
    std::fs::write(dir.join("doc.pdf"), out).unwrap();
    let mut app = Automation::new().with_root(&dir).unwrap();
    let doc = call(&mut app, "doc_open", json!({"path":"doc.pdf"}))["doc"].as_u64().unwrap();
    (app, doc, dir)
}

/// Decode a PNG: width, height, channels, pixels.
fn decode(data: &[u8]) -> (usize, usize, usize, Vec<u8>) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(data.to_vec())).read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    buf.truncate(info.buffer_size());
    (info.width as usize, info.height as usize, info.color_type.samples(), buf)
}

type Image = (usize, usize, usize, Vec<u8>);

fn pixel(img: &Image, x: usize, y: usize) -> [u8; 3] {
    let (w, _, c, data) = img;
    let at = (y * w + x) * c;
    [data[at], data[at + 1], data[at + 2]]
}

/// Render: the JSON and the decoded image.
fn render(app: &mut Automation, args: Value) -> (Value, Image) {
    let out = app.call("print_output_preview_render", &args).unwrap();
    let Content::Json(v) = &out[0] else { panic!("JSON first") };
    let Some(Content::Png { data, width, height }) = out.get(1) else { panic!("an inline PNG") };
    let img = decode(data);
    assert_eq!((img.0, img.1, img.2), (*width as usize, *height as usize, 3));
    (v.clone(), img)
}

fn ink<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["inks"].as_array().unwrap().iter().find(|i| i["name"] == name).unwrap_or_else(|| panic!("no {name} in {}", v["inks"]))
}

const WHITE: [u8; 3] = [255, 255, 255];

#[test]
fn settings_round_trip_and_are_checked() {
    let (mut app, doc, dir) = open("settings", &[(BARS, 0)], None);
    let get = call(&mut app, "print_output_preview_get", json!({"doc":doc}));
    let s = &get["settings"];
    assert_eq!(s["simulation_profile"]["id"], "output_intent");
    assert_eq!(
        (s["simulate_overprint"].as_bool(), s["simulate_paper"].as_bool(), s["simulate_black"].as_bool()),
        (Some(true), Some(false), Some(false))
    );
    assert_eq!(s["show"], "all");
    assert_eq!(s["coverage_limit"], 300.0);
    assert_eq!(s["warning_color"], "#00FF00");
    // No output intent: the colour settings' working CMYK space is simulated.
    let sim = &get["simulation"];
    assert_eq!((sim["source"].as_str(), sim["used"].as_str(), sim["profile_used"].as_bool()), (Some("output_intent"), Some("working"), Some(true)));
    assert!(sim["name"].as_str().unwrap().starts_with("cgats001"), "{sim}");
    assert!(sim["problem"].as_str().unwrap().contains("no output intent"), "{sim}");
    assert_eq!(get["show_options"].as_array().unwrap().len(), 11);
    let names: Vec<&str> = get["inks"].as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Cyan", "Magenta", "Yellow", "Black", "Gold"]);
    assert!(get["inks"].as_array().unwrap().iter().all(|i| i["shown"] == true));

    let set = call(
        &mut app,
        "print_output_preview_set",
        json!({"doc":doc, "show":"Spot Color", "hide_inks":["Cyan","Gold"], "coverage_warning":true, "coverage_limit":250, "warning_color":"#ff00ff", "simulate_overprint":false}),
    );
    let s = &set["settings"];
    assert_eq!(s["show"], "spot");
    assert_eq!(s["hidden_inks"], json!(["Cyan", "Gold"]));
    assert_eq!((s["coverage_warning"].as_bool(), s["coverage_limit"].as_f64()), (Some(true), Some(250.0)));
    assert_eq!(s["warning_color"], "#FF00FF");
    assert_eq!(s["simulate_overprint"], false);
    assert_eq!(ink(&set, "Cyan")["shown"], false);
    assert_eq!(ink(&set, "Magenta")["shown"], true);
    let set = call(&mut app, "print_output_preview_set", json!({"doc":doc, "show_inks":["Gold"]}));
    assert_eq!(set["settings"]["hidden_inks"], json!(["Cyan"]));
    let set = call(&mut app, "print_output_preview_set", json!({"doc":doc, "simulate_paper":true}));
    assert_eq!((set["settings"]["simulate_paper"].as_bool(), set["settings"]["simulate_black"].as_bool()), (Some(true), Some(true)));
    assert!(err(&mut app, "print_output_preview_set", json!({"doc":doc, "simulate_black":false})).contains("simulate_paper"));
    for (args, says) in [
        (json!({"doc":doc, "hide_inks":["Silver"]}), "no ink"),
        (json!({"doc":doc, "show":"smooth_shades"}), "unknown show"),
        (json!({"doc":doc, "coverage_limit":0.5}), "coverage_limit"),
        (json!({"doc":doc, "warning_color":"green"}), "#RRGGBB"),
        (json!({"doc":doc, "simulation_profile":"user:missing"}), "CMYK profiles are"),
        (json!({"doc":doc, "simulation_profile":"srgb"}), "not a CMYK"),
        (json!({"doc":doc, "simulation_profile":""}), "empty"),
        (json!({"doc":doc, "profile":"x.icc"}), ""),
    ] {
        let e = err(&mut app, "print_output_preview_set", args.clone());
        assert!(e.contains(says), "{args}: {e}");
    }
    // Nothing failed half-way: the settings are as last set.
    let get = call(&mut app, "print_output_preview_get", json!({"doc":doc}));
    assert_eq!(get["settings"]["hidden_inks"], json!(["Cyan"]));
    let reset = call(&mut app, "print_output_preview_set", json!({"doc":doc, "reset":true}));
    assert_eq!(reset["settings"]["show"], "all");
    assert_eq!(reset["settings"]["hidden_inks"], json!([]));
    assert_eq!(reset["settings"]["simulate_paper"], false);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn show_filters_and_separations_change_the_pixels() {
    let (mut app, doc, dir) = open("pixels", &[(BARS, 0)], None);
    let (v, img) = render(&mut app, json!({"doc":doc, "page":1}));
    assert_eq!((v["width"].as_u64(), v["height"].as_u64(), v["dpi"].as_f64()), (Some(200), Some(100), Some(72.0)));
    let cyan = pixel(&img, 20, 50);
    assert!(cyan[0] < 80 && cyan[1] > 120 && cyan[2] > 180, "cyan bar {cyan:?}");
    let red = pixel(&img, 70, 50);
    assert!(red[0] > 180 && red[1] < 80 && red[2] < 80, "red bar {red:?}");
    let gold = pixel(&img, 120, 50);
    assert!(gold[0] > 200 && gold[1] > 150 && gold[2] < 80, "gold bar {gold:?}");
    assert!(pixel(&img, 175, 75).iter().all(|c| *c < 40), "rich black {:?}", pixel(&img, 175, 75));
    assert_eq!(pixel(&img, 175, 25), WHITE);
    assert_eq!(pixel(&img, 45, 50), WHITE);
    assert_eq!(v["hidden_objects"], 0);
    assert_eq!(v["simulated"], json!({"overprint": true, "paper": false, "black_ink": false}));
    assert!(v["simulation"]["display"].as_str().unwrap().contains("working CMYK"), "{}", v["simulation"]);
    // Per-ink area coverage: cyan is the 40 pt bar and the black square (2000 + 4000 of 20000).
    let c = ink(&v, "Cyan");
    assert_eq!((c["max"].as_f64(), c["inked_percent"].as_f64(), c["mean_percent"].as_f64()), (Some(100.0), Some(32.5), Some(32.5)));
    assert_eq!(ink(&v, "Gold")["inked_percent"], 20.0);
    assert_eq!(ink(&v, "Black")["inked_percent"], 12.5);
    // RGB red separates through the built-in conversion to 0/100/100/0.
    assert_eq!(ink(&v, "Magenta")["inked_percent"], 32.5);
    assert!(v["converted_paints"].as_u64().unwrap() >= 1);

    // Show: spot colour only.
    call(&mut app, "print_output_preview_set", json!({"doc":doc, "show":"spot"}));
    let (v, img) = render(&mut app, json!({"doc":doc, "page":1}));
    assert_eq!(pixel(&img, 20, 50), WHITE);
    assert_eq!(pixel(&img, 70, 50), WHITE);
    assert_eq!(pixel(&img, 120, 50), gold);
    assert_eq!(pixel(&img, 175, 75), WHITE);
    assert_eq!(v["hidden_objects"], 3);
    assert_eq!(ink(&v, "Cyan")["max"], 0.0);
    assert_eq!(v["coverage"]["max"], 100.0, "coverage is of the shown objects");
    // Show: RGB only; then DeviceCMYK only.
    call(&mut app, "print_output_preview_set", json!({"doc":doc, "show":"rgb"}));
    let (_, img) = render(&mut app, json!({"doc":doc, "page":1}));
    assert_eq!(pixel(&img, 70, 50), red);
    assert_eq!(pixel(&img, 20, 50), WHITE);
    call(&mut app, "print_output_preview_set", json!({"doc":doc, "show":"device_cmyk"}));
    let (_, img) = render(&mut app, json!({"doc":doc, "page":1}));
    assert_eq!(pixel(&img, 20, 50), cyan);
    assert_eq!(pixel(&img, 70, 50), WHITE);
    assert_eq!(pixel(&img, 120, 50), WHITE);
    // Separations: everything shown but Cyan and Gold switched off.
    call(&mut app, "print_output_preview_set", json!({"doc":doc, "show":"all", "hide_inks":["Cyan","Gold"]}));
    let (v, img) = render(&mut app, json!({"doc":doc, "page":1}));
    assert_eq!(pixel(&img, 20, 50), WHITE);
    assert_eq!(pixel(&img, 120, 50), WHITE);
    assert_eq!(pixel(&img, 70, 50), red);
    let black_without_cyan = pixel(&img, 175, 75);
    assert!(black_without_cyan.iter().all(|c| *c < 60), "M+Y+K is still dark {black_without_cyan:?}");
    assert_eq!(ink(&v, "Cyan")["shown"], false);
    assert_eq!(v["coverage"]["max"], 400.0, "hidden separations still count for coverage");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn total_area_coverage_is_measured_flagged_and_read_at_a_point() {
    let (mut app, doc, dir) = open("tac", &[(BARS, 0)], None);
    let (v, img) = render(&mut app, json!({"doc":doc, "page":1, "x":175, "y":75}));
    let c = &v["coverage"];
    assert_eq!(c["max"], 400.0);
    let at = c["max_at"].as_array().unwrap();
    let (x, y) = (at[0].as_f64().unwrap(), at[1].as_f64().unwrap());
    assert!((150.0..200.0).contains(&x) && (50.0..100.0).contains(&y), "{at:?} in view coordinates");
    assert_eq!((c["limit"].as_f64(), c["warning"].as_bool(), c["over_limit"].as_bool()), (Some(300.0), Some(false), Some(true)));
    assert_eq!((c["over_cells"].as_u64(), c["over_percent"].as_f64(), c["over_area"].as_f64()), (Some(2500), Some(12.5), Some(2500.0)));
    assert_eq!(c["warned_pixels"], 0);
    let s = &v["sample"];
    assert_eq!(s["total"], 400.0);
    assert!(s["inks"].as_array().unwrap().iter().take(4).all(|i| i["percent"] == 100.0));
    assert_ne!(pixel(&img, 175, 75), [255, 0, 255]);

    call(&mut app, "print_output_preview_set", json!({"doc":doc, "coverage_warning":true, "coverage_limit":150, "warning_color":"#FF00FF"}));
    let (v, img) = render(&mut app, json!({"doc":doc, "page":1, "x":70, "y":10}));
    let c = &v["coverage"];
    assert_eq!((c["limit"].as_f64(), c["warning"].as_bool()), (Some(150.0), Some(true)));
    // The 200% red bar (4000 cells) and the 400% square (2500).
    assert_eq!((c["over_cells"].as_u64(), c["over_percent"].as_f64(), c["warned_pixels"].as_u64()), (Some(6500), Some(32.5), Some(6500)));
    assert_eq!(pixel(&img, 70, 50), [255, 0, 255]);
    assert_eq!(pixel(&img, 175, 75), [255, 0, 255]);
    assert_ne!(pixel(&img, 20, 50), [255, 0, 255], "100% cyan is under the limit");
    assert_eq!(v["sample"]["total"], 200.0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_output_intent_simulates_paper_colour_and_black_ink() {
    let profile = synthetic_cmyk_profile([0.90, 0.94, 0.66], 20.0);
    let (mut app, doc, dir) = open("intent", &[(b"0 0 0 1 k 0 0 40 100 re f 0.6 0 0 0 k 100 0 40 100 re f", 0)], Some(&profile));
    let get = call(&mut app, "print_output_preview_get", json!({"doc":doc}));
    assert_eq!(get["is_pdfx"], true);
    let sim = &get["simulation"];
    assert_eq!((sim["source"].as_str(), sim["profile_used"].as_bool()), (Some("output_intent"), Some(true)));
    assert_eq!(sim["name"], "Test press");
    assert!(sim["problem"].is_null(), "{sim}");
    assert!(sim["conversion"].as_str().unwrap().contains("CMYK profile"), "{sim}");

    let (v, normal) = render(&mut app, json!({"doc":doc, "page":1}));
    assert_eq!(v["simulated"], json!({"overprint": true, "paper": false, "black_ink": false}));
    assert!(pixel(&normal, 70, 50).iter().all(|c| *c >= 253), "the paper is white {:?}", pixel(&normal, 70, 50));
    assert!(pixel(&normal, 20, 50).iter().all(|c| *c <= 3), "black is black {:?}", pixel(&normal, 20, 50));

    call(&mut app, "print_output_preview_set", json!({"doc":doc, "simulate_black":true}));
    let (v, black) = render(&mut app, json!({"doc":doc, "page":1}));
    assert_eq!(v["simulated"]["black_ink"], true);
    let k = pixel(&black, 20, 50);
    assert!(k.iter().all(|c| (40..=56).contains(c)), "black ink at L* 20 is dark gray {k:?}");
    assert!(pixel(&black, 70, 50).iter().all(|c| *c >= 253));

    call(&mut app, "print_output_preview_set", json!({"doc":doc, "simulate_paper":true}));
    let (v, paper) = render(&mut app, json!({"doc":doc, "page":1}));
    assert_eq!(v["simulated"], json!({"overprint": true, "paper": true, "black_ink": true}));
    let blank = pixel(&paper, 70, 50);
    assert!(blank[0] >= 250 && blank[2] + 20 < blank[0], "the paper is yellowish {blank:?}");
    let (cyan_rel, cyan_abs) = (pixel(&black, 120, 50), pixel(&paper, 120, 50));
    assert!(cyan_abs[2] + 10 < cyan_rel[2], "the paper tints the ink: {cyan_rel:?} → {cyan_abs:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn simulation_profiles_come_from_the_colour_settings() {
    let (mut app, doc, dir) = open("chosen", &[(b"1 0 0 rg 0 0 100 100 re f 0 0 0 1 k 100 0 100 100 re f", 0)], None);
    let (v, working) = render(&mut app, json!({"doc":doc, "page":1}));
    assert_eq!(v["simulation"]["used"], "working");
    assert_eq!(ink(&v, "Magenta")["max"], 100.0, "built-in conversion: red is 0/100/100/0");

    // Untagged colour unmanaged in the colour settings: no profile, nothing to simulate.
    call(&mut app, "color_settings_set", json!({"untagged":"unmanaged"}));
    call(&mut app, "print_output_preview_set", json!({"doc":doc, "simulate_paper":true}));
    let (v, _) = render(&mut app, json!({"doc":doc, "page":1}));
    assert_eq!((v["simulation"]["used"].as_str(), v["simulation"]["profile_used"].as_bool()), (Some("none"), Some(false)));
    assert_eq!(v["simulated"]["paper"], false, "no profile, no paper simulation");
    assert!(v["simulation"]["problem"].as_str().unwrap().contains("unmanaged") || v["simulation"]["problem"].as_str().unwrap().contains("off"));
    call(&mut app, "color_settings_set", json!({"reset":true}));

    // A profile loaded into the colour settings, chosen by id.
    let profile = synthetic_cmyk_profile([0.92, 0.96, 0.70], 25.0);
    std::fs::write(dir.join("press.icc"), &profile).unwrap();
    call(&mut app, "color_profile_load", json!({"path":"press.icc", "name":"press"}));
    let set = call(&mut app, "print_output_preview_set", json!({"doc":doc, "simulation_profile":"user:press"}));
    assert_eq!(set["settings"]["simulation_profile"]["id"], "user:press");
    let sim = &set["simulation"];
    assert_eq!((sim["source"].as_str(), sim["used"].as_str(), sim["profile_used"].as_bool()), (Some("profile"), Some("profile"), Some(true)));
    assert!(sim["conversion"].as_str().unwrap().contains("user:press"), "{sim}");
    let (v, img) = render(&mut app, json!({"doc":doc, "page":1}));
    assert_eq!(v["simulated"]["paper"], true);
    assert!(v["conversion"].as_str().unwrap().contains("user:press"));
    // Red separates through the profile's BToA table now, not the built-in conversion.
    let m = ink(&v, "Magenta")["max"].as_f64().unwrap();
    let y = ink(&v, "Yellow")["max"].as_f64().unwrap();
    assert!(m > 50.0 && y > 50.0, "red is magenta and yellow ({m}, {y})");
    assert_ne!(pixel(&img, 50, 50), pixel(&working, 50, 50));
    let k = pixel(&img, 150, 50);
    assert!(k.iter().all(|c| *c > 40), "paper simulation shows black ink as gray {k:?}");
    let blank = call(&mut app, "print_output_preview_set", json!({"doc":doc, "simulation_profile":"working"}));
    assert_eq!(blank["simulation"]["used"], "working");
    // The working CMYK space set to the loaded profile is what output_intent falls back to.
    call(&mut app, "color_settings_set", json!({"cmyk":"user:press"}));
    let set = call(&mut app, "print_output_preview_set", json!({"doc":doc, "simulation_profile":"output_intent"}));
    assert!(set["simulation"]["name"].as_str().unwrap().starts_with("user:press"), "{}", set["simulation"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rotated_pages_render_and_sample_as_displayed_and_files_are_written() {
    let (mut app, doc, dir) = open("rotated", &[(BARS, 90)], None);
    let (v, img) = render(&mut app, json!({"doc":doc, "page":1}));
    assert_eq!((v["width"].as_u64(), v["height"].as_u64(), v["rotation"].as_u64()), (Some(100), Some(200), Some(90)));
    assert_eq!((img.0, img.1), (100, 200));
    // The separations readout and the preview agree on where the cyan bar is displayed.
    let s = call(&mut app, "print_separations_sample", json!({"doc":doc, "page":1, "x":50, "y":20}));
    let cyan = s["inks"].as_array().unwrap().iter().find(|i| i["name"] == "Cyan").unwrap()["percent"].as_f64().unwrap();
    // Turned clockwise, the page's left edge (the cyan bar) is at the top.
    assert_eq!(cyan, 100.0);
    let px = pixel(&img, 50, 20);
    assert!(px[0] < 80 && px[2] > 180, "{px:?}");
    assert_eq!(pixel(&img, 50, 45), WHITE);
    let (v2, _) = render(&mut app, json!({"doc":doc, "page":1, "x":50, "y":20}));
    assert_eq!(v2["sample"]["total"], s["total"]);
    let max_at = v["coverage"]["max_at"].as_array().unwrap();
    let (x, y) = (max_at[0].as_f64().unwrap(), max_at[1].as_f64().unwrap());
    assert!((0.0..=100.0).contains(&x) && (0.0..=200.0).contains(&y), "{max_at:?}");
    let at = call(&mut app, "print_separations_sample", json!({"doc":doc, "page":1, "x":x, "y":y}));
    assert_eq!(at["total"], 400.0, "max_at is in display coordinates");
    // To a file.
    let out = app.call("print_output_preview_render", &json!({"doc":doc, "page":1, "path":"preview.png", "dpi":36})).unwrap();
    assert_eq!(out.len(), 1, "no inline image with path");
    let Content::Json(v) = &out[0] else { panic!() };
    assert!(v["path"].as_str().unwrap().ends_with("preview.png"));
    let img = decode(&std::fs::read(dir.join("preview.png")).unwrap());
    assert_eq!((img.0, img.1), (50, 100));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_arguments_and_hostile_pages_are_refused_without_panicking() {
    let hostile: &[u8] = b"/Nope cs 1 scn 0 0 10 10 re f /Gold cs 1 2 3 scn 1e308 1e308 re f q q q 9 9 9 9 9 9 cm Q BT /F9 1e300 Tf (x) Tj ET 0 0 0 1 k -1e20 -1e20 1e40 1e40 re f";
    let (mut app, doc, dir) = open("bad", &[(BARS, 0), (hostile, 0)], None);
    for (args, says) in [
        (json!({"doc":doc, "page":3}), "out of range"),
        (json!({"doc":doc, "page":1, "dpi":500}), "dpi"),
        (json!({"doc":doc, "page":1, "dpi":2}), "dpi"),
        (json!({"doc":doc, "page":1, "x":10}), "both x and y"),
        (json!({"doc":doc, "page":1, "x":500, "y":10}), "outside"),
        (json!({"doc":doc, "page":1, "path":""}), "path"),
        (json!({"doc":doc, "page":1, "path":"../escape.png"}), ""),
        (json!({"doc":99, "page":1}), ""),
    ] {
        let e = err(&mut app, "print_output_preview_render", args.clone());
        assert!(e.contains(says), "{args}: {e}");
    }
    for show in ["all", "device_cmyk", "not_device_cmyk", "spot", "rgb", "gray", "icc_based", "images", "solid_color", "text", "line_art"] {
        call(&mut app, "print_output_preview_set", json!({"doc":doc, "show":show, "coverage_warning":true}));
        let (v, _) = render(&mut app, json!({"doc":doc, "page":2, "dpi":9}));
        assert_eq!(v["page"], 2);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

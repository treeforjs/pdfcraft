//! Colour management settings end to end through the tool table: `color_settings_get`,
//! `color_settings_set`, `color_profiles_list` and `color_profile_load`, and their effect on
//! `page_render`, the separations preview, overprint preview, `print_convert_colors` and
//! `pdfx_convert`.

use pdfcraft_automation::{Automation, Content, ToolError};
use serde_json::{Value, json};

fn call(app: &mut Automation, name: &str, args: Value) -> Value {
    match app.call(name, &args).unwrap().remove(0) {
        Content::Json(v) => v,
        _ => panic!("JSON result"),
    }
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< /Length {} {dict} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// A PDF of `objs` (object 1 the catalog).
fn pdf(objs: &[Vec<u8>]) -> Vec<u8> {
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
    out
}

/// Five 40 × 100 pt bands on a 200 × 100 pt page: DeviceRGB (0.8 0.2 0.2), DeviceGray 0.5,
/// DeviceCMYK 100 % cyan, 100 % black and all four inks at 100 %.
const BANDS: &[u8] = b"0.8 0.2 0.2 rg 0 0 40 100 re f 0.5 g 40 0 40 100 re f 1 0 0 0 k 80 0 40 100 re f \
    0 0 0 1 k 120 0 40 100 re f 1 1 1 1 k 160 0 40 100 re f";

/// A temp root holding `bands.pdf` (with a `GTS_PDFX` output intent whose profile is
/// `intent_profile`, when given) and `press.icc` ([`test_press_profile`]).
fn root(name: &str, content: &[u8], intent_profile: Option<&[u8]>) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-colour-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let catalog = if intent_profile.is_some() {
        "<< /Type /Catalog /Pages 2 0 R /OutputIntents [<< /Type /OutputIntent /S /GTS_PDFX /OutputConditionIdentifier (Test press) /DestOutputProfile 5 0 R >>] >>"
    } else {
        "<< /Type /Catalog /Pages 2 0 R >>"
    };
    let mut objs = vec![
        catalog.as_bytes().to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /ExtGState << /OP << /OP true /op true /OPM 1 >> >> >> /Contents 4 0 R >>".to_vec(),
        stream("", content),
    ];
    if let Some(p) = intent_profile {
        objs.push(stream("/N 4", p));
    }
    std::fs::write(dir.join("bands.pdf"), pdf(&objs)).unwrap();
    std::fs::write(dir.join("press.icc"), test_press_profile()).unwrap();
    dir
}

fn open(dir: &std::path::Path) -> (Automation, u64) {
    let mut app = Automation::new().with_root(dir).unwrap();
    let doc = call(&mut app, "doc_open", json!({"path":"bands.pdf"}))["doc"].as_u64().unwrap();
    (app, doc)
}

/// The RGB of the rendered page (72 dpi) at (`x`, `y`) points from the top left.
fn pixel(app: &mut Automation, doc: u64, x: f64, y: f64) -> [u8; 3] {
    let Content::Png { data, width, .. } = app.call("page_render", &json!({"doc":doc,"page":1,"dpi":72})).unwrap().remove(0) else { panic!("PNG") };
    let mut reader = png::Decoder::new(std::io::Cursor::new(data)).read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    let channels = info.color_type.samples();
    let i = (y as usize * width as usize + x as usize) * channels;
    [buf[i], buf[i + 1], buf[i + 2]]
}

/// Every band's colour: RGB, gray, cyan, black, rich black.
fn bands(app: &mut Automation, doc: u64) -> [[u8; 3]; 5] {
    [20.0, 60.0, 100.0, 140.0, 180.0].map(|x| pixel(app, doc, x, 50.0))
}

fn near(a: [u8; 3], b: [u8; 3], tol: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol)
}

fn mft2(inputs: u8, outputs: u8, grid: &[Vec<f64>]) -> Vec<u8> {
    let mut lut = b"mft2\0\0\0\0".to_vec();
    lut.extend([inputs, outputs, 2, 0]);
    for v in [1i32, 0, 0, 0, 1, 0, 0, 0, 1] {
        lut.extend((v << 16).to_be_bytes());
    }
    lut.extend(2u16.to_be_bytes());
    lut.extend(2u16.to_be_bytes());
    for _ in 0..inputs {
        lut.extend([0u16, 65535].iter().flat_map(|v| v.to_be_bytes()));
    }
    for point in grid {
        for v in point {
            lut.extend(((v.clamp(0.0, 1.0) * 65535.0).round() as u16).to_be_bytes());
        }
    }
    for _ in 0..outputs {
        lut.extend([0u16, 65535].iter().flat_map(|v| v.to_be_bytes()));
    }
    lut
}

/// A synthetic CMYK output profile (lut16, Lab PCS). Display (`A2B0`): neutral, L* = 100 ×
/// (1 − 0.3C)(1 − 0.3M)(1 − 0.3Y)(1 − 0.8K), so 100 % K is a dark gray (L* 20) and its darkest
/// colour isn't black (L* 6.9). Into it: perceptual (`B2A0`) prints everything on black,
/// K = 1 − L*/100; relative colorimetric (`B2A1`) on cyan, magenta and yellow only.
fn test_press_profile() -> Vec<u8> {
    let corners = |n: usize| -> Vec<Vec<f64>> { (0..1usize << n).map(|i| (0..n).map(|b| ((i >> (n - 1 - b)) & 1) as f64).collect()).collect() };
    // Lab encoding of lut16 (v2): L* 0–100 over 0–65280, a* and b* 0 at 32768.
    let lab = |l: f64| vec![l * 652.8 / 65535.0, 32768.0 / 65535.0, 32768.0 / 65535.0];
    let a2b: Vec<Vec<f64>> =
        corners(4).iter().map(|c| lab(100.0 * (1.0 - 0.3 * c[0]) * (1.0 - 0.3 * c[1]) * (1.0 - 0.3 * c[2]) * (1.0 - 0.8 * c[3]))).collect();
    let l_of = |c: &[f64]| (c[0] * 65535.0 / 652.8 / 100.0).clamp(0.0, 1.0);
    let b2a0: Vec<Vec<f64>> = corners(3).iter().map(|c| vec![0.0, 0.0, 0.0, 1.0 - l_of(c)]).collect();
    let b2a1: Vec<Vec<f64>> = corners(3)
        .iter()
        .map(|c| {
            let v = 1.0 - l_of(c);
            vec![v, v, v, 0.0]
        })
        .collect();
    let mut desc = b"desc\0\0\0\0".to_vec();
    let text = b"Test press\0";
    desc.extend((text.len() as u32).to_be_bytes());
    desc.extend(text);
    desc.extend([0u8; 79]);
    let mut wtpt = b"XYZ \0\0\0\0".to_vec();
    for v in [0.9642f64, 1.0, 0.8249] {
        wtpt.extend(((v * 65536.0).round() as i32).to_be_bytes());
    }
    let tags: Vec<(&[u8; 4], Vec<u8>)> =
        vec![(b"desc", desc), (b"wtpt", wtpt), (b"A2B0", mft2(4, 3, &a2b)), (b"B2A0", mft2(3, 4, &b2a0)), (b"B2A1", mft2(3, 4, &b2a1))];
    let mut table = (tags.len() as u32).to_be_bytes().to_vec();
    let mut data = Vec::new();
    let mut offset = 128 + 4 + tags.len() * 12;
    for (sig, body) in &tags {
        table.extend(*sig);
        table.extend((offset as u32).to_be_bytes());
        table.extend((body.len() as u32).to_be_bytes());
        data.extend(body);
        let pad = (4 - body.len() % 4) % 4;
        data.extend(std::iter::repeat_n(0u8, pad));
        offset += body.len() + pad;
    }
    let size = 128 + table.len() + data.len();
    let mut h = (size as u32).to_be_bytes().to_vec();
    h.extend([0u8; 4]);
    h.extend([2, 0x10, 0, 0]);
    h.extend(b"prtrCMYKLab ");
    h.extend([0u8; 12]);
    h.extend(b"acsp");
    h.resize(68, 0);
    for v in [0.9642f64, 1.0, 0.8249] {
        h.extend(((v * 65536.0).round() as i32).to_be_bytes());
    }
    h.resize(128, 0);
    h.extend(table);
    h.extend(data);
    h
}

fn percent(sample: &Value, ink: &str) -> f64 {
    sample["inks"].as_array().unwrap().iter().find(|i| i["name"] == ink).unwrap()["percent"].as_f64().unwrap()
}

#[test]
fn the_defaults_and_built_in_profiles_are_listed() {
    let dir = root("list", BANDS, None);
    let (mut app, doc) = open(&dir);
    let got = call(&mut app, "color_settings_get", json!({"doc":doc}));
    let s = &got["settings"];
    assert_eq!((s["rgb"].as_str(), s["cmyk"].as_str(), s["gray"].as_str()), (Some("srgb"), Some("cgats001"), Some("sgray")));
    assert_eq!(
        (s["intent"].as_str(), s["black_point_compensation"].as_bool(), s["untagged"].as_str()),
        (Some("relative_colorimetric"), Some(false), Some("output_intent"))
    );
    assert_eq!(s["default"], true);
    assert_eq!(got["spaces"].as_array().unwrap().len(), 3);
    assert!(got["separation"]["conversion"].as_str().unwrap().contains("no output intent"), "{got}");
    let list = call(&mut app, "color_profiles_list", json!({}));
    let ids: Vec<&str> = list["profiles"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
    for id in ["srgb", "display-p3", "a98-rgb", "romm-rgb", "sgray", "gray-gamma-2.2", "dot-gain-20", "cgats001"] {
        assert!(ids.contains(&id), "{id} in {ids:?}");
    }
    assert!(list["profiles"].as_array().unwrap().iter().all(|p| p["displays"] == true && p["built_in"] == true));
    let cmyk = call(&mut app, "color_profiles_list", json!({"family":"cmyk"}));
    assert_eq!(cmyk["count"], 1);
    assert_eq!((cmyk["profiles"][0]["working"].as_bool(), cmyk["profiles"][0]["converts_into"].as_bool()), (Some(true), Some(false)));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn working_spaces_and_the_untagged_policy_change_how_device_colour_renders() {
    let dir = root("render", BANDS, None);
    let (mut app, doc) = open(&dir);
    let before = bands(&mut app, doc);
    // The factory settings render DeviceRGB and DeviceGray as sRGB numbers.
    assert!(near(before[0], [204, 51, 51], 1), "{before:?}");
    assert!(near(before[1], [128, 128, 128], 1), "{before:?}");
    // A wide-gamut RGB working space: the same numbers are a more saturated red on screen.
    call(&mut app, "color_settings_set", json!({"rgb":"display-p3"}));
    let p3 = bands(&mut app, doc);
    assert!(p3[0][0] > before[0][0] + 5 && p3[0][1] < before[0][1], "{p3:?} vs {before:?}");
    assert_eq!(&p3[1..], &before[1..], "only DeviceRGB changes");
    // A dot-gain gray working space prints 50 % gray darker.
    call(&mut app, "color_settings_set", json!({"gray":"dot-gain-20"}));
    let gain = bands(&mut app, doc);
    assert!(gain[1][0] + 20 < before[1][0] && gain[1][0] == gain[1][2], "{gain:?}");
    // Without colour management of untagged colour, CMYK converts naively and RGB/gray are
    // sRGB numbers again.
    let r = call(&mut app, "color_settings_set", json!({"untagged":"unmanaged","doc":doc}));
    assert!(r["spaces"].as_array().unwrap().iter().all(|s| s["source"] == "unmanaged"), "{r}");
    let naive = bands(&mut app, doc);
    assert_eq!(naive[2], [0, 255, 255]);
    assert_eq!(naive[3], [0, 0, 0]);
    assert!(near(naive[0], before[0], 1) && near(naive[1], before[1], 1), "{naive:?}");
    assert_ne!(before[2], naive[2], "the CMYK profile's cyan is not naive cyan");
    // Reset: exactly the factory rendering.
    let r = call(&mut app, "color_settings_set", json!({"reset":true}));
    assert_eq!(r["settings"]["default"], true);
    assert_eq!(bands(&mut app, doc), before);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_loaded_cmyk_profile_drives_rendering_separations_and_black_point_compensation() {
    let dir = root("press", b"1 0 0 rg 0 0 100 100 re f 0 0 0 1 k 100 0 50 100 re f 1 1 1 1 k 150 0 50 100 re f", None);
    let (mut app, doc) = open(&dir);
    let default_black = pixel(&mut app, doc, 120.0, 50.0);
    let default_red = call(&mut app, "print_separations_sample", json!({"doc":doc,"page":1,"x":50,"y":50}));
    assert!(default_red["conversion"].as_str().unwrap().contains("built-in"), "{default_red}");
    let loaded = call(&mut app, "color_profile_load", json!({"path":"press.icc","name":"Test press","working":true}));
    assert_eq!(loaded["profile"]["id"], "user:Test press");
    assert_eq!(
        (loaded["profile"]["family"].as_str(), loaded["profile"]["displays"].as_bool(), loaded["profile"]["converts_into"].as_bool()),
        (Some("cmyk"), Some(true), Some(true))
    );
    assert_eq!(loaded["settings"]["cmyk"], "user:Test press");
    // DeviceCMYK displays through the profile: 100 % K is its neutral L* 20 gray.
    let k = pixel(&mut app, doc, 120.0, 50.0);
    assert_ne!(k, default_black);
    assert!(k[0].abs_diff(k[1]) <= 2 && k[1].abs_diff(k[2]) <= 2 && (35..=60).contains(&k[0]), "{k:?}");
    let rich = pixel(&mut app, doc, 175.0, 50.0);
    assert!(rich[0] > 10, "the profile's darkest colour is not black: {rich:?}");
    // Black point compensation maps it to display black, and lightens nothing to white.
    call(&mut app, "color_settings_set", json!({"black_point_compensation":true}));
    let rich_bpc = pixel(&mut app, doc, 175.0, 50.0);
    assert!(rich_bpc.iter().all(|v| *v <= 2), "{rich_bpc:?}");
    assert!(pixel(&mut app, doc, 120.0, 50.0)[0] < k[0]);
    // RGB separates through the working CMYK profile with the intent: relative colorimetric
    // (the default) puts red on C, M and Y; perceptual on black only.
    let rel = call(&mut app, "print_separations_sample", json!({"doc":doc,"page":1,"x":50,"y":50}));
    assert!(rel["conversion"].as_str().unwrap().contains("working CMYK space"), "{rel}");
    assert!(percent(&rel, "Cyan") > 30.0 && percent(&rel, "Black") < 1.0, "{rel}");
    let got = call(&mut app, "color_settings_set", json!({"intent":"perceptual","doc":doc}));
    assert!(got["separation"]["conversion"].as_str().unwrap().contains("perceptual"), "{got}");
    let per = call(&mut app, "print_separations_sample", json!({"doc":doc,"page":1,"x":50,"y":50}));
    assert!(percent(&per, "Black") > 30.0 && percent(&per, "Cyan") < 1.0, "{per}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_output_intent_overrides_the_working_space_unless_the_policy_says_otherwise() {
    let dir = root("intent", BANDS, Some(&test_press_profile()));
    let (mut app, doc) = open(&dir);
    let got = call(&mut app, "color_settings_get", json!({"doc":doc}));
    let cmyk = got["spaces"].as_array().unwrap().iter().find(|s| s["family"] == "cmyk").unwrap().clone();
    assert_eq!((cmyk["source"].as_str(), cmyk["working"].as_str()), (Some("output_intent"), Some("cgats001")), "{got}");
    assert!(got["separation"]["conversion"].as_str().unwrap().contains("Test press"), "{got}");
    let through_intent = bands(&mut app, doc);
    // 100 % K through the output intent's profile: its neutral dark gray.
    assert!(through_intent[3][0].abs_diff(through_intent[3][2]) <= 2 && (35..=60).contains(&through_intent[3][0]), "{through_intent:?}");
    let r = call(&mut app, "color_settings_set", json!({"untagged":"working_space","doc":doc}));
    let cmyk = r["spaces"].as_array().unwrap().iter().find(|s| s["family"] == "cmyk").unwrap().clone();
    assert_eq!(cmyk["source"], "working_space");
    let working = bands(&mut app, doc);
    assert_ne!(working[2], through_intent[2], "cyan through the working space differs");
    // Overprint preview composites through the same display conversion as the page.
    call(&mut app, "color_settings_set", json!({"untagged":"output_intent"}));
    let content = b"0 0 0 1 k 0 0 200 100 re f";
    std::fs::write(dir.join("k.pdf"), pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R /OutputIntents [<< /Type /OutputIntent /S /GTS_PDFX /OutputConditionIdentifier (Test press) /DestOutputProfile 5 0 R >>] >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /ExtGState << /OP << /OP true /op true /OPM 1 >> >> >> /Contents 4 0 R >>".to_vec(),
        stream("", &[content.as_slice(), b" /OP gs 1 0 0 0 k 50 0 100 100 re f"].concat()),
        stream("/N 4", &test_press_profile()),
    ])).unwrap();
    let k = call(&mut app, "doc_open", json!({"path":"k.pdf"}))["doc"].as_u64().unwrap();
    let knocked = pixel_with(&mut app, json!({"doc":k,"page":1,"dpi":72,"overprint_preview":"never"}), 100.0, 50.0);
    let over = pixel_with(&mut app, json!({"doc":k,"page":1,"dpi":72,"overprint_preview":"always"}), 100.0, 50.0);
    // Cyan over black on the test press (L* 100·0.7·0.2 = 14) is darker than cyan alone (L* 70).
    assert!(over[0] + 20 < knocked[0], "{over:?} vs {knocked:?}");
    assert!(over[0].abs_diff(over[2]) <= 3, "the test press is neutral: {over:?}");
    std::fs::remove_dir_all(dir).unwrap();
}

fn pixel_with(app: &mut Automation, args: Value, x: f64, y: f64) -> [u8; 3] {
    let Content::Png { data, width, .. } = app.call("page_render", &args).unwrap().remove(0) else { panic!("PNG") };
    let mut reader = png::Decoder::new(std::io::Cursor::new(data)).read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    let channels = info.color_type.samples();
    let i = (y as usize * width as usize + x as usize) * channels;
    [buf[i], buf[i + 1], buf[i + 2]]
}

#[test]
fn convert_colors_and_pdfx_take_their_defaults_from_the_settings() {
    let dir = root("convert", b"1 0 0 rg 0 0 200 100 re f", None);
    let (mut app, doc) = open(&dir);
    // Factory settings: the built-in approximation, relative colorimetric.
    let r = call(&mut app, "print_convert_colors", json!({"doc":doc}));
    assert_eq!(
        (r["destination"].as_str(), r["through"].as_str(), r["intent"].as_str(), r["approximate"].as_bool()),
        (Some("DeviceCMYK"), None, Some("relative_colorimetric"), Some(true)),
        "{r}"
    );
    call(&mut app, "edit_undo", json!({"doc":doc}));
    // A loaded working CMYK space: DeviceCMYK through its tables, with the settings' intent.
    call(&mut app, "color_profile_load", json!({"path":"press.icc","working":true}));
    call(&mut app, "color_settings_set", json!({"intent":"perceptual"}));
    let r = call(&mut app, "print_convert_colors", json!({"doc":doc}));
    assert_eq!(
        (r["destination"].as_str(), r["intent"].as_str(), r["approximate"].as_bool()),
        (Some("DeviceCMYK"), Some("perceptual"), Some(false)),
        "{r}"
    );
    assert!(r["through"].as_str().unwrap().contains("user:press"), "{r}");
    let s = call(&mut app, "print_separations_sample", json!({"doc":doc,"page":1,"x":50,"y":50}));
    assert!(percent(&s, "Black") > 30.0 && percent(&s, "Cyan") < 1.0 && percent(&s, "Magenta") < 1.0, "converted to K only: {s}");
    call(&mut app, "edit_undo", json!({"doc":doc}));
    // use_working_space false keeps the built-in approximation.
    let r = call(&mut app, "print_convert_colors", json!({"doc":doc,"use_working_space":false}));
    assert_eq!((r["through"].as_str(), r["approximate"].as_bool()), (None, Some(true)), "{r}");
    call(&mut app, "edit_undo", json!({"doc":doc}));
    // Convert to PDF/X without an intent uses the working CMYK output profile.
    let x = call(&mut app, "pdfx_convert", json!({"doc":doc,"level":"x1a"}));
    assert_eq!(x["output_intent_from_settings"], "Test press", "{x}");
    assert_eq!((x["converted"].as_bool(), x["conforms"].as_bool()), (Some(true), Some(true)), "{x}");
    let got = call(&mut app, "color_settings_get", json!({"doc":doc}));
    let cmyk = got["spaces"].as_array().unwrap().iter().find(|s| s["family"] == "cmyk").unwrap().clone();
    assert_eq!(cmyk["source"], "output_intent", "{got}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn bad_settings_and_profiles_are_refused() {
    let dir = root("bad", BANDS, None);
    std::fs::write(dir.join("junk.icc"), b"not a profile").unwrap();
    std::fs::write(dir.join("short.icc"), vec![0u8; 300]).unwrap();
    // A profile whose tables are cut short: parsed without panicking, refused.
    let mut cut = test_press_profile();
    cut.truncate(400);
    let n = cut.len() as u32;
    cut[0..4].copy_from_slice(&n.to_be_bytes());
    std::fs::write(dir.join("cut.icc"), cut).unwrap();
    let (mut app, doc) = open(&dir);
    for args in [
        json!({"rgb":"cgats001"}),
        json!({"cmyk":"srgb"}),
        json!({"gray":"no-such-profile"}),
        json!({"intent":"vivid"}),
        json!({"untagged":"sometimes"}),
        json!({"rgb":"user:missing"}),
        json!({"rgb":7}),
    ] {
        let e = app.call("color_settings_set", &args);
        assert!(matches!(e, Err(ToolError::InvalidArgs(_))), "{args}: {e:?}");
    }
    for args in [
        json!({"path":"junk.icc"}),
        json!({"path":"short.icc"}),
        json!({"path":"cut.icc"}),
        json!({"path":"press.icc","name":"a/b"}),
        json!({"path":"press.icc","name":""}),
    ] {
        let e = app.call("color_profile_load", &args);
        assert!(matches!(e, Err(ToolError::InvalidArgs(_))), "{args}: {e:?}");
    }
    assert!(app.call("color_profile_load", &json!({"path":"missing.icc"})).is_err());
    assert!(app.call("color_profile_load", &json!({"path":"../outside.icc"})).is_err());
    assert!(matches!(app.call("color_profiles_list", &json!({"family":"lab"})), Err(ToolError::InvalidArgs(_))));
    // Nothing changed, and the page still renders.
    let got = call(&mut app, "color_settings_get", json!({}));
    assert_eq!(got["settings"]["default"], true);
    let _ = bands(&mut app, doc);
    std::fs::remove_dir_all(dir).unwrap();
}

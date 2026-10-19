use std::sync::Arc;

use super::*;
use crate::coverage::BuiltIn;

fn stream(extra: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< /Length {} {extra} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// A one-page 200 × 200 pt document drawing `content` with `resources`; `extra` objects follow
/// as 5, 6, …
fn pdf(resources: &str, content: &[u8], extra: &[Vec<u8>], catalog_extra: &str) -> Document {
    let mut objs: Vec<Vec<u8>> = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>").into_bytes(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources {resources} /Contents 4 0 R >>").into_bytes(),
        stream("", content),
    ];
    objs.extend(extra.iter().cloned());
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
    Document::open(Arc::new(out)).unwrap()
}

/// The pixel at user-space point (x, y) of a 72 dpi preview of a 200 pt page.
fn px(p: &Preview, x: usize, y: usize) -> [u8; 3] {
    let row = 199 - y;
    let at = (row * p.width + x) * 3;
    [p.rgb[at], p.rgb[at + 1], p.rgb[at + 2]]
}

const RES: &str = "<< /ColorSpace << \
    /Gold [/Separation /Gold /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.2 1 0] /N 1 >>] \
    /Icc [/ICCBased 5 0 R] >> \
    /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> \
    /XObject << /Im 6 0 R >> >>";

/// Four boxes: DeviceCMYK cyan (0–40), DeviceRGB red (50–90), Gold (100–140) and ICC gray
/// (150–190) along the bottom; text at the top left; an image at the top right.
fn mixed() -> Document {
    let content = b"1 0 0 0 k 0 0 40 40 re f 1 0 0 rg 50 0 40 40 re f /Gold cs 1 scn 100 0 40 40 re f /Icc cs 0.5 sc 150 0 40 40 re f \
        0 0 0 1 k BT /F1 40 Tf 10 150 Td (II) Tj ET q 40 0 0 40 150 150 cm /Im Do Q";
    let icc = stream("/N 1 /Alternate /DeviceGray", b"");
    let image = stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceCMYK", &[0, 255, 0, 0]);
    pdf(RES, content, &[icc, image], "")
}

fn preview(d: &Document, settings: &Settings, display: &Display) -> Preview {
    render(d, 0, 72.0, settings, &BuiltIn, display).unwrap()
}

const WHITE: [u8; 3] = [255, 255, 255];

#[test]
fn show_filters_keep_only_the_matching_objects() {
    let d = mixed();
    let display = Display::renderer();
    let all = preview(&d, &Settings::default(), &display);
    assert_eq!((all.width, all.height), (200, 200));
    for (x, y) in [(20, 20), (70, 20), (120, 20), (170, 20), (170, 170)] {
        assert_ne!(px(&all, x, y), WHITE, "({x}, {y}) is painted");
    }
    assert_eq!(all.raster.hidden_paints, 0);
    let only = |show: Show| preview(&d, &Settings { show, ..Settings::default() }, &display);
    let shown = |p: &Preview| -> Vec<bool> { [(20, 20), (70, 20), (120, 20), (170, 20), (170, 170)].map(|(x, y)| px(p, x, y) != WHITE).to_vec() };
    // cyan box, red box, gold box, ICC gray box, image
    assert_eq!(shown(&only(Show::DeviceCmyk)), [true, false, false, false, true]);
    assert_eq!(shown(&only(Show::NotDeviceCmyk)), [false, true, true, true, false]);
    assert_eq!(shown(&only(Show::Rgb)), [false, true, false, false, false]);
    assert_eq!(shown(&only(Show::Spot)), [false, false, true, false, false]);
    assert_eq!(shown(&only(Show::IccBased)), [false, false, false, true, false]);
    assert_eq!(shown(&only(Show::Gray)), [false, false, false, true, false]);
    assert_eq!(shown(&only(Show::Images)), [false, false, false, false, true]);
    assert_eq!(shown(&only(Show::LineArt)), [true, true, true, true, false]);
    assert_eq!(shown(&only(Show::SolidColor)), [true, true, true, true, false]);
    assert_eq!(shown(&only(Show::Text)), [false; 5]);
    let text = only(Show::Text);
    assert!(text.raster.hidden_paints >= 5, "{}", text.raster.hidden_paints);
    let black = text.inks.iter().find(|i| i.info.name == "Black").unwrap();
    assert_eq!(black.max, 100.0, "the text is still there");
    assert_eq!(only(Show::LineArt).inks.iter().find(|i| i.info.name == "Black").unwrap().max, 50.0, "only the ICC gray box");
    assert_eq!(Show::from_id("Spot Color"), Some(Show::Spot));
    assert_eq!(Show::from_id("device_cmyk"), Some(Show::DeviceCmyk));
    assert_eq!(Show::from_id("smooth"), None);
}

#[test]
fn hidden_separations_leave_their_ink_out_but_count_for_coverage() {
    let d = pdf("<< >>", b"1 0 0 0 k 0 0 40 40 re f 1 1 1 1 k 100 100 40 40 re f", &[], "");
    let display = Display::renderer();
    let p = preview(&d, &Settings::default(), &display);
    assert_ne!(px(&p, 20, 20), WHITE);
    let hidden = Settings { hidden_inks: ["Cyan".to_string()].into(), ..Settings::default() };
    let q = preview(&d, &hidden, &display);
    assert_eq!(px(&q, 20, 20), WHITE);
    assert!(!q.inks[0].shown && q.inks[1].shown);
    assert_eq!(q.coverage.max, 400.0, "coverage counts every ink");
    assert_eq!(q.inks[0].max, 100.0);
    assert_eq!(q.inks[0].inked_percent, 8.0, "two 40 × 40 boxes on 200 × 200 have cyan");
    assert_eq!(q.inks[0].mean, 8.0);
    assert_eq!(q.inks[2].inked_percent, 4.0);
}

#[test]
fn coverage_warnings_paint_what_is_over_the_limit() {
    let d = pdf("<< >>", b"1 1 1 0 k 0 0 40 40 re f 1 1 1 1 k 100 100 50 20 re f 0 0 0 1 k 0 100 20 20 re f", &[], "");
    let display = Display::renderer();
    let off = preview(&d, &Settings::default(), &display);
    assert!(!off.coverage.warning && off.coverage.warned_pixels == 0);
    assert_eq!(off.coverage.limit, 300.0);
    assert_eq!(off.coverage.over_cells, 1000, "only the 400% box is over 300%");
    let on = Settings { coverage_warning: Some(250.0), warning_colour: [255, 0, 255], ..Settings::default() };
    let p = preview(&d, &on, &display);
    assert_eq!(p.coverage.max, 400.0);
    let at = p.coverage.max_at.unwrap();
    assert!((100.0..150.0).contains(&at[0]) && (100.0..120.0).contains(&at[1]), "{at:?}");
    assert_eq!(p.coverage.over_cells, 1600 + 1000);
    assert_eq!(p.coverage.warned_pixels, 2600);
    assert_eq!(p.coverage.over_area, 2600.0);
    assert_eq!(p.coverage.over_percent, 6.5);
    assert_eq!(px(&p, 20, 20), [255, 0, 255]);
    assert_eq!(px(&p, 120, 110), [255, 0, 255]);
    assert_ne!(px(&p, 10, 110), [255, 0, 255], "100% black is under the limit");
}

#[test]
fn a_simulation_profile_shows_paper_and_black_ink() {
    let paper = [0.92, 0.95, 0.70];
    let bytes = synthetic_cmyk_profile(paper, 20.0);
    let display = Display::from_profile(&bytes, "test press").unwrap();
    assert!(display.has_profile());
    assert!((display.media_white[2] - 0.70).abs() < 1e-3, "{:?}", display.media_white);
    assert!((display.black_point[1] - lab_to_xyz([20.0, 0.0, 0.0])[1]).abs() < 0.01, "{:?}", display.black_point);
    let d = pdf("<< >>", b"0 0 0 1 k 0 0 40 40 re f 0.5 0 0 0 k 100 0 40 40 re f", &[], "");
    let normal = preview(&d, &Settings::default(), &display);
    assert_eq!(normal.simulated, Simulate::default());
    let paper_px = px(&normal, 100, 100);
    assert!(paper_px.iter().all(|v| *v >= 253), "relative: the paper is white {paper_px:?}");
    let k = px(&normal, 20, 20);
    assert!(k.iter().all(|v| *v <= 3), "black point compensation: black is black {k:?}");
    let black = Settings { simulate: Simulate { paper: false, black: true }, ..Settings::default() };
    let b = preview(&d, &black, &display);
    let k = px(&b, 20, 20);
    // L* 20 is about 19% sRGB.
    assert!(k.iter().all(|v| (40..=56).contains(v)) && k[0].abs_diff(k[2]) <= 2, "black ink prints dark gray {k:?}");
    assert!(px(&b, 100, 100).iter().all(|v| *v >= 253));
    let both = Settings { simulate: Simulate { paper: true, black: true }, ..Settings::default() };
    let p = preview(&d, &both, &display);
    let paper_px = px(&p, 100, 100);
    let expected = xyz_to_srgb(paper).map(|v| (v * 255.0).round() as u8);
    for i in 0..3 {
        assert!(paper_px[i].abs_diff(expected[i]) <= 2, "{paper_px:?} vs {expected:?}");
    }
    assert!(paper_px[2] + 10 < paper_px[0], "a yellowish paper {paper_px:?}");
    let cyan_relative = px(&b, 120, 20);
    let cyan_paper = px(&p, 120, 20);
    assert!(cyan_paper[2] < cyan_relative[2], "the paper tints the ink too");
    // Without a profile paper and black ink can't be simulated.
    let r = preview(&d, &both, &Display::renderer());
    assert_eq!(r.simulated, Simulate::default());
}

#[test]
fn spot_inks_multiply_their_alternate() {
    let d = mixed();
    let display = Display::renderer();
    let p = preview(&d, &Settings { show: Show::Spot, ..Settings::default() }, &display);
    let gold = px(&p, 120, 20);
    let alt = display.srgb([0.0, 0.2, 1.0, 0.0], Simulate::default(), &BuiltIn).map(|v| (v * 255.0).round() as u8);
    assert_eq!(gold, alt);
    let hidden = Settings { show: Show::Spot, hidden_inks: ["Gold".to_string()].into(), ..Settings::default() };
    assert_eq!(px(&preview(&d, &hidden, &display), 120, 20), WHITE);
}

#[test]
fn bad_settings_and_profiles_are_refused() {
    let d = mixed();
    let display = Display::renderer();
    for bad in [
        Settings { coverage_warning: Some(0.0), ..Settings::default() },
        Settings { coverage_warning: Some(f64::NAN), ..Settings::default() },
        Settings { simulate: Simulate { paper: true, black: false }, ..Settings::default() },
        Settings { hidden_inks: [String::new()].into(), ..Settings::default() },
        Settings { hidden_inks: (0..300).map(|i| format!("ink{i}")).collect(), ..Settings::default() },
    ] {
        assert!(render(&d, 0, 72.0, &bad, &BuiltIn, &display).is_err(), "{bad:?}");
    }
    assert!(render(&d, 3, 72.0, &Settings::default(), &BuiltIn, &display).is_err());
    let mut srgb = crate::icc::srgb();
    assert!(Display::from_profile(&srgb, "srgb").unwrap_err().contains("CMYK"));
    for junk in [Vec::new(), vec![0u8; 200], b"not a profile".to_vec()] {
        assert!(Display::from_profile(&junk, "junk").is_err());
    }
    // A profile cut short, or with its tables scrambled, never panics.
    let good = synthetic_cmyk_profile([0.9642, 1.0, 0.8249], 10.0);
    for cut in [140, 300, good.len() / 2, good.len() - 10] {
        let _ = Display::from_profile(&good[..cut], "cut");
    }
    let mut scrambled = good.clone();
    for (i, b) in scrambled.iter_mut().enumerate().skip(300) {
        *b = (i * 37 % 251) as u8;
    }
    if let Ok(d2) = Display::from_profile(&scrambled, "scrambled") {
        let _ = d2.srgb([0.3, 0.2, 0.1, 0.5], Simulate { paper: true, black: true }, &BuiltIn);
    }
    srgb.truncate(10);
    assert!(Display::from_profile(&srgb, "short").is_err());
}

#[test]
fn hostile_spaces_classify_without_panicking() {
    let d = pdf(
        "<< /ColorSpace << /A /B /B /A /N [/DeviceN 7 /DeviceCMYK 0] /I [/Indexed [/Indexed /A 1 <00>] 1 <0000>] /S [/Separation] >> >>",
        b"/A cs 1 sc 0 0 10 10 re f /N cs 1 scn 10 10 10 10 re f /I cs 0 sc 20 20 10 10 re f /S cs 1 sc 30 30 10 10 re f",
        &[],
        "",
    );
    for show in Show::ALL {
        let _ = render(&d, 0, 9.0, &Settings { show, ..Settings::default() }, &BuiltIn, &Display::renderer());
    }
}

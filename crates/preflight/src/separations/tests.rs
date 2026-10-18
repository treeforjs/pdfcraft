use std::sync::Arc;

use super::*;
use crate::coverage::{Options, page_coverage, page_coverage_with};

fn stream(extra: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< /Length {} {extra} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// A document whose pages have `contents` (objects 3.. are pages and their contents, then
/// `extra` objects); every page shares `resources`. `catalog_extra` goes into the catalog.
fn pdf(resources: &str, contents: &[&[u8]], extra: &[Vec<u8>], catalog_extra: &str) -> Document {
    let n = contents.len();
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + 2 * i)).collect();
    let mut objs: Vec<Vec<u8>> = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>").into_bytes(),
        format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")).into_bytes(),
    ];
    for (i, c) in contents.iter().enumerate() {
        objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources {resources} /Contents {} 0 R >>", 4 + 2 * i).into_bytes());
        objs.push(stream("", c));
    }
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

/// Spot colour resources: Gold (Type 2 into CMYK 0 0.2 1 0), DeviceN Cyan + Teal (Teal defined
/// in Colorants), an unused Silver, registration and None.
const SPOTS: &str = "<< /ColorSpace << \
    /Gold [/Separation /Gold /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.2 1 0] /N 1 >>] \
    /Duo [/DeviceN [/Cyan /Teal] /DeviceCMYK << /FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1] >> \
        << /Colorants << /Teal [/Separation /Teal /DeviceRGB << /FunctionType 2 /Domain [0 1] /C0 [1 1 1] /C1 [0 0.5 0.5] /N 1 >>] >> >>] \
    /Silver [/Separation /Silver /DeviceGray << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0.4] /N 1 >>] \
    /Reg [/Separation /All /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [1 1 1 1] /N 1 >>] \
    /Off [/Separation /None /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [1 1 1 1] /N 1 >>] \
    >> /ExtGState << /OP << /OP true /op true /OPM 1 >> /OP0 << /OP true /op true /OPM 0 >> /Soft << /ca 0.5 >> >> >>";

fn raster(d: &Document, page: usize) -> Raster {
    rasterize(d, page, 72.0, true, &BuiltIn).unwrap()
}

fn at(r: &Raster, x: f64, y: f64) -> Sample {
    sample(r, x, y).unwrap()
}

fn ink(s: &Sample, name: &str) -> f64 {
    s.inks.iter().find(|(n, _)| n == name).map(|(_, v)| *v).unwrap_or_else(|| panic!("no {name} in {:?}", s.inks))
}

#[test]
fn the_ink_list_has_process_and_spot_inks_with_their_alternates_and_pages() {
    let d = pdf(
        SPOTS,
        &[b"0.1 0.2 0.3 0.4 k 0 0 50 50 re f /Gold cs 0.5 scn 60 0 50 50 re f", b"/Duo cs 0.2 0.8 scn 0 0 50 50 re f 1 0 0 rg 60 60 10 10 re f"],
        &[],
        "",
    );
    let list = list_inks(&d, &[], &BuiltIn).unwrap();
    let names: Vec<&str> = list.inks.iter().map(|i| i.info.name.as_str()).collect();
    assert_eq!(&names[..4], &["Cyan", "Magenta", "Yellow", "Black"]);
    for spot in ["Gold", "Teal", "Silver"] {
        assert!(names.contains(&spot), "{names:?}");
    }
    // Registration and None are not inks.
    assert!(!names.contains(&"All") && !names.contains(&"None"), "{names:?}");
    let get = |n: &str| list.inks.iter().find(|i| i.info.name == n).unwrap();
    assert_eq!(get("Cyan").pages, vec![0, 1]);
    // The red square on page 2 converts to magenta and yellow.
    assert_eq!(get("Magenta").pages, vec![0, 1]);
    assert_eq!(get("Black").pages, vec![0]);
    let gold = get("Gold");
    assert_eq!(gold.pages, vec![0]);
    assert_eq!(gold.declared_pages, vec![0, 1]);
    assert!(!gold.info.process);
    assert_eq!(gold.info.defined_by, Some("Separation"));
    assert_eq!(gold.info.alternate.as_deref(), Some("DeviceCMYK"));
    assert_eq!(gold.info.tint_transform, Some(2));
    assert_eq!(gold.info.alternate_value, Some(vec![0.0, 0.2, 1.0, 0.0]));
    assert!(gold.info.preview_exact);
    let teal = get("Teal");
    assert_eq!(teal.pages, vec![1]);
    assert_eq!(teal.info.alternate.as_deref(), Some("DeviceRGB"), "Colorants defines Teal");
    assert_eq!(teal.info.alternate_value, Some(vec![0.0, 0.5, 0.5]));
    let silver = get("Silver");
    assert!(silver.pages.is_empty(), "declared but never painted");
    assert_eq!(silver.declared_pages, vec![0, 1]);
    assert_eq!(list.converted_pages, vec![1]);
    assert!(list.conversion.contains("built-in"));
    // Only some pages.
    let second = list_inks(&d, &[1], &BuiltIn).unwrap();
    assert_eq!(second.pages, vec![1]);
    assert_eq!(second.inks.iter().find(|i| i.info.name == "Gold").unwrap().pages, Vec::<usize>::new());
}

#[test]
fn plates_hold_the_exact_ink_at_known_points() {
    let d = pdf(
        SPOTS,
        &[b"0.1 0.2 0.3 0.4 k 0 0 50 50 re f /Gold cs 0.5 scn 60 0 50 50 re f /Duo cs 0.25 0.75 scn 120 0 50 50 re f \
            0.5 g 0 60 50 50 re f /Off cs 1 scn 0 60 50 50 re f /Reg cs 1 scn 180 180 10 10 re f"],
        &[],
        "",
    );
    let r = raster(&d, 0);
    let s = at(&r, 25.0, 25.0);
    assert_eq!((ink(&s, "Cyan"), ink(&s, "Magenta"), ink(&s, "Yellow"), ink(&s, "Black")), (10.0, 20.0, 30.0, 40.0));
    assert_eq!(ink(&s, "Gold"), 0.0);
    assert_eq!(s.total, 100.0);
    let gold = at(&r, 85.0, 25.0);
    assert_eq!((ink(&gold, "Gold"), gold.total), (50.0, 50.0), "{gold:?}");
    let duo = at(&r, 145.0, 25.0);
    assert_eq!((ink(&duo, "Cyan"), ink(&duo, "Teal"), duo.total), (25.0, 75.0, 100.0));
    // Gray is black ink; a None colour paints nothing over it.
    let gray = at(&r, 25.0, 85.0);
    assert_eq!((ink(&gray, "Black"), gray.total), (50.0, 50.0));
    // Registration puts 100 % on every plate.
    let reg = at(&r, 185.0, 185.0);
    assert!(reg.inks.iter().all(|(_, v)| *v == 100.0), "{reg:?}");
    assert_eq!(reg.total, 100.0 * r.inks.len() as f64);
    // Paper, and outside the page.
    assert_eq!(at(&r, 190.0, 100.0).total, 0.0);
    assert!(sample(&r, 300.0, 10.0).is_none());
    assert!(sample(&r, f64::NAN, 10.0).is_none());
    assert_eq!(r.exact_paints, 6);
    assert_eq!(r.converted_paints, 0);
}

#[test]
fn overprint_keeps_the_plates_below_and_can_be_turned_off() {
    let content: &[u8] = b"1 0 0 0 k 0 0 100 100 re f q /OP gs 0 1 0 0 k 0 0 50 50 re f Q \
        q /OP0 gs 0 1 0 0 k 50 50 50 50 re f Q q /OP gs /Gold cs 1 scn 0 50 50 50 re f Q";
    let d = pdf(SPOTS, &[content], &[], "");
    let r = raster(&d, 0);
    // Overprint mode 1: DeviceCMYK zeros leave the cyan below.
    let opm1 = at(&r, 25.0, 25.0);
    assert_eq!((ink(&opm1, "Cyan"), ink(&opm1, "Magenta")), (100.0, 100.0));
    // Overprint mode 0: a process colour replaces all four process plates.
    let opm0 = at(&r, 75.0, 75.0);
    assert_eq!((ink(&opm0, "Cyan"), ink(&opm0, "Magenta")), (0.0, 100.0));
    // A spot colour overprints only its own plate.
    let spot = at(&r, 25.0, 75.0);
    assert_eq!((ink(&spot, "Cyan"), ink(&spot, "Gold"), spot.total), (100.0, 100.0, 200.0));
    // Not simulated: everything knocks out.
    let knock = rasterize(&d, 0, 72.0, false, &BuiltIn).unwrap();
    assert!(!knock.overprint);
    let opm1 = at(&knock, 25.0, 25.0);
    assert_eq!((ink(&opm1, "Cyan"), ink(&opm1, "Magenta")), (0.0, 100.0));
    let spot = at(&knock, 25.0, 75.0);
    assert_eq!((ink(&spot, "Cyan"), ink(&spot, "Gold")), (0.0, 100.0));
    // And the total area coverage follows.
    let on = page_coverage(&d, 0, &Options::default()).unwrap();
    let off = page_coverage(&d, 0, &Options { overprint: false, ..Options::default() }).unwrap();
    assert_eq!((on.max, off.max), (200.0, 100.0));
}

#[test]
fn total_area_coverage_reports_its_maximum_and_where_it_is() {
    let d = pdf("<< >>", &[b"0.5 0.5 0.5 0.5 k 0 0 200 200 re f 1 1 1 0.9 k 120 140 20 20 re f", b""], &[], "");
    let c = page_coverage(&d, 0, &Options::default()).unwrap();
    assert_eq!(c.max, 390.0);
    let [x, y] = c.max_at.unwrap();
    assert!((120.0..140.0).contains(&x) && (140.0..160.0).contains(&y), "{x} {y}");
    // The topmost, leftmost cell of the maximum.
    assert_eq!([x, y], [120.5, 159.5]);
    assert!(c.flagged());
    assert!((c.over_area - 400.0).abs() < 1.0, "{}", c.over_area);
    let low = page_coverage(&d, 0, &Options { limit: 150.0, ..Options::default() }).unwrap();
    assert_eq!(low.over_percent, 100.0);
    let empty = page_coverage(&d, 1, &Options::default()).unwrap();
    assert_eq!((empty.max, empty.max_at), (0.0, None));
}

struct BlackOnly;

impl Conversion for BlackOnly {
    fn describe(&self) -> String {
        "test profile".into()
    }
    fn uses_profile(&self) -> bool {
        true
    }
    fn rgb(&self, [r, g, b]: [f64; 3]) -> [f64; 4] {
        [0.0, 0.0, 0.0, 1.0 - (r + g + b) / 3.0]
    }
    fn lab(&self, [l, _, _]: [f64; 3]) -> [f64; 4] {
        [0.0, 0.0, 0.0, 1.0 - l / 100.0]
    }
}

#[test]
fn rgb_and_lab_are_converted_by_the_conversion_given_and_say_so() {
    let d = pdf(
        "<< /ColorSpace << /L [/Lab << /WhitePoint [0.9642 1 0.8249] >>] >> >>",
        &[b"0 0 0 rg 0 0 50 50 re f 1 0 0 rg 60 0 50 50 re f /L cs 50 0 0 sc 120 0 50 50 re f"],
        &[],
        "",
    );
    let r = raster(&d, 0);
    assert_eq!(r.converted_paints, 3);
    assert!(!r.conversion_from_profile);
    let black = at(&r, 25.0, 25.0);
    assert_eq!((ink(&black, "Cyan"), ink(&black, "Black"), black.total), (30.0, 100.0, 190.0));
    let red = at(&r, 85.0, 25.0);
    assert_eq!((ink(&red, "Magenta"), ink(&red, "Yellow"), red.total), (100.0, 100.0, 200.0));
    // L* 50 is a mid gray: sRGB ≈ 0.466.
    let lab = at(&r, 145.0, 25.0);
    assert!((ink(&lab, "Black") - 53.4).abs() < 1.0, "{lab:?}");
    let profiled = rasterize(&d, 0, 72.0, true, &BlackOnly).unwrap();
    assert!(profiled.conversion_from_profile && profiled.conversion == "test profile");
    let black = at(&profiled, 25.0, 25.0);
    assert_eq!((ink(&black, "Cyan"), black.total), (0.0, 100.0));
    assert_eq!(at(&profiled, 145.0, 25.0).total, 50.0);
    let c = page_coverage_with(&d, 0, &Options::default(), &BlackOnly).unwrap();
    assert_eq!((c.max, c.conversion.as_str()), (100.0, "test profile"));
}

#[test]
fn plates_render_as_film_with_the_top_of_the_page_first() {
    let d = pdf(SPOTS, &[b"/Gold cs 1 scn 0 100 200 100 re f 0 0 0 0.5 k 0 0 200 100 re f"], &[], "");
    let r = rasterize(&d, 0, 36.0, true, &BuiltIn).unwrap();
    assert_eq!((r.width, r.height), (100, 100));
    let gold = r.plate_gray(r.ink("Gold").unwrap()).unwrap();
    assert_eq!(gold.len(), 100 * 100);
    assert_eq!((gold[0], gold[99 * 100]), (0, 255), "full ink is black, at the top");
    let black = r.plate_gray(r.ink("Black").unwrap()).unwrap();
    assert_eq!((black[0], black[99 * 100]), (255, 128));
    assert!(r.plate_gray(99).is_none());
    // A composite of only the black plate: gold hidden.
    let k = r.composite(&[r.ink("Black").unwrap()]);
    assert_eq!(k.len(), 100 * 100 * 3);
    assert_eq!(&k[..3], &[255, 255, 255]);
    assert_eq!(&k[99 * 300..99 * 300 + 3], &[128, 128, 128]);
    let all: Vec<usize> = (0..r.inks.len()).collect();
    let both = r.composite(&all);
    // Gold previews as its CMYK alternate 0 20 100 0.
    assert_eq!(&both[..3], &[255, 204, 0]);
}

#[test]
fn bad_arguments_are_refused() {
    let d = pdf("<< >>", &[b""], &[], "");
    for dpi in [0.0, 8.9, 301.0, f64::NAN, f64::INFINITY] {
        assert!(rasterize(&d, 0, dpi, true, &BuiltIn).is_err(), "{dpi}");
    }
    assert!(rasterize(&d, 1, 72.0, true, &BuiltIn).unwrap_err().contains("does not exist"));
    assert!(list_inks(&d, &[3], &BuiltIn).unwrap_err().contains("does not exist"));
    assert!(list_inks(&d, &vec![0; MAX_LIST_PAGES + 1], &BuiltIn).is_err());
}

#[test]
fn hostile_inks_and_resources_do_not_panic() {
    // 80 distinct spot colours: the first ones are tracked, the rest counted.
    let mut spaces = String::new();
    let mut content = String::new();
    for i in 0..80 {
        spaces.push_str(&format!("/S{i} [/Separation /Spot{i} /DeviceGray 99 0 R] "));
        content.push_str(&format!("/S{i} cs 1 scn {i} 0 1 1 re f "));
    }
    let huge_devicen: Vec<String> = (0..1000).map(|i| format!("/N{i}")).collect();
    spaces.push_str(&format!("/Big [/DeviceN [{}] /DeviceCMYK 99 0 R] ", huge_devicen.join(" ")));
    spaces
        .push_str("/Bad [/Separation 5 /Nope] /Worse [/DeviceN 7] /Idx [/Indexed /Gold 1 <ff00>] /Gold [/Separation /Gold [/ICCBased 6 0 R] 6 0 R] ");
    content
        .push_str("/Big cs 1 scn 0 0 5 5 re f /Bad cs 1 scn /Worse cs /Idx cs 0 sc 0 0 9 9 re f /Gold cs 1e308 scn 0 0 1 1 re f /Nope gs /Loop Do ");
    let form = stream("/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources 5 0 R", b"/Loop Do 1 1 1 1 k 0 0 1 1 re f");
    let resources = format!("<< /ColorSpace << {spaces} >> /XObject << /Loop 6 0 R >> /ExtGState << /Nope 1 >> >>");
    let res_obj = resources.clone().into_bytes();
    let d = pdf(&resources, &[content.as_bytes()], &[res_obj, form], "");
    let r = rasterize(&d, 0, 300.0, true, &BuiltIn).unwrap();
    assert!(r.inks.len() <= MAX_INKS);
    assert!(r.unmeasured.contains_key("inks beyond the tracked limit"), "{:?}", r.unmeasured);
    // Many plates share the cell budget: a lower resolution.
    assert!(r.dpi < 300.0);
    assert!(r.width * r.height * r.inks.len() <= crate::coverage::MAX_PLATE_CELLS, "{} x {}", r.width * r.height, r.inks.len());
    let s = at(&r, 0.5, 0.5);
    assert!(s.total.is_finite());
    assert!(list_inks(&d, &[], &BuiltIn).is_ok());
    let soft = pdf(SPOTS, &[b"/Soft gs 1 1 1 1 k 0 0 10 10 re f"], &[], "");
    assert_eq!(raster(&soft, 0).approximated.get("transparency painted as opaque"), Some(&1));
}

#[test]
fn the_output_intent_is_found() {
    let profile = stream("/N 4", b"not really a profile");
    let d = pdf(
        "<< >>",
        &[b""],
        &[profile],
        "/OutputIntents [<< /Type /OutputIntent /S /GTS_PDFA1 >> << /Type /OutputIntent /S /GTS_PDFX /OutputConditionIdentifier (FOGRA39) /DestOutputProfile 5 0 R >>]",
    );
    let oi = output_intent(&d).unwrap();
    assert_eq!(oi.subtype, "GTS_PDFX");
    assert_eq!(oi.condition.as_deref(), Some("FOGRA39"));
    assert_eq!(oi.profile.as_deref(), Some(b"not really a profile".as_slice()));
    assert!(output_intent(&pdf("<< >>", &[b""], &[], "")).is_none());
}

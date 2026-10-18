use std::sync::Arc;

use pdfcraft_content::parse;
use pdfcraft_cos::{Document, Object, SaveOptions, write_incremental};

use super::*;

/// A document from object bodies (object 1 is the catalog). `{len:...}` isn't needed: streams
/// are written with their `/Length`.
fn build(objs: &[String]) -> Document {
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
    Document::open(Arc::new(out)).unwrap()
}

fn stream(dict: &str, data: &str) -> String {
    format!("<< {dict} /Length {} >>\nstream\n{data}\nendstream", data.len())
}

fn binary_stream(dict: &str, data: &[u8]) -> String {
    // Hex-encoded so the fixture stays text.
    let hex: String = data.iter().map(|b| format!("{b:02X}")).collect::<String>() + ">";
    format!("<< {dict} /Filter /ASCIIHexDecode /Length {} >>\nstream\n{hex}\nendstream", hex.len())
}

/// One page per content string, sharing resources `res`, plus `extra` objects from number
/// `3 + 2 * pages`.
fn doc_with(res: &str, contents: &[&str], extra: &[String]) -> Document {
    let n = contents.len();
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + 2 * i)).collect();
    let mut objs = vec!["<< /Type /Catalog /Pages 2 0 R >>".to_string(), format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" "))];
    for (i, c) in contents.iter().enumerate() {
        objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources {res} /Contents {} 0 R >>", 4 + 2 * i));
        objs.push(stream("", c));
    }
    objs.extend(extra.iter().cloned());
    build(&objs)
}

fn content(doc: &Document, page: usize) -> String {
    let p = &pdfcraft_model::pages(doc)[page];
    let streams = content_streams(doc, &p.dict);
    String::from_utf8(streams.iter().flat_map(|(_, d)| d.clone()).collect()).unwrap()
}

fn object(doc: &Document, n: u32) -> Object {
    (*doc.get(ObjRef::new(n, 0))).clone()
}

fn form_content(doc: &Document, n: u32) -> String {
    match object(doc, n) {
        Object::Stream(s) => String::from_utf8(s.decoded().unwrap()).unwrap(),
        o => panic!("object {n} is {o:?}"),
    }
}

/// The fill and stroke colour (as written) at each painting operator.
fn paints(content: &str) -> Vec<(String, String, String)> {
    let mut fill = "g 0".to_string();
    let mut stroke = "g 0".to_string();
    let mut fill_cs = String::new();
    let mut stroke_cs = String::new();
    let mut stack = Vec::new();
    let mut out = Vec::new();
    let nums = |op: &Op| {
        op.operands
            .iter()
            .map(|o| match o {
                Object::Int(i) => i.to_string(),
                Object::Real(r) => format!("{r}"),
                Object::Name(n) => format!("/{}", String::from_utf8_lossy(n)),
                _ => "?".into(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    for op in parse(content.as_bytes()).ops {
        let k = op_name(&op).to_string();
        match k.as_str() {
            "q" => stack.push((fill.clone(), stroke.clone(), fill_cs.clone(), stroke_cs.clone())),
            "Q" => {
                if let Some(s) = stack.pop() {
                    (fill, stroke, fill_cs, stroke_cs) = s;
                }
            }
            "g" | "rg" | "k" => fill = format!("{k} {}", nums(&op)),
            "G" | "RG" | "K" => stroke = format!("{} {}", k.to_lowercase(), nums(&op)),
            "cs" => {
                fill_cs = nums(&op);
                fill = format!("{fill_cs} initial");
            }
            "CS" => {
                stroke_cs = nums(&op);
                stroke = format!("{stroke_cs} initial");
            }
            "sc" | "scn" => fill = format!("{fill_cs} {}", nums(&op)),
            "SC" | "SCN" => stroke = format!("{stroke_cs} {}", nums(&op)),
            "f" | "F" | "f*" | "S" | "s" | "B" | "B*" | "b" | "b*" | "Tj" | "TJ" | "Do" | "BI" | "sh" => out.push((k, fill.clone(), stroke.clone())),
            _ => {}
        }
    }
    out
}

fn cmyk() -> ColorConversion {
    ColorConversion::default()
}

#[test]
fn rgb_fill_and_stroke_become_cmyk_with_black_preserved() {
    let mut doc = doc_with("<< >>", &["1 0 0 rg 0 0 1 RG 10 10 50 50 re B 0 0 0 rg 0 0 10 10 re f 0.5 0.5 0.5 rg 5 5 m 20 20 l S"], &[]);
    let r = convert_colors(&mut doc, &[0], &cmyk()).unwrap();
    assert_eq!(r.pages, vec![0]);
    assert!(r.approximate);
    let c = content(&doc, 0);
    assert!(!c.contains("rg") && !c.contains("RG"), "{c}");
    let p = paints(&c);
    assert_eq!(p[0], ("B".into(), "k 0 1 1 0".into(), "k 1 1 0 0".into()), "{c}");
    assert_eq!(p[1].1, "k 0 0 0 1", "pure black is 100% K: {c}");
    // The stroke at the end paints in the stroke colour, which was set once: still blue.
    assert_eq!(p[2].2, "k 1 1 0 0");
    // The colour is set before the path, not inside it.
    assert!(c.contains("0 1 1 0 k\n1 1 0 0 K\n10 10 50 50 re"), "{c}");

    // Without black preservation, RGB black is a rich black.
    let mut doc = doc_with("<< >>", &["0 0 0 rg 0 0 10 10 re f"], &[]);
    convert_colors(&mut doc, &[0], &ColorConversion { preserve_black: false, ..cmyk() }).unwrap();
    assert_eq!(paints(&content(&doc, 0))[0].1, "k 0.3 0.3 0.3 1");
}

#[test]
fn gray_goes_to_the_black_plate_including_the_default_colour() {
    let mut doc = doc_with(
        "<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>",
        &["BT /F1 12 Tf 10 10 Td (Default black) Tj ET 0.25 g 0 0 10 10 re f /DeviceGray cs 0.5 sc 0 0 5 5 re f"],
        &[],
    );
    convert_colors(&mut doc, &[0], &cmyk()).unwrap();
    let c = content(&doc, 0);
    let p = paints(&c);
    assert_eq!(p[0], ("Tj".into(), "k 0 0 0 1".into(), "g 0".into()), "{c}");
    assert_eq!(p[1].1, "k 0 0 0 0.75");
    assert_eq!(p[2].1, "k 0 0 0 0.5");
    assert!(!c.contains(" g\n") && !c.contains(" sc"), "{c}");
}

#[test]
fn only_the_chosen_object_types_and_spaces_change() {
    let res = "<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>";
    let page = "1 0 0 rg BT /F1 12 Tf 10 10 Td (Red text) Tj ET 0 0 50 50 re f 0.2 g 0 0 5 5 re f";
    let mut doc = doc_with(res, &[page], &[]);
    let text_only = ColorConversion { objects: ObjectKinds { text: true, vector: false, images: false }, ..cmyk() };
    convert_colors(&mut doc, &[0], &text_only).unwrap();
    let p = paints(&content(&doc, 0));
    assert_eq!(p[0].1, "k 0 1 1 0", "text converted");
    assert_eq!(p[1].1, "rg 1 0 0", "the rectangle in the same colour stays RGB");
    assert_eq!(p[2].1, "g 0.2", "gray vector art stays gray");

    // Only DeviceGray: the RGB stays.
    let mut doc = doc_with(res, &[page], &[]);
    let gray_only = ColorConversion { sources: SourceSpaces { device_gray: true, ..SourceSpaces::NONE }, ..cmyk() };
    convert_colors(&mut doc, &[0], &gray_only).unwrap();
    let p = paints(&content(&doc, 0));
    assert_eq!((p[0].1.as_str(), p[1].1.as_str(), p[2].1.as_str()), ("rg 1 0 0", "rg 1 0 0", "k 0 0 0 0.8"));
}

#[test]
fn rgb_images_become_gray() {
    // 2×1 RGB: red, white.
    let img =
        binary_stream("/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8", &[255, 0, 0, 255, 255, 255]);
    let mut doc = doc_with("<< /XObject << /Im1 5 0 R >> >>", &["q 20 0 0 10 0 0 cm /Im1 Do Q"], &[img]);
    let r = convert_colors(&mut doc, &[0], &ColorConversion { destination: Destination::DeviceGray, ..cmyk() }).unwrap();
    assert_eq!(r.images, 1);
    let Object::Stream(s) = object(&doc, 5) else { panic!() };
    assert_eq!(s.dict.name(b"ColorSpace"), Some(b"DeviceGray".as_slice()));
    assert_eq!(s.dict.int(b"BitsPerComponent"), Some(8));
    assert_eq!(s.decoded().unwrap(), vec![to_byte(0.299), 255]);
    // The content doesn't change for an image.
    assert_eq!(content(&doc, 0), "q 20 0 0 10 0 0 cm /Im1 Do Q");
}

#[test]
fn indexed_images_get_a_converted_palette_and_low_bit_images_convert() {
    let pal = binary_stream(
        "/Type /XObject /Subtype /Image /Width 4 /Height 1 /ColorSpace [/Indexed /DeviceRGB 1 <FF0000000000>] /BitsPerComponent 1",
        &[0b0101_0000],
    );
    let gray4 =
        binary_stream("/Type /XObject /Subtype /Image /Width 3 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 4 /Decode [1 0]", &[0x0F, 0x80]);
    let mut doc = doc_with("<< /XObject << /P 5 0 R /G 6 0 R >> >>", &["/P Do /G Do"], &[pal, gray4]);
    let r = convert_colors(&mut doc, &[0], &cmyk()).unwrap();
    assert_eq!(r.images, 2);
    let Object::Stream(p) = object(&doc, 5) else { panic!() };
    let cs = p.dict.get(b"ColorSpace").and_then(Object::as_array).unwrap().clone();
    assert_eq!(cs[1], Object::name("DeviceCMYK"));
    assert_eq!(cs[3], Object::String(PdfString { bytes: vec![0, 255, 255, 0, 0, 0, 0, 255], hex: true }));
    assert_eq!(p.decoded().unwrap(), vec![0b0101_0000], "indices kept");
    let Object::Stream(g) = object(&doc, 6) else { panic!() };
    // Decode [1 0] inverts: sample 0 is white (K 0), 15 is black (K 100%), 8 is gray 0.467 (K 53%).
    assert_eq!(g.decoded().unwrap(), vec![0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 0, 136]);
    assert!(g.dict.get(b"Decode").is_none());
}

#[test]
fn spot_colours_are_kept_or_converted_to_process() {
    let res = "<< /ColorSpace << /Spot [/Separation /PANTONE#20Orange /DeviceRGB << /FunctionType 2 /Domain [0 1] /C0 [1 1 1] /C1 [1 0.5 0] /N 1 >>] \
        /Reg [/Separation /All /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [1 1 1 1] /N 1 >>] >> >>";
    let page = "/Spot cs 1 scn 0 0 10 10 re f /Reg cs 1 scn 0 0 1 1 re f";
    let mut doc = doc_with(res, &[page], &[]);
    let r = convert_colors(&mut doc, &[0], &cmyk()).unwrap();
    assert!(!r.changed(), "{r:?}");
    assert_eq!(content(&doc, 0), page);

    let mut doc = doc_with(res, &[page], &[]);
    convert_colors(&mut doc, &[0], &ColorConversion { convert_spots: true, ..cmyk() }).unwrap();
    let p = paints(&content(&doc, 0));
    // Orange through its RGB alternate: (1, 0.5, 0) → C 0, M 0.5, Y 1, K 0.
    assert_eq!(p[0].1, "k 0 0.5 1 0");
    assert_eq!(p[1].1, "/Reg 1", "registration colour is never converted");
}

#[test]
fn devicen_with_a_postscript_tint_transform_converts() {
    let tint = stream("/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1]", "{ 0 0 4 2 roll }");
    let res = "<< /ColorSpace << /Duo [/DeviceN [/Spot1 /Spot2] /DeviceCMYK 5 0 R] >> >>";
    let mut doc = doc_with(res, &["/Duo cs 0.25 0.75 scn 0 0 1 1 re f"], &[tint]);
    convert_colors(&mut doc, &[0], &ColorConversion { convert_spots: true, destination: Destination::DeviceRgb, ..cmyk() }).unwrap();
    // C 0 M 0 Y 0.25 K 0.75 → RGB (0.25, 0.25, 0.1875).
    assert_eq!(paints(&content(&doc, 0))[0].1, "rg 0.25 0.25 0.1875");
}

#[test]
fn a_form_shared_by_pages_is_converted_once_and_takes_the_callers_colour() {
    let form = stream("/Type /XObject /Subtype /Form /BBox [0 0 10 10]", "0 1 0 rg 0 0 5 5 re f 0 0 10 10 re S");
    let mut doc = doc_with("<< /XObject << /Fm 7 0 R >> >>", &["1 0 0 RG /Fm Do", "/Fm Do"], &[form]);
    let r = convert_colors(&mut doc, &[0, 1], &cmyk()).unwrap();
    assert_eq!(r.forms, 1, "{r:?}");
    let f = form_content(&doc, 7);
    let p = paints(&f);
    assert_eq!(p[0].1, "k 1 0 1 0", "{f}");
    // The stroke uses the inherited colour: the caller sets it converted before Do.
    assert_eq!(p[1].2, "g 0");
    assert_eq!(paints(&content(&doc, 0))[0].2, "k 0 1 1 0");
    assert_eq!(paints(&content(&doc, 1))[0], ("Do".into(), "k 0 0 0 1".into(), "k 0 0 0 1".into()));
    // Only one form object: nothing was copied.
    assert!(matches!(object(&doc, 7), Object::Stream(_)));
}

#[test]
fn looping_forms_stop() {
    let a = stream("/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << /XObject << /B 6 0 R >> >>", "1 0 0 rg /B Do 0 0 1 1 re f");
    let b = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << /XObject << /A 5 0 R /B 6 0 R >> >>",
        "0 0 1 rg /A Do /B Do 0 0 1 1 re f",
    );
    let mut doc = doc_with("<< /XObject << /A 5 0 R >> >>", &["/A Do"], &[a, b]);
    let r = convert_colors(&mut doc, &[0], &cmyk()).unwrap();
    assert_eq!(r.forms, 2);
    assert!(form_content(&doc, 5).contains("0 1 1 0 k"));
    assert!(form_content(&doc, 6).contains("1 1 0 0 k"));
}

#[test]
fn only_the_chosen_pages_change() {
    let mut doc = doc_with("<< >>", &["1 0 0 rg 0 0 1 1 re f", "1 0 0 rg 0 0 1 1 re f", "1 0 0 rg 0 0 1 1 re f"], &[]);
    let r = convert_colors(&mut doc, &[1], &cmyk()).unwrap();
    assert_eq!(r.pages, vec![1]);
    assert_eq!(content(&doc, 0), "1 0 0 rg 0 0 1 1 re f");
    assert!(content(&doc, 1).contains(" k\n"));
    assert_eq!(content(&doc, 2), "1 0 0 rg 0 0 1 1 re f");
    assert!(matches!(convert_colors(&mut doc, &[3], &cmyk()), Err(EditError::NoSuchPage(3))));
}

#[test]
fn shadings_patterns_inline_images_and_appearances_convert() {
    let shading = "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> /Extend [true true] >>";
    let pattern = stream(
        "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 4 4] /XStep 4 /YStep 4 /Resources << >>",
        "0 1 0 rg 0 0 2 2 re f",
    );
    let ap = stream("/Type /XObject /Subtype /Form /BBox [0 0 10 10]", "0 0 1 rg 0 0 10 10 re f");
    let annot = "<< /Type /Annot /Subtype /Square /Rect [0 0 10 10] /AP << /N 8 0 R >> >>".to_string();
    let objs = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Shading << /Sh 5 0 R >> /Pattern << /Tile 6 0 R >> >> /Contents 4 0 R /Annots [7 0 R] /Group << /S /Transparency /CS /DeviceRGB >> >>".into(),
        stream("", "/Sh sh /Pattern cs /Tile scn 0 0 50 50 re f BI /W 2 /H 1 /CS /RGB /BPC 8 /F /AHx ID 00FF00FFFFFF> EI"),
        shading.into(),
        pattern,
        annot,
        ap,
    ];
    let mut doc = build(&objs);
    let r = convert_colors(&mut doc, &[0], &cmyk()).unwrap();
    assert_eq!((r.shadings, r.patterns, r.inline_images, r.annotations), (1, 1, 1, 1), "{r:?}");
    let sh = doc.dict(&object(&doc, 5)).unwrap();
    assert_eq!(sh.name(b"ColorSpace"), Some(b"DeviceCMYK".as_slice()));
    let Some(Object::Ref(f)) = sh.get(b"Function") else { panic!("{sh:?}") };
    let func = Func::parse(&doc, &Object::Ref(*f), 0).unwrap();
    let start = func.eval(&[0.0]).unwrap();
    assert!((start[1] - 1.0).abs() < 1e-3 && start[0].abs() < 1e-3, "red at the start: {start:?}");
    assert!(form_content(&doc, 6).contains("1 0 1 0 k"));
    assert!(form_content(&doc, 8).contains("1 1 0 0 k"));
    let c = content(&doc, 0);
    assert!(c.contains("/CS /CMYK") && c.contains("/F /AHx") && c.contains("FF00FF0000000000"), "{c}");
    let page = doc.dict(&Object::Ref(pdfcraft_model::pages(&doc)[0].obj)).unwrap();
    assert_eq!(doc.dict(page.get(b"Group").unwrap()).unwrap().name(b"CS"), Some(b"DeviceCMYK".as_slice()));
}

/// A CMYK output profile (lut16, Lab PCS) whose tables print everything on black:
/// K = 1 − L*/100.
fn black_only_profile() -> Vec<u8> {
    let grid = 9usize;
    let mut lut = b"mft2\0\0\0\0".to_vec();
    lut.extend([3u8, 4, grid as u8, 0]);
    for v in [1i32, 0, 0, 0, 1, 0, 0, 0, 1] {
        lut.extend((v << 16).to_be_bytes());
    }
    lut.extend(2u16.to_be_bytes());
    lut.extend(2u16.to_be_bytes());
    for _ in 0..3 {
        lut.extend(0u16.to_be_bytes());
        lut.extend(65535u16.to_be_bytes());
    }
    for l in 0..grid {
        let ll = (l as f64 / (grid - 1) as f64) * 65535.0 / 652.8;
        let k = (1.0 - ll / 100.0).clamp(0.0, 1.0);
        for _ in 0..grid * grid {
            for v in [0.0, 0.0, 0.0, k] {
                lut.extend(((v * 65535.0f64).round() as u16).to_be_bytes());
            }
        }
    }
    for _ in 0..4 {
        lut.extend(0u16.to_be_bytes());
        lut.extend(65535u16.to_be_bytes());
    }
    let mut desc = b"desc\0\0\0\0".to_vec();
    let text = b"Black only test\0";
    desc.extend((text.len() as u32).to_be_bytes());
    desc.extend(text);
    desc.extend([0u8; 79]);
    profile(b"prtr", b"CMYK", b"Lab ", &[(b"desc", desc), (b"B2A0", lut.clone()), (b"B2A1", lut)])
}

fn profile(class: &[u8; 4], space: &[u8; 4], pcs: &[u8; 4], tags: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut table = (tags.len() as u32).to_be_bytes().to_vec();
    let mut data = Vec::new();
    let mut offset = 128 + 4 + tags.len() * 12;
    for (sig, body) in tags {
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
    h.extend(class);
    h.extend(space);
    h.extend(pcs);
    h.extend([0u8; 12]);
    h.extend(b"acsp");
    h.resize(128, 0);
    h.extend(table);
    h.extend(data);
    h
}

#[test]
fn an_icc_destination_converts_through_its_tables_and_becomes_the_output_intent() {
    let bytes = Arc::new(black_only_profile());
    let settings = ColorConversion { destination: Destination::Icc(bytes.clone()), output_intent: true, ..cmyk() };
    let mut doc = doc_with("<< >>", &["1 1 1 rg 0 0 1 1 re f 0.5 0.5 0.5 rg 0 0 1 1 re f 0 0 0 1 k 0 0 2 2 re f"], &[]);
    let r = convert_colors(&mut doc, &[0], &settings).unwrap();
    assert!(!r.approximate && r.output_intent, "{r:?}");
    let c = content(&doc, 0);
    let p = paints(&c);
    let values = |s: &str| -> Vec<f64> { s.split(' ').skip(1).map(|v| v.parse().unwrap()).collect() };
    let white = values(&p[0].1);
    assert!(p[0].1.starts_with("/PCcs0 0 0 0 ") && white[3] < 0.01, "white prints (almost) nothing: {c}");
    let gray = values(&p[1].1);
    // sRGB 50% gray is L* ≈ 53.4.
    assert!(gray[..3].iter().all(|v| *v == 0.0) && (gray[3] - 0.466).abs() < 0.02, "{c}");
    assert_eq!(p[2].1, "/PCcs0 0 0 0 1", "DeviceCMYK keeps its numbers in the profile's space");
    // Resources name the profile; the catalog lists it as the PDF/X output intent.
    let res = doc.dict(pdfcraft_model::pages(&doc)[0].dict.get(b"Resources").unwrap()).unwrap();
    let cs = doc.dict(res.get(b"ColorSpace").unwrap()).unwrap();
    let Some(Object::Array(icc)) = cs.get(b"PCcs0") else { panic!("{cs:?}") };
    let Some(Object::Ref(profile_ref)) = icc.get(1) else { panic!() };
    let catalog = doc.dict(&Object::Ref(doc.root().unwrap())).unwrap();
    let intents = catalog.get(b"OutputIntents").and_then(Object::as_array).unwrap();
    let oi = doc.dict(&intents[0]).unwrap();
    assert_eq!(oi.name(b"S"), Some(b"GTS_PDFX".as_slice()));
    assert_eq!(oi.get(b"DestOutputProfile"), Some(&Object::Ref(*profile_ref)));
    assert_eq!(oi.get(b"OutputConditionIdentifier").and_then(Object::as_string).map(|s| s.bytes.clone()), Some(b"Black only test".to_vec()));
    // Converting again finds the colours already in the profile's space.
    let again = convert_colors(&mut doc, &[0], &ColorConversion { output_intent: false, ..settings.clone() }).unwrap();
    assert_eq!(again.colors, 0, "{again:?}");
}

#[test]
fn device_output_converts_through_the_profile_but_writes_device_cmyk() {
    let bytes = Arc::new(black_only_profile());
    let settings = ColorConversion { destination: Destination::Icc(bytes), device_output: true, ..cmyk() };
    let mut doc = doc_with("<< >>", &["0.5 0.5 0.5 rg 0 0 1 1 re f 0 0 0 1 k 0 0 2 2 re f"], &[]);
    let r = convert_colors(&mut doc, &[0], &settings).unwrap();
    assert!(!r.approximate, "{r:?}");
    let c = content(&doc, 0);
    let p = paints(&c);
    assert!(p[0].1.starts_with("k 0 0 0 0.4"), "the profile's numbers, as DeviceCMYK: {c}");
    assert_eq!(p[1].1, "k 0 0 0 1", "DeviceCMYK stays as it was: {c}");
    let res = doc.dict(pdfcraft_model::pages(&doc)[0].dict.get(b"Resources").unwrap()).unwrap();
    assert!(res.get(b"ColorSpace").is_none(), "no ICCBased space is added: {res:?}");
}

#[test]
fn conversion_survives_save_and_reopen() {
    let img = binary_stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8", &[0, 0, 255]);
    let mut doc = doc_with("<< /XObject << /Im 5 0 R >> >>", &["1 0 0 rg 0 0 1 1 re f /Im Do"], &[img]);
    convert_colors(&mut doc, &[0], &cmyk()).unwrap();
    let bytes = write_incremental(&doc, &SaveOptions::default()).unwrap();
    hayro_syntax::Pdf::new(bytes.clone()).expect("parses");
    let again = Document::open(Arc::new(bytes)).unwrap();
    assert_eq!(paints(&content(&again, 0))[0].1, "k 0 1 1 0");
    let Object::Stream(s) = object(&again, 5) else { panic!() };
    assert_eq!(s.dict.name(b"ColorSpace"), Some(b"DeviceCMYK".as_slice()));
    assert_eq!(s.decoded().unwrap(), vec![255, 255, 0, 0]);
}

#[test]
fn bad_settings_are_refused() {
    let mut doc = doc_with("<< >>", &["1 0 0 rg 0 0 1 1 re f"], &[]);
    let before = content(&doc, 0);
    for bad in [
        ColorConversion { sources: SourceSpaces::NONE, ..cmyk() },
        ColorConversion { objects: ObjectKinds { text: false, vector: false, images: false }, ..cmyk() },
        ColorConversion { output_intent: true, ..cmyk() },
        ColorConversion { destination: Destination::Icc(Arc::new(b"not a profile".to_vec())), ..cmyk() },
        ColorConversion { destination: Destination::Icc(Arc::new(profile(b"link", b"CMYK", b"Lab ", &[]))), ..cmyk() },
        ColorConversion { destination: Destination::Icc(Arc::new(profile(b"prtr", b"XCLR", b"Lab ", &[]))), ..cmyk() },
        ColorConversion { output_condition: Some("a\nb".into()), ..cmyk() },
    ] {
        assert!(convert_colors(&mut doc, &[0], &bad).is_err(), "{bad:?}");
    }
    assert_eq!(content(&doc, 0), before);
}

#[test]
fn hostile_content_does_not_panic() {
    let huge = binary_stream("/Type /XObject /Subtype /Image /Width 100000 /Height 100000 /ColorSpace /DeviceRGB /BitsPerComponent 8", &[1, 2, 3]);
    let short = binary_stream("/Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceRGB /BitsPerComponent 8", &[1, 2, 3]);
    let odd =
        binary_stream("/Type /XObject /Subtype /Image /Width -3 /Height 1 /ColorSpace [/Indexed /DeviceRGB 300 <00>] /BitsPerComponent 7", &[1]);
    let bad_fn = stream("/FunctionType 4 /Domain [0 1] /Range [0 1 0 1 0 1]", "{ 1 0 div dup dup roll roll roll { { { ");
    let sampled = stream("/FunctionType 0 /Domain [0 1] /Range [0 1 0 1 0 1] /Size [99999999] /BitsPerSample 8", "");
    let res = "<< /XObject << /H 5 0 R /S 6 0 R /O 7 0 R /Self 10 0 R >> /ColorSpace << /X [/Separation /X /DeviceRGB 8 0 R] /Y [/Separation /Y /DeviceRGB 9 0 R] \
        /Loop /Loop /Deep [/Indexed [/Indexed [/Indexed /DeviceRGB 1 <00>] 1 <00>] 1 <00>] /N [/DeviceN [] /DeviceRGB 8 0 R] >> /Shading << /Bad 11 0 R >> >>";
    let page = "q q q Q Q Q Q Q 1 0 0 rg /X cs 1 scn 0 0 1 1 re f /Y cs 0.5 scn 0 0 1 1 re f /Loop cs 1 2 3 4 5 scn f /Deep cs 0 sc S /N CS 1 SCN S \
        1e308 1e308 1e308 rg 0 0 m f NaN g f 1 2 rg f /H Do /S Do /O Do /Self Do /Missing Do /Bad sh 0 Tr (x) Tj 99 Tr (y) Tj BI /W 99999 /H 99999 /CS /RGB ID x EI \
        BI /CS /RGB /W 1 /H 1 /BPC 8 ID \x01 EI ) ] >> {";
    let self_form =
        stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /Self 10 0 R >> >>", "0 1 0 rg /Self Do 0 0 1 1 re f");
    let bad_sh = "<< /ShadingType 4 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 >>".to_string();
    let mut doc = doc_with(res, &[page], &[huge, short, odd, bad_fn, sampled, self_form, bad_sh]);
    for convert_spots in [false, true] {
        let mut d = doc.clone();
        let r = convert_colors(&mut d, &[0], &ColorConversion { convert_spots, ..cmyk() }).unwrap();
        assert!(!r.skipped.is_empty(), "{r:?}");
    }
    let r = convert_colors(&mut doc, &[0, 0], &ColorConversion { destination: Destination::DeviceRgb, ..cmyk() }).unwrap();
    assert_eq!(r.pages, vec![0]);
}

#[test]
fn calibrated_lab_and_icc_sources_convert_and_a_second_pass_finds_nothing() {
    let icc = binary_stream("/N 3", &[0u8; 4]);
    let res = "<< /ColorSpace << /Cal [/CalRGB << /WhitePoint [0.9505 1 1.089] /Gamma [2.2 2.2 2.2] /Matrix [0.4124 0.2126 0.0193 0.3576 0.7152 0.1192 0.1805 0.0722 0.9505] >>] \
        /L [/Lab << /WhitePoint [0.9642 1 0.8249] /Range [-128 127 -128 127] >>] /I [/ICCBased 5 0 R] /CG [/CalGray << /WhitePoint [0.9505 1 1.089] /Gamma 1 >>] >> >>";
    let page = "/Cal cs 1 1 1 sc 0 0 1 1 re f /L cs 100 0 0 sc 0 0 1 1 re f /L cs 0 0 0 sc 0 0 1 1 re f /I cs 1 0 0 sc 0 0 1 1 re f /CG cs 0 sc 0 0 1 1 re f";
    let mut doc = doc_with(res, &[page], &[icc]);
    let r = convert_colors(&mut doc, &[0], &cmyk()).unwrap();
    assert_eq!(r.colors, 5, "{r:?}");
    let p = paints(&content(&doc, 0));
    let k = |i: usize| -> Vec<f64> { p[i].1.split(' ').skip(1).map(|v| v.parse().unwrap()).collect() };
    assert!(k(0).iter().all(|v| v.abs() < 0.01), "CalRGB white: {p:?}");
    assert!(k(1).iter().all(|v| v.abs() < 0.01), "Lab white: {p:?}");
    assert_eq!(p[2].1, "k 0 0 0 1", "Lab black, preserved");
    assert_eq!(p[3].1, "k 0 1 1 0", "3-component ICC read as sRGB");
    assert_eq!(p[4].1, "k 0 0 0 1");
    let again = convert_colors(&mut doc, &[0], &cmyk()).unwrap();
    assert!(!again.changed(), "{again:?}");
}

#[test]
fn an_rgb_matrix_profile_destination_keeps_srgb_colours() {
    let xyz = |x: f64, y: f64, z: f64| {
        let mut t = b"XYZ \0\0\0\0".to_vec();
        for v in [x, y, z] {
            t.extend(((v * 65536.0).round() as i32).to_be_bytes());
        }
        t
    };
    let mut trc = b"curv\0\0\0\0".to_vec();
    trc.extend(1024u32.to_be_bytes());
    for i in 0..1024 {
        let v = i as f64 / 1023.0;
        let lin = if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        trc.extend(((lin * 65535.0).round() as u16).to_be_bytes());
    }
    let p = profile(
        b"mntr",
        b"RGB ",
        b"XYZ ",
        &[
            (b"rXYZ", xyz(0.4361, 0.2225, 0.0139)),
            (b"gXYZ", xyz(0.3851, 0.7169, 0.0971)),
            (b"bXYZ", xyz(0.1431, 0.0606, 0.7141)),
            (b"rTRC", trc.clone()),
            (b"gTRC", trc.clone()),
            (b"bTRC", trc),
        ],
    );
    let mut doc = doc_with("<< >>", &["0.2 0.4 0.6 rg 0 0 1 1 re f 0 0 0 1 k 0 0 1 1 re f"], &[]);
    let r = convert_colors(&mut doc, &[0], &ColorConversion { destination: Destination::Icc(Arc::new(p)), ..cmyk() }).unwrap();
    assert!(!r.approximate);
    let paints = paints(&content(&doc, 0));
    let v: Vec<f64> = paints[0].1.split(' ').skip(1).map(|v| v.parse().unwrap()).collect();
    for (a, b) in v.iter().zip([0.2, 0.4, 0.6]) {
        assert!((a - b).abs() < 0.01, "{paints:?}");
    }
    assert_eq!(paints[1].1, "/PCcs0 0 0 0", "K-only black is black");
}

#[test]
fn jpeg_images_are_decoded_and_re_encoded() {
    let mut jpg = Vec::new();
    let px: Vec<u8> = (0..16 * 16).flat_map(|_| [200u8, 30, 30]).collect();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 95).encode(&px, 16, 16, image::ExtendedColorType::Rgb8).unwrap();
    let jpeg_image = || {
        let mut d = Dict::new();
        for (k, v) in [("Type", Object::name("XObject")), ("Subtype", Object::name("Image")), ("Width", Object::Int(16)), ("Height", Object::Int(16))]
        {
            d.set(k.as_bytes().to_vec(), v);
        }
        d.set(b"ColorSpace".to_vec(), Object::name("DeviceRGB"));
        d.set(b"BitsPerComponent".to_vec(), Object::Int(8));
        d.set(b"Filter".to_vec(), Object::name("DCTDecode"));
        Object::Stream(Stream::from_raw(d, jpg.clone()))
    };
    let mut doc = doc_with("<< /XObject << /J 5 0 R >> >>", &["/J Do"], &[]);
    doc.set(ObjRef::new(5, 0), jpeg_image());
    let gray = ColorConversion { destination: Destination::DeviceGray, ..cmyk() };
    assert_eq!(convert_colors(&mut doc.clone(), &[0], &gray).unwrap().images, 1);
    let r = convert_colors(&mut doc, &[0], &gray).unwrap();
    assert_eq!(r.images, 1);
    let Object::Stream(s) = object(&doc, 5) else { panic!() };
    assert_eq!((s.dict.name(b"Filter"), s.dict.name(b"ColorSpace")), (Some(b"DCTDecode".as_slice()), Some(b"DeviceGray".as_slice())));
    let back = image::load_from_memory_with_format(&s.raw, image::ImageFormat::Jpeg).unwrap().into_luma8();
    let expect = (0.299 * 200.0 + 0.587 * 30.0 + 0.114 * 30.0) as i32;
    assert!((i32::from(back.get_pixel(8, 8)[0]) - expect).abs() < 6, "{:?}", back.get_pixel(8, 8));
    // To CMYK the JPEG becomes Flate (baseline JPEG has no CMYK here).
    let mut doc2 = doc_with("<< /XObject << /J 5 0 R >> >>", &["/J Do"], &[]);
    doc2.set(ObjRef::new(5, 0), jpeg_image());
    convert_colors(&mut doc2, &[0], &cmyk()).unwrap();
    let Object::Stream(s) = object(&doc2, 5) else { panic!() };
    assert_eq!(s.dict.name(b"Filter"), Some(b"FlateDecode".as_slice()));
    assert_eq!(s.decoded().unwrap().len(), 16 * 16 * 4);
}

#[test]
fn a_cmyk_profile_separates_srgb_and_lab_colours() {
    let p = Profile::parse(&black_only_profile(), 1).unwrap();
    let black = p.cmyk_from_srgb([0.0, 0.0, 0.0]).unwrap();
    assert!(black[..3].iter().all(|v| *v == 0.0) && (black[3] - 1.0).abs() < 0.01, "{black:?}");
    let white = p.cmyk_from_srgb([1.0, 1.0, 1.0]).unwrap();
    assert!(white[3] < 0.01, "{white:?}");
    let mid = p.cmyk_from_lab([50.0, 0.0, 0.0]).unwrap();
    assert!((mid[3] - 0.5).abs() < 0.02, "{mid:?}");
    assert!(p.cmyk_from_lab([f64::NAN, 0.0, 0.0]).is_none());
    assert!(p.cmyk_from_srgb([f64::NAN, 2.0, -1.0]).is_some());
}

#[test]
fn only_the_chosen_spot_inks_convert_and_mixed_devicen_is_kept() {
    let tint = |c1: &str| format!("<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [{c1}] /N 1 >>");
    let res = format!(
        "<< /ColorSpace << /A [/Separation /Orange /DeviceCMYK {}] /B [/Separation /Green /DeviceCMYK {}] /D [/DeviceN [/Orange /Green] /DeviceCMYK 5 0 R] >> >>",
        tint("0 0.5 1 0"),
        tint("1 0 1 0")
    );
    let ps = stream("/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1]", "{ 0 0 4 2 roll }");
    let page = "/A cs 1 scn 0 0 10 10 re f /B cs 1 scn 0 0 1 1 re f /D cs 1 1 scn 0 0 2 2 re f";
    let mut doc = doc_with(&res, &[page], &[ps]);
    let only: BTreeSet<Vec<u8>> = [b"Orange".to_vec()].into_iter().collect();
    let settings = ColorConversion { sources: SourceSpaces::NONE, convert_spots: true, spot_names: Some(only), ..cmyk() };
    let r = convert_colors(&mut doc, &[0], &settings).unwrap();
    let p = paints(&content(&doc, 0));
    assert_eq!(p[0].1, "k 0 0.5 1 0", "{p:?}");
    assert_eq!(p[1].1, "/B 1", "Green is kept: {p:?}");
    assert_eq!(p[2].1, "/D 1 1", "the DeviceN mixes a kept ink: {p:?}");
    assert!(r.skipped.iter().any(|s| s.contains("DeviceN")), "{r:?}");
}

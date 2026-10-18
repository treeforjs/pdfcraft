use std::sync::Arc;

use super::*;
use crate::coverage::BuiltIn;
use crate::ink_manager::{InkManager, Managed};

fn stream(extra: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< /Length {} {extra} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// One 200 × 200 pt page with `content` and `resources` (`page_extra` goes into the page
/// dictionary); `extra` objects are numbered from 5.
fn pdf(resources: &str, content: &[u8], extra: &[Vec<u8>], page_extra: &str) -> Document {
    let mut objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources {resources} /Contents 4 0 R {page_extra} >>").into_bytes(),
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

const RESOURCES: &str = "<< /ColorSpace << \
    /Gold [/Separation /Gold /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.2 1 0] /N 1 >>] >> \
    /ExtGState << /OP << /OP true /op true /OPM 1 >> /Soft << /ca 0.5 /CA 0.25 /BM /Multiply /SMask << /S /Luminosity /G 6 0 R >> >> \
        /Rel << /RI /Saturation >> >> \
    /Font << /F1 5 0 R >> /XObject << /Im1 7 0 R /Fm1 8 0 R >> >>";

/// Objects 5.. for [`RESOURCES`]: a non-embedded Helvetica, a soft mask group, a 4 × 2 RGB
/// image, a form holding a Gold square.
fn extras() -> Vec<Vec<u8>> {
    vec![
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Group << /S /Transparency /CS /DeviceGray >>", b""),
        stream("/Type /XObject /Subtype /Image /Width 4 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Interpolate true", &[0u8; 24]),
        stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 50 50] /Group << /S /Transparency /I true /K false >> \
             /Resources << /ColorSpace << /Gold [/Separation /Gold /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.2 1 0] /N 1 >>] >> >>",
            b"/Gold cs 0.6 scn 0 0 50 50 re f",
        ),
    ]
}

fn inspect_at(d: &Document, x: f64, y: f64) -> Inspection {
    inspect(d, 0, x, y, 72.0, true, &BuiltIn).unwrap()
}

fn ink(c: &ObjectColour, name: &str) -> f64 {
    c.inks.iter().find(|(n, _)| n == name).map(|(_, v)| *v).unwrap_or_else(|| panic!("no {name} in {:?}", c.inks))
}

#[test]
fn stacked_objects_are_listed_topmost_first_with_their_colours() {
    let d = pdf(
        RESOURCES,
        b"0.1 0.2 0.3 0.4 k 0 0 100 100 re f \
          /Gold cs 0.5 scn 20 20 60 60 re f \
          q /OP gs 1 0 0 0 K 0 0 1 rg 4 w 40 10 m 40 90 l S Q \
          1 0 0 rg 150 150 20 20 re f",
        &extras(),
        "",
    );
    let r = inspect_at(&d, 40.0, 50.0);
    let kinds: Vec<ObjectKind> = r.objects.iter().map(|o| o.kind).collect();
    assert_eq!(kinds, vec![ObjectKind::Path; 3], "{:?}", r.objects);
    // The stroke, painted last, is on top.
    let top = &r.objects[0];
    assert_eq!(top.painted, vec!["stroke"]);
    let stroke = top.stroke.as_ref().unwrap();
    assert_eq!(stroke.space, "DeviceCMYK");
    assert_eq!(stroke.components, vec![1.0, 0.0, 0.0, 0.0]);
    assert_eq!(ink(stroke, "Cyan"), 100.0);
    assert_eq!(top.line_width, Some(4.0));
    assert_eq!(top.overprint, Overprint { fill: true, stroke: true, mode: 1 });
    assert!(top.fill.is_none());
    // The spot square, then the CMYK square at the bottom.
    let gold = r.objects[1].fill.as_ref().unwrap();
    assert_eq!(gold.space, "Separation (Gold)");
    assert_eq!(ink(gold, "Gold"), 50.0);
    assert!(!r.objects[1].overprint.fill);
    let base = r.objects[2].fill.as_ref().unwrap();
    assert_eq!((ink(base, "Cyan"), ink(base, "Magenta"), ink(base, "Yellow"), ink(base, "Black")), (10.0, 20.0, 30.0, 40.0));
    assert_eq!(r.objects[2].fill_rule, Some("nonzero"));
    assert_eq!(r.objects[2].bbox, [0.0, 0.0, 100.0, 100.0]);
    assert!(r.objects[0].order > r.objects[1].order && r.objects[1].order > r.objects[2].order);
    // The plates at the point: Cyan 100 overprints (OPM 1) the Gold and the CMYK below it.
    let s = r.sample.as_ref().unwrap();
    let plate = |n: &str| s.inks.iter().find(|(i, _)| i == n).map(|(_, v)| *v).unwrap();
    assert_eq!(plate("Cyan"), 100.0);
    assert_eq!(plate("Gold"), 50.0);
    assert_eq!(plate("Black"), 0.0);
    // Outside every object: nothing listed.
    assert!(inspect_at(&d, 190.0, 10.0).objects.is_empty());
    // The RGB square is converted.
    let red = inspect_at(&d, 160.0, 160.0);
    assert!(red.objects[0].fill.as_ref().unwrap().converted);
}

#[test]
fn transparency_intent_and_fill_rules_are_reported() {
    let d = pdf(RESOURCES, b"q /Soft gs /Rel gs 0 0 0 1 k 0 0 100 100 re 25 25 50 50 re f* Q /Saturation ri 0.5 g 120 0 50 50 re f", &extras(), "");
    let r = inspect_at(&d, 10.0, 10.0);
    let o = &r.objects[0];
    assert_eq!(o.fill_rule, Some("even-odd"));
    assert_eq!(o.transparency.blend_mode, "Multiply");
    assert_eq!((o.transparency.fill_alpha, o.transparency.stroke_alpha), (0.5, 0.25));
    assert_eq!(o.transparency.soft_mask.as_deref(), Some("Luminosity"));
    assert!(o.transparency.is_transparent());
    assert_eq!(o.rendering_intent, "Saturation");
    // The hole of the even-odd fill is empty.
    assert!(inspect_at(&d, 50.0, 50.0).objects.is_empty());
    // After Q the state is back to opaque; ri sets the intent.
    let g = inspect_at(&d, 130.0, 10.0);
    let o = &g.objects[0];
    assert!(!o.transparency.is_transparent());
    assert_eq!(o.rendering_intent, "Saturation");
    assert_eq!(o.fill.as_ref().unwrap().space, "DeviceGray");
    assert_eq!(ink(o.fill.as_ref().unwrap(), "Black"), 50.0);
}

#[test]
fn images_report_size_bits_space_and_effective_resolution() {
    // 4 × 2 pixels drawn 72 × 36 pt: 4 ppi horizontally, 4 vertically; then 2 × 1 inch.
    let d = pdf(RESOURCES, b"q 72 0 0 36 10 10 cm /Im1 Do Q q 144 0 0 72 100 100 cm BI /W 2 /H 2 /BPC 1 /CS /G ID \x00\x00 EI Q", &extras(), "");
    let r = inspect_at(&d, 20.0, 20.0);
    let o = &r.objects[0];
    assert_eq!(o.kind, ObjectKind::Image);
    let im = o.image.as_ref().unwrap();
    assert_eq!(im.name.as_deref(), Some("Im1"));
    assert_eq!((im.width, im.height, im.bits_per_component), (4, 2, 8));
    assert_eq!(im.colour_space, "DeviceRGB");
    assert_eq!(im.ppi, [4.0, 4.0]);
    assert!(im.interpolate && !im.stencil);
    assert_eq!(o.bbox, [10.0, 10.0, 82.0, 46.0]);
    let inline = inspect_at(&d, 150.0, 150.0);
    let im = inline.objects[0].image.as_ref().unwrap();
    assert_eq!(im.name, None);
    assert_eq!(im.colour_space, "DeviceGray");
    assert_eq!(im.ppi, [1.0, 2.0]);
}

#[test]
fn text_reports_its_font_size_and_embedding() {
    let d = pdf(RESOURCES, b"BT /F1 12 Tf 2 0 0 2 20 100 Tm (Hello) Tj ET BT /F1 10 Tf 3 Tr 20 50 Td (Hidden) Tj ET", &extras(), "");
    // Helvetica without Widths falls back to 500/1000 per glyph: 5 glyphs × 6 pt × 2 = 60 pt.
    let r = inspect_at(&d, 40.0, 105.0);
    let o = &r.objects[0];
    assert_eq!(o.kind, ObjectKind::Text);
    let t = o.text.as_ref().unwrap();
    assert_eq!(t.font, "Helvetica");
    assert_eq!(t.resource, "F1");
    assert_eq!(t.font_type, "Type1");
    assert!(!t.embedded && !t.subset && !t.invisible);
    assert_eq!((t.size, t.effective_size), (12.0, 24.0));
    assert_eq!(o.painted, vec!["fill"]);
    // Invisible text (render mode 3) is listed but paints nothing.
    let h = inspect_at(&d, 25.0, 53.0);
    let t = h.objects[0].text.as_ref().unwrap();
    assert!(t.invisible);
    assert_eq!(t.render_mode, 3);
    assert!(h.objects[0].painted.is_empty() && h.objects[0].fill.is_none());
}

#[test]
fn forms_shadings_and_annotations_are_listed_in_order() {
    let resources = format!(
        "{} /Shading << /Sh1 << /ShadingType 2 /ColorSpace /DeviceCMYK /Coords [0 0 1 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [1 0 0 0] /N 1 >> >> >> >>",
        RESOURCES.trim_end_matches(">>")
    );
    let d = pdf(
        &resources,
        b"q 0 0 200 100 re W n /Sh1 sh Q q 1 0 0 1 10 10 cm /Fm1 Do Q",
        &extras(),
        "/Annots [<< /Type /Annot /Subtype /Square /Rect [0 0 30 30] /F 4 /NM (sq1) >>]",
    );
    let r = inspect_at(&d, 20.0, 20.0);
    let kinds: Vec<ObjectKind> = r.objects.iter().map(|o| o.kind).collect();
    assert_eq!(kinds, vec![ObjectKind::Annotation, ObjectKind::Path, ObjectKind::Form, ObjectKind::Shading]);
    let a = r.objects[0].annotation.as_ref().unwrap();
    assert_eq!((a.subtype.as_str(), a.name.as_deref(), a.printed, a.hidden), ("Square", Some("sq1"), true, false));
    assert_eq!(r.objects[1].forms, vec!["Fm1".to_string()]);
    assert_eq!(ink(r.objects[1].fill.as_ref().unwrap(), "Gold"), 60.0);
    let form = r.objects[2].form.as_ref().unwrap();
    assert_eq!(form.name, "Fm1");
    assert_eq!(form.group.as_deref(), Some("Transparency group, isolated"));
    assert_eq!(r.objects[2].bbox, [10.0, 10.0, 60.0, 60.0]);
    let sh = r.objects[3].shading.as_ref().unwrap();
    assert_eq!((sh.name.as_str(), sh.shading_type, sh.description.as_str(), sh.colour_space.as_str()), ("Sh1", Some(2), "axial", "DeviceCMYK"));
    // The shading is clipped to the bottom half.
    let top = inspect_at(&d, 100.0, 150.0);
    assert!(top.objects.is_empty(), "{:?}", top.objects);
}

#[test]
fn ink_manager_routes_are_reflected() {
    let d = pdf(RESOURCES, b"/Gold cs 0.5 scn 0 0 100 100 re f", &extras(), "");
    let mut m = InkManager::default();
    m.to_process.insert("Gold".into());
    let conversion = Managed { conversion: &BuiltIn, inks: &m };
    let r = inspect(&d, 0, 50.0, 50.0, 72.0, true, &conversion).unwrap();
    let fill = r.objects[0].fill.as_ref().unwrap();
    assert_eq!(fill.space, "Separation (Gold)");
    assert_eq!(fill.components, vec![0.5]);
    assert_eq!((ink(fill, "Magenta"), ink(fill, "Yellow")), (10.0, 50.0));
    assert!(fill.converted);
    assert!(r.raster.routes.contains_key("Gold"));
    let s = r.sample.unwrap();
    assert!(s.inks.iter().all(|(n, _)| n != "Gold"));
}

#[test]
fn hostile_input_does_not_panic() {
    // Self-referencing forms, NaN-ish numbers, missing resources, huge images, bad points.
    let d = pdf(
        "<< /XObject << /Loop 5 0 R /Big 6 0 R >> /Font << /F1 7 0 R >> /Shading << /S 8 0 R >> /Pattern << /P 9 0 R >> >>",
        b"q 1e300 0 0 1e300 0 0 cm /Loop Do Q /Loop Do /Big Do /Missing Do /Pattern cs /P scn 0 0 200 200 re f \
          BT /F1 1e30 Tf (x) Tj /F9 12 Tf 99 Tr (y) Tj ET /S sh /Nope sh 0 0 0 0 cm /Big Do 2 w 5 5 m S",
        &[
            stream("/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /XObject << /Loop 5 0 R >> >>", b"0 g 0 0 10 10 re f /Loop Do"),
            stream("/Type /XObject /Subtype /Image /Width 100000000 /Height 100000000 /BitsPerComponent 3 /ColorSpace /Nope /SMask 1 0 R", b""),
            b"<< /Type /Font /Subtype /Type0 /BaseFont /ABCDEF+X /DescendantFonts 5 0 R >>".to_vec(),
            b"<< /ShadingType -4 /BBox [1 2 3] /ColorSpace [/Indexed [/Indexed /DeviceRGB 1 <00>] 1 <00>] >>".to_vec(),
            b"<< /PatternType 2 /Shading 9 0 R >>".to_vec(),
        ],
        "/Annots [<< /Rect [0 0 (x) /y 99999999999999999999999] >> 5 0 R << /Rect [200 200 0 0] /NM 3 >>]",
    );
    for (x, y) in [(5.0, 5.0), (0.0, 0.0), (200.0, 200.0), (-1.0, 1e12), (100.0, 100.0)] {
        let _ = inspect(&d, 0, x, y, 72.0, true, &BuiltIn).unwrap();
    }
    assert!(inspect(&d, 0, f64::NAN, 0.0, 72.0, true, &BuiltIn).is_err());
    assert!(inspect(&d, 3, 0.0, 0.0, 72.0, true, &BuiltIn).is_err());
    assert!(inspect(&d, 0, 0.0, 0.0, 1.0, true, &BuiltIn).is_err());
}

#[test]
fn many_objects_keep_the_topmost() {
    let mut content = Vec::new();
    for _ in 0..(MAX_OBJECTS + 10) {
        content.extend_from_slice(b"0 0 10 10 re f ");
    }
    content.extend_from_slice(b"1 0 0 0 k 0 0 10 10 re f");
    let d = pdf("<< >>", &content, &[], "");
    let r = inspect_at(&d, 5.0, 5.0);
    assert_eq!(r.objects.len(), MAX_OBJECTS);
    assert!(r.objects_truncated);
    assert_eq!(ink(r.objects[0].fill.as_ref().unwrap(), "Cyan"), 100.0);
}

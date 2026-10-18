use std::collections::BTreeMap;
use std::sync::Arc;

use pdfcraft_cos::{Document, ObjRef, Object};

use super::*;

/// A document from object bodies (object 1 is the catalog).
fn build(objs: &[&str]) -> Document {
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

fn map(pairs: &[(&str, &str)]) -> BTreeMap<Vec<u8>, Vec<u8>> {
    pairs.iter().map(|(a, b)| (a.as_bytes().to_vec(), b.as_bytes().to_vec())).collect()
}

fn text(doc: &Document, n: u32) -> String {
    format!("{:?}", doc.get(ObjRef::new(n, 0)))
}

const TINT: &str = "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 1 0] /N 1 >>";

#[test]
fn separations_devicen_and_colorants_are_renamed_and_images_keep_their_data() {
    let res = format!(
        "<< /ColorSpace << /A [/Separation /Red /DeviceCMYK {TINT}] /B [/Indexed [/Separation /Red /DeviceCMYK {TINT}] 1 <00FF>] \
         /D [/DeviceN [/Red /Blue] /DeviceCMYK 6 0 R << /Colorants << /Red [/Separation /Red /DeviceCMYK {TINT}] >> >>] >> /XObject << /Im 5 0 R >> >>"
    );
    let page = format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Resources {res} >>");
    let image = format!(
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace [/Separation /Red /DeviceCMYK {TINT}] /Length 1 >>\nstream\nX\nendstream"
    );
    let tint = "<< /FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1] /Length 14 >>\nstream\n{ 0 0 4 2 roll }\nendstream";
    let mut doc = build(&["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 >>", &page, "<< >>", &image, tint]);
    let r = alias_inks(&mut doc, &map(&[("Red", "Warm Red")])).unwrap();
    assert!(r.changed());
    assert_eq!(r.objects, 2, "the page (direct resources) and the image: {r:?}");
    // Separation (A), Indexed base (B), DeviceN name and its Colorants key and value, the image.
    assert_eq!(r.renamed.get("Red"), Some(&6), "{r:?}");
    let page = text(&doc, 3);
    assert!(!page.contains("\"Red\"") && !page.contains("[82, 101, 100]"), "{page}");
    let Object::Stream(s) = doc.get(ObjRef::new(5, 0)).as_ref().clone() else { panic!() };
    assert_eq!(s.raw.as_ref(), b"X");
    let cs = format!("{:?}", s.dict.get(b"ColorSpace"));
    assert!(cs.contains(&format!("{:?}", b"Warm Red".to_vec())), "{cs}");
    // Applying again changes nothing.
    let again = alias_inks(&mut doc, &map(&[("Red", "Warm Red")])).unwrap();
    assert!(!again.changed(), "{again:?}");
}

#[test]
fn devicen_that_would_repeat_an_ink_is_left_and_reported() {
    let page =
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Resources << /ColorSpace << /D [/DeviceN [/Red /Blue] /DeviceCMYK 4 0 R] >> >> >>";
    let mut doc = build(&["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 >>", page, TINT]);
    let r = alias_inks(&mut doc, &map(&[("Red", "Blue")])).unwrap();
    assert!(!r.changed(), "{r:?}");
    assert_eq!(r.skipped.len(), 1, "{r:?}");
}

#[test]
fn bad_aliases_are_refused() {
    for bad in [
        map(&[("Red", "Red")]),
        map(&[("Cyan", "Red")]),
        map(&[("Red", "All")]),
        map(&[("None", "Red")]),
        map(&[("Red", "Blue"), ("Blue", "Green")]),
        map(&[("", "Blue")]),
    ] {
        assert!(check_aliases(&bad).is_err(), "{bad:?}");
    }
    assert!(check_aliases(&map(&[("Red", "Magenta"), ("Blue", "Magenta")])).is_ok());
}

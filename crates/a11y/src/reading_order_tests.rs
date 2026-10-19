//! The Reading Order tool on synthetic pages, with a text extractor that reads nothing (so only
//! pictures and drawings are located) and hostile trees and content.

use std::sync::Arc;

use pdfcraft_cos::{Document, ObjRef};

use super::*;

fn pdf(bodies: &[String]) -> Document {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, b) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{b}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", bodies.len() + 1).as_bytes());
    Document::open(Arc::new(out)).unwrap()
}

fn stream(data: &str) -> String {
    format!("<< /Length {} >>\nstream\n{data}\nendstream", data.len())
}

/// Two images and a rule (no text), in one tagged Figure (MCID 0) when `tagged`.
fn doc(content: &str, tagged: bool) -> Document {
    let cat = if tagged {
        "<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R >>"
    } else {
        "<< /Type /Catalog /Pages 2 0 R >>"
    };
    pdf(&[
        cat.into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /StructParents 0 /Resources << /XObject << /Im 5 0 R >> >> >>".into(),
        stream(content),
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Length 1 >>\nstream\n\x7f\nendstream"
            .into(),
        "<< /Type /StructTreeRoot /K 7 0 R /ParentTree 9 0 R /ParentTreeNextKey 1 >>".into(),
        "<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R] >>".into(),
        "<< /Type /StructElem /S /Figure /P 7 0 R /Pg 3 0 R /K 0 /Alt (Two dots) >>".into(),
        "<< /Nums [0 [8 0 R]] >>".into(),
    ])
}

const TWO: &str = "/Figure << /MCID 0 >> BDC q 20 0 0 20 20 160 cm /Im Do Q q 20 0 0 20 20 20 cm /Im Do Q EMC";

fn snap(d: &Document) -> (Tags, Vec<u8>) {
    let p = pdfcraft_model::pages(d);
    (tags(d), crate::tags::page_content(d, &p[0].dict))
}

fn no_text(_: &Document, pages: &[usize]) -> Vec<Option<Vec<AutotagGlyph>>> {
    pages.iter().map(|_| Some(Vec::new())).collect()
}

fn req(rect: [f64; 4], role: RegionRole) -> RegionTag {
    RegionTag { page: 0, rect, role, alt: None, artifact_type: None, artifact_subtype: None }
}

#[test]
fn a_figure_cut_by_the_rectangle_is_split_and_the_tree_follows() {
    let mut d = doc(TWO, true);
    // The bottom image (view y 160–180) becomes a second figure.
    let mut r = req([0.0, 150.0, 200.0, 200.0], RegionRole::Figure);
    r.alt = Some("Lower dot".into());
    let res = tag_region(&mut d, &r, &mut no_text).unwrap();
    assert_eq!((res.split, res.moved, res.mcids.clone()), (1, 0, vec![1]));
    let t = tags(&d);
    let figs: Vec<&TagNode> = t.nodes.iter().filter(|n| n.role() == "Figure").collect();
    assert_eq!(figs.len(), 2);
    assert_eq!(figs[0].kids, vec![TagKid::Content { page: Some(0), mcid: 0, stream: None }]);
    assert_eq!(figs[1].alt.as_deref(), Some("Lower dot"));
    let o = page_order(&d, 0, &mut no_text).unwrap();
    assert_eq!(o.regions.len(), 2);
    assert!(o.regions[0].rect.unwrap()[1] < o.regions[1].rect.unwrap()[1], "the top image reads first");
    let report = check(&d, &Options::default());
    assert_eq!(report.result(Rule::TaggedContent).map(|r| r.status), Some(Status::Passed));
    // Move the lower figure first, then clear the page.
    move_element(&mut d, res.elements[0], OrderPlace::Before(figs[0].obj.unwrap())).unwrap();
    let o = page_order(&d, 0, &mut no_text).unwrap();
    assert_eq!(o.regions[0].obj, Some(res.elements[0]));
    let c = clear_page_structure(&mut d, 0, false).unwrap();
    assert_eq!((c.mcids, c.removed), (2, 2));
    assert_eq!(page_order(&d, 0, &mut no_text).unwrap().untagged, 2);
}

#[test]
fn untagged_pages_get_a_tree_and_background_needs_none() {
    let mut d = doc("q 20 0 0 20 20 160 cm /Im Do Q 0 0 m 200 0 l S", false);
    let res = tag_region(&mut d, &req([0.0, 190.0, 200.0, 200.0], RegionRole::Background), &mut no_text).unwrap();
    assert!(!res.created_tree && res.artifacts == 1);
    assert!(!tags(&d).exists);
    let res = tag_region(&mut d, &req([0.0, 0.0, 200.0, 100.0], RegionRole::Figure), &mut no_text).unwrap();
    assert!(res.created_tree);
    let t = tags(&d);
    assert!(t.marked);
    assert_eq!(t.nodes.iter().map(|n| n.role()).collect::<Vec<_>>(), ["Document", "Figure"]);
}

#[test]
fn hostile_requests_and_content_never_panic_and_change_nothing() {
    let cases = [
        TWO.to_string(),
        "/Figure << /MCID 0 >> BDC q 20 0 0 20 20 160 cm /Im Do Q".into(),
        "EMC EMC /Figure << /MCID 99999999999 >> BDC /Im Do".into(),
        "/Figure << /MCID 0 >> BDC /Artifact BMC 0 0 m 10 10 l S EMC q 20 0 0 20 20 20 cm /Im Do Q EMC".into(),
        "/Figure << /MCID 0 >> BDC 0 0 m q 200 200 l S Q EMC".into(),
        "/X BMC ".repeat(5000),
        "q 1e308 0 0 1e308 0 0 cm /Im Do Q 0 0 m NaN NaN l S".into(),
    ];
    for c in &cases {
        for role in RegionRole::IDS.iter().filter_map(|r| RegionRole::from_id(r)) {
            for rect in [[0.0, 0.0, 200.0, 200.0], [0.0, 150.0, 200.0, 200.0], [f64::NAN, 0.0, 1.0, 1.0], [5.0, 5.0, 5.0, 9.0]] {
                let mut d = doc(c, true);
                let before = snap(&d);
                if tag_region(&mut d, &req(rect, role), &mut no_text).is_err() {
                    assert!(snap(&d) == before, "refused requests change nothing ({c:?}, {role:?})");
                }
            }
        }
        let mut d = doc(c, true);
        let _ = page_order(&d, 0, &mut no_text);
        let _ = page_order(&d, 7, &mut no_text);
        let _ = clear_page_structure(&mut d, 0, true);
        let _ = clear_page_structure(&mut d, 9, false);
    }
    // A looped tree: moves are refused, not followed forever.
    let mut d = pdf(&[
        "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 4 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".into(),
        "<< /Type /StructTreeRoot /K 5 0 R >>".into(),
        "<< /Type /StructElem /S /Sect /P 4 0 R /K [6 0 R] >>".into(),
        "<< /Type /StructElem /S /P /P 5 0 R /K [5 0 R 7 0 R] >>".into(),
        "<< /Type /StructElem /S /P /P 6 0 R /K [] >>".into(),
    ]);
    let r5 = ObjRef::new(5, 0);
    let r6 = ObjRef::new(6, 0);
    let r7 = ObjRef::new(7, 0);
    assert!(move_element(&mut d, r5, OrderPlace::After(r7)).is_err(), "into itself");
    assert!(move_element(&mut d, r5, OrderPlace::After(r5)).is_err());
    assert!(move_element(&mut d, ObjRef::new(99, 0), OrderPlace::After(r5)).is_err());
    move_element(&mut d, r7, OrderPlace::Before(r6)).unwrap();
    let t = tags(&d);
    assert_eq!(t.nodes.iter().filter(|n| n.parent == Some(0)).count(), 2);
}

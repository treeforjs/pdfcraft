//! Opening very large and hostile documents through the session (`core.lazy-loading`): many
//! pages, broken cross-reference data that forces a full reconstruction, cyclic and very deep
//! page trees, and truncated files end in an opened (possibly repaired) document or an
//! error — never a crash. Fixtures are original and generated here.

use std::sync::Arc;

use pdfcraft_engine::{Edit, Session};

/// A classic-xref file whose object `i + 1` is `bodies[i]`; `startxref` says `start` if given.
fn classic(bodies: &[String], start: Option<&str>) -> Vec<u8> {
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
    let start = start.map_or_else(|| xref.to_string(), str::to_owned);
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n", bodies.len() + 1).as_bytes());
    out
}

/// Catalog 1, root 2, font 3, then 50-kid intermediate nodes, then (page, contents) per page.
fn pages_doc(pages: usize) -> Vec<String> {
    let nodes = pages.div_ceil(50);
    let first_page = 4 + nodes;
    let mut bodies = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!("<< /Type /Pages /Count {pages} /Kids [{}] >>", (0..nodes).map(|k| format!("{} 0 R", 4 + k)).collect::<Vec<_>>().join(" ")),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    for k in 0..nodes {
        let kids: Vec<String> = (k * 50..((k + 1) * 50).min(pages)).map(|i| format!("{} 0 R", first_page + 2 * i)).collect();
        bodies.push(format!("<< /Type /Pages /Parent 2 0 R /Count {} /Kids [{}] >>", kids.len(), kids.join(" ")));
    }
    for i in 0..pages {
        bodies.push(format!(
            "<< /Type /Page /Parent {} 0 R /MediaBox [0 0 612 792] /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>",
            4 + i / 50,
            first_page + 2 * i + 1
        ));
        let text = format!("BT /F1 24 Tf 72 700 Td (Page {}) Tj ET", i + 1);
        bodies.push(format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len()));
    }
    bodies
}

fn first_page_renders(session: &Session, id: pdfcraft_engine::DocId) {
    let doc = session.get(id).unwrap();
    let mut renderer = pdfcraft_render::PageRenderer::new(doc.display.clone(), pdfcraft_render::RenderConfig::default());
    let page = renderer.render(pdfcraft_render::RenderRequest { page: 0, scale: 0.25, ..Default::default() });
    assert!(page.error.is_none() && page.width > 0, "{:?}", page.error);
}

#[test]
fn a_document_of_thousands_of_pages_opens_renders_and_saves() {
    let bytes = Arc::new(classic(&pages_doc(5_000), None));
    let mut session = Session::new();
    let id = session.open("large.pdf", None, bytes.clone(), None).unwrap();
    assert_eq!(session.get(id).unwrap().info.pages.len(), 5_000);
    first_page_renders(&session, id);
    // Saving: an incremental save appends only the edit to the original bytes.
    session.apply(id, Edit::SetInfo { key: "Title".into(), value: "large".into() }).unwrap();
    let saved = session.save_bytes(id).unwrap();
    assert!(saved.len() > bytes.len() && saved.starts_with(&bytes), "incremental");
    let again = session.open("saved.pdf", None, saved, None).unwrap();
    assert_eq!(session.get(again).unwrap().info.pages.len(), 5_000);
}

#[test]
fn a_broken_xref_forces_a_reconstruction_that_still_opens() {
    for wrong in ["9999999999", "17", "xref", ""] {
        let bytes = Arc::new(classic(&pages_doc(2_000), Some(wrong)));
        let mut session = Session::new();
        let id = session.open("broken.pdf", None, bytes, None).unwrap_or_else(|e| panic!("startxref {wrong:?}: {e}"));
        assert_eq!(session.get(id).unwrap().info.pages.len(), 2_000, "startxref {wrong:?}");
        first_page_renders(&session, id);
    }
}

#[test]
fn cyclic_and_very_deep_page_trees_open_or_fail_cleanly() {
    let page = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".to_string();
    let cases: Vec<(&str, Vec<String>)> = vec![
        // The root lists itself.
        (
            "root in its own kids",
            vec!["<< /Type /Catalog /Pages 2 0 R >>".into(), "<< /Type /Pages /Count 2 /Kids [3 0 R 2 0 R] >>".into(), page.clone()],
        ),
        // Two nodes that are each other's kids.
        (
            "mutual nodes",
            vec![
                "<< /Type /Catalog /Pages 2 0 R >>".into(),
                "<< /Type /Pages /Count 3 /Kids [3 0 R 4 0 R] >>".into(),
                "<< /Type /Pages /Parent 2 0 R /Count 2 /Kids [4 0 R 5 0 R] >>".into(),
                "<< /Type /Pages /Parent 3 0 R /Count 2 /Kids [3 0 R 5 0 R] >>".into(),
                "<< /Type /Page /Parent 3 0 R /MediaBox [0 0 200 200] >>".into(),
            ],
        ),
        // A page that claims to be a node whose kid is the root.
        (
            "page with kids",
            vec![
                "<< /Type /Catalog /Pages 2 0 R >>".into(),
                "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".into(),
                "<< /Type /Pages /Kids [2 0 R] /Count 1 >>".into(),
            ],
        ),
        // Counts that lie wildly.
        (
            "lying counts",
            vec!["<< /Type /Catalog /Pages 2 0 R >>".into(), "<< /Type /Pages /Count 2147483647 /Kids [3 0 R 3 0 R 3 0 R] >>".into(), page.clone()],
        ),
    ];
    for (what, bodies) in cases {
        let mut session = Session::new();
        if let Ok(id) = session.open("cyclic.pdf", None, Arc::new(classic(&bodies, None)), None) {
            let n = session.get(id).unwrap().info.pages.len();
            assert!((1..=8).contains(&n), "{what}: {n} pages");
            first_page_renders(&session, id);
        }
    }
    // A chain of 3 000 nested page-tree nodes with one page at the bottom.
    let depth = 3_000;
    let mut bodies = vec!["<< /Type /Catalog /Pages 2 0 R >>".to_string()];
    for d in 0..depth {
        let num = 2 + d;
        let parent = if d == 0 { String::new() } else { format!("/Parent {} 0 R ", num - 1) };
        bodies.push(format!("<< /Type /Pages {parent}/Count 1 /Kids [{} 0 R] >>", num + 1));
    }
    bodies.push(format!("<< /Type /Page /Parent {} 0 R /MediaBox [0 0 200 200] >>", depth + 1));
    let mut session = Session::new();
    if let Ok(id) = session.open("deep.pdf", None, Arc::new(classic(&bodies, None)), None) {
        assert!(session.get(id).unwrap().info.pages.len() <= 1);
    }
}

#[test]
fn truncated_large_files_open_recovered_or_fail_cleanly() {
    let full = classic(&pages_doc(1_500), None);
    for fraction in [0.0, 0.01, 0.3, 0.5, 0.97, 0.9999] {
        let cut = (full.len() as f64 * fraction) as usize;
        let mut session = Session::new();
        match session.open("truncated.pdf", None, Arc::new(full[..cut].to_vec()), None) {
            Ok(id) => {
                let doc = session.get(id).unwrap();
                assert!(!doc.info.pages.is_empty() && doc.info.pages.len() <= 1_500, "{fraction}");
                first_page_renders(&session, id);
            }
            Err(e) => assert!(!e.to_string().is_empty()),
        }
    }
}

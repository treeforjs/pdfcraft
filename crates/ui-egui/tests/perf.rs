//! Frame-time budget on a 500-page document (parity view.large-document-performance): scrolling
//! continuously must keep the UI's own per-frame work far below a 60 fps frame. Rendering runs on
//! worker threads and is not part of the measurement.

use egui_kittest::Harness;
use printcraft_ui_egui::PrintCraftApp;

/// `n` text pages with a few comments each, so panels and overlays have work to do.
fn big(n: usize) -> Vec<u8> {
    let mut objs: Vec<String> = vec!["<< /Type /Catalog /Pages 2 0 R >>".into()];
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 3 * i)).collect();
    objs.push(format!("<< /Type /Pages /Kids [{}] /Count {n} /MediaBox [0 0 612 792] >>", kids.join(" ")));
    objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    for i in 0..n {
        objs.push(format!(
            "<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> /Annots [{} 0 R] >>",
            5 + 3 * i,
            6 + 3 * i
        ));
        let body: String =
            (0..40).map(|l| format!("BT /F1 10 Tf 72 {} Td (Page {} line {l} with some words to lay out) Tj ET\n", 720 - l * 15, i + 1)).collect();
        objs.push(format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()));
        objs.push("<< /Type /Annot /Subtype /Square /Rect [72 72 144 144] /C [1 0 0] /T (Ada) /Contents (Check) >>".into());
    }
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
    out
}

#[test]
fn scrolling_a_500_page_document_stays_within_the_frame_budget() {
    let bytes = big(500);
    let t0 = std::time::Instant::now();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PrintCraftApp::new();
        app.open_bytes("big.pdf", None, bytes).expect("opens");
        app
    });
    h.run_steps(4);
    let open = t0.elapsed();
    // Scroll through the document: 200 frames, jumping ~2 pages per frame.
    let frames = 200;
    let mut worst = std::time::Duration::ZERO;
    let start = std::time::Instant::now();
    for f in 0..frames {
        h.state_mut().views[0].goto = Some(((f * 5) / 2 % 500, 0.3));
        let t = std::time::Instant::now();
        h.step();
        worst = worst.max(t.elapsed());
    }
    let avg = start.elapsed() / frames as u32;
    eprintln!("PERF open {open:?}, avg frame {avg:?}, worst {worst:?}");
    // Generous for debug builds and shared CI machines; release builds are ~10× faster.
    assert!(avg < std::time::Duration::from_millis(40), "average frame {avg:?}");
}

#[test]
fn panels_with_hundreds_of_items_stay_within_the_frame_budget() {
    let bytes = big(500);
    for panel in ["comments", "pages", "fields"] {
        let b = bytes.clone();
        let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
            let mut app = PrintCraftApp::new();
            app.open_bytes("big.pdf", None, b).expect("opens");
            app.set_option("panel", panel).unwrap();
            app
        });
        h.run_steps(4);
        let frames = 100;
        let start = std::time::Instant::now();
        let mut worst = std::time::Duration::ZERO;
        for f in 0..frames {
            h.state_mut().views[0].goto = Some(((f * 5) % 500, 0.3));
            let t = std::time::Instant::now();
            h.step();
            worst = worst.max(t.elapsed());
        }
        let avg = start.elapsed() / frames as u32;
        eprintln!("PERF {panel}: avg frame {avg:?}, worst {worst:?}");
        assert!(avg < std::time::Duration::from_millis(40), "{panel}: average frame {avg:?}");
    }
}

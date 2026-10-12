//! `render --out` writes the file format its name says (#248): a `.png` is a PNG, not a netpbm
//! PAM stream with a `.png` extension.

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_pdfcraft-cli");

fn tmp(test: &str, name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pdfcraft-cli-render-{test}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

/// A one-page PDF with some text, as `--out` would be given it.
fn fixture(test: &str) -> PathBuf {
    let pdf = tmp(test, "in.pdf");
    let bytes = pdfcraft_engine::Session::new().create_from_text("Render test", "Hello from PdfCraft").unwrap();
    std::fs::write(&pdf, bytes.as_slice()).unwrap();
    pdf
}

fn render(pdf: &std::path::Path, out: &std::path::Path) -> Output {
    Command::new(BIN).args(["render", pdf.to_str().unwrap(), "--page", "1", "--dpi", "36", "--out", out.to_str().unwrap()]).output().unwrap()
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

#[test]
fn a_png_name_gets_a_png() {
    let pdf = fixture("png");
    let out = tmp("png", "p1.png");
    let r = render(&pdf, &out);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG: {:?}", &bytes[..bytes.len().min(16)]);
    // IHDR: the page is 612×792 pt, rendered at 36 dpi
    assert_eq!(&bytes[12..16], b"IHDR");
    assert_eq!((be32(&bytes[16..20]), be32(&bytes[20..24])), (306, 396));
    // the reported size matches the file
    let stderr = String::from_utf8_lossy(&r.stderr);
    assert!(stderr.contains("306×396"), "{stderr}");
}

#[test]
fn jpeg_and_tiff_names_get_those_formats() {
    let pdf = fixture("jpg");
    let jpg = tmp("jpg", "p1.JPG");
    assert!(render(&pdf, &jpg).status.success());
    assert_eq!(&std::fs::read(&jpg).unwrap()[..2], b"\xff\xd8");
    let tif = tmp("jpg", "p1.tif");
    assert!(render(&pdf, &tif).status.success());
    let bytes = std::fs::read(&tif).unwrap();
    assert!(bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*"), "not a TIFF: {:?}", &bytes[..4]);
}

#[test]
fn a_pam_name_still_gets_the_netpbm_stream() {
    let pdf = fixture("pam");
    let out = tmp("pam", "p1.pam");
    assert!(render(&pdf, &out).status.success());
    let bytes = std::fs::read(&out).unwrap();
    let header = b"P7\nWIDTH 306\nHEIGHT 396\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n";
    assert!(bytes.starts_with(header), "{:?}", String::from_utf8_lossy(&bytes[..bytes.len().min(80)]));
    assert_eq!(bytes.len(), header.len() + 306 * 396 * 4);
}

#[test]
fn an_unknown_extension_fails_and_writes_nothing() {
    let pdf = fixture("bmp");
    let out = tmp("bmp", "p1.bmp");
    let r = render(&pdf, &out);
    assert!(!r.status.success());
    let stderr = String::from_utf8_lossy(&r.stderr);
    assert!(stderr.contains(".png, .jpg, .tif or .pam"), "{stderr}");
    assert!(!out.exists());
}

/// `render` and `text` show a form field and a comment that have no appearance stream, as a
/// viewer does (ISO 32000-2 §12.5.5), without changing the file.
#[test]
fn fields_and_comments_without_appearances_are_shown() {
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /DA (/Helv 12 Tf 0 g) >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 100] >>",
        "<< /Type /Page /Parent 2 0 R /Annots [4 0 R 5 0 R] >>",
        "<< /Type /Annot /Subtype /Widget /F 4 /P 3 0 R /FT /Tx /T (city) /V (Lisbon) /Rect [10 60 190 90] /MK << /BG [1 1 0] >> >>",
        "<< /Type /Annot /Subtype /Square /F 4 /P 3 0 R /Rect [10 10 50 40] /C [0 0 1] /IC [0 0 1] >>",
    ];
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        bytes.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    let pdf = tmp("noap", "form.pdf");
    std::fs::write(&pdf, &bytes).unwrap();
    let out = tmp("noap", "p1.pam");
    let r = Command::new(BIN).args(["render", pdf.to_str().unwrap(), "--dpi", "72", "--out", out.to_str().unwrap()]).output().unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let pam = std::fs::read(&out).unwrap();
    let header = b"P7\nWIDTH 200\nHEIGHT 100\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n";
    assert!(pam.starts_with(header));
    let px = |x: usize, y: usize| pam[header.len() + (y * 200 + x) * 4..][..3].to_vec();
    assert_eq!(px(185, 15), vec![255, 255, 0], "the field background");
    assert_eq!(px(30, 75), vec![0, 0, 255], "the Square");
    let t = Command::new(BIN).args(["text", pdf.to_str().unwrap()]).output().unwrap();
    assert!(t.status.success(), "{}", String::from_utf8_lossy(&t.stderr));
    assert!(String::from_utf8_lossy(&t.stdout).contains("Lisbon"), "{}", String::from_utf8_lossy(&t.stdout));
    assert_eq!(std::fs::read(&pdf).unwrap(), bytes, "the file is unchanged");
}

/// A one-page PDF of solid vector paths only, which the native vector path accepts.
fn vector_fixture(test: &str) -> PathBuf {
    let content = b"0 0 1 rg 10 10 60 40 re f 1 0 0 RG 4 w 10 70 m 90 90 l S";
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>".to_vec(),
    ];
    let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    stream.extend_from_slice(content);
    stream.extend_from_slice(b"\nendstream");
    objects.push(stream);
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        bytes.extend_from_slice(object);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    let pdf = tmp(test, "vector.pdf");
    std::fs::write(&pdf, bytes).unwrap();
    pdf
}

fn render_with(pdf: &std::path::Path, out: &std::path::Path, extra: &[&str], env: Option<&str>) -> Output {
    let mut c = Command::new(BIN);
    // The flag goes before the file: a switch must not swallow the next argument.
    c.arg("render").args(extra).args([pdf.to_str().unwrap(), "--page", "1", "--dpi", "72", "--out", out.to_str().unwrap()]);
    c.env_remove("PDFCRAFT_NATIVE_VECTOR");
    if let Some(value) = env {
        c.env("PDFCRAFT_NATIVE_VECTOR", value);
    }
    c.output().unwrap()
}

#[test]
fn the_native_vector_renderer_is_opt_in() {
    let pdf = vector_fixture("native");
    let stderr = |o: &Output| {
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stderr).into_owned()
    };
    let default_out = tmp("native", "default.pam");
    let default = stderr(&render_with(&pdf, &default_out, &[], None));
    assert!(!default.contains("native vector"), "the default must not use the native path: {default}");
    // `0` or any other value is not an opt-in.
    assert!(!stderr(&render_with(&pdf, &tmp("native", "zero.pam"), &[], Some("0"))).contains("native vector"));
    let flag_out = tmp("native", "flag.pam");
    let flagged = stderr(&render_with(&pdf, &flag_out, &["--native-vector"], None));
    assert!(flagged.contains("100×100 px") && flagged.contains("(native vector)"), "{flagged}");
    let env = stderr(&render_with(&pdf, &tmp("native", "env.pam"), &[], Some("1")));
    assert!(env.contains("(native vector)"), "{env}");
    // An axis-aligned filled rectangle is the same on both devices.
    let (a, b) = (std::fs::read(&default_out).unwrap(), std::fs::read(&flag_out).unwrap());
    assert_eq!(a.len(), b.len());
    let header = a.len() - 100 * 100 * 4;
    let at = |img: &[u8], x: usize, y: usize| img[header + (y * 100 + x) * 4..header + (y * 100 + x) * 4 + 4].to_vec();
    assert_eq!(at(&a, 40, 70), at(&b, 40, 70));
    assert_eq!(at(&b, 40, 70), vec![0, 0, 255, 255]);
    // A page with text opted in still renders, through the bootstrap renderer.
    let text = fixture("native-text");
    let fallback = stderr(&render_with(&text, &tmp("native-text", "t.png"), &["--native-vector"], None));
    assert!(fallback.contains("rendered page 1") && !fallback.contains("native vector"), "{fallback}");
}

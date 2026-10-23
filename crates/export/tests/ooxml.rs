//! The Office packages are structurally sound: every part is listed in the zip directory and
//! inflates, every XML part is well-formed, every part has a content type and every override
//! names a part, every relationship target exists and every `r:id`/`r:embed` names a
//! relationship. And the content survives: multi-column reading order, inline bold, fonts,
//! lists and pictures. Where LibreOffice is installed it is used as an external oracle only
//! (it opens the files and its text matches); without it that test returns early.

use std::collections::{BTreeMap, BTreeSet};

use pdfcraft_export::{Block, Image, Page, Placement, Run, Slide, SlidePicture, SlideText, docx, html, pptx, rtf, xlsx};

/// A 4×3 red PNG.
const PNG: &[u8] = b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\x00\x00\x00\x04\x00\x00\x00\x03\x08\x02\x00\x00\x00\x3b\x96\x39\x91\x00\x00\x00\x10\x49\x44\x41\x54\x78\x9c\x63\xf8\xcf\xc0\x00\x47\x0c\x38\x39\x00\xf5\x31\x0b\xf5\x35\x7b\xfb\x82\x00\x00\x00\x00\x49\x45\x4e\x44\xae\x42\x60\x82";

/// The parts of a zip package, read through its central directory, inflated.
fn unzip(zip: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let u = |i: usize, n: usize| zip[i..i + n].iter().rev().fold(0usize, |v, b| v << 8 | *b as usize);
    let end = (0..zip.len() - 21).rev().find(|&i| zip[i..].starts_with(b"PK\x05\x06")).expect("end of central directory");
    let (count, mut at) = (u(end + 10, 2), u(end + 16, 4));
    let mut parts = BTreeMap::new();
    for _ in 0..count {
        assert!(zip[at..].starts_with(b"PK\x01\x02"), "central directory entry");
        let (method, csize, size) = (u(at + 10, 2), u(at + 20, 4), u(at + 24, 4));
        let (name_len, extra, comment, local) = (u(at + 28, 2), u(at + 30, 2), u(at + 32, 2), u(at + 42, 4));
        let name = String::from_utf8(zip[at + 46..at + 46 + name_len].to_vec()).expect("utf-8 name");
        assert!(zip[local..].starts_with(b"PK\x03\x04"), "{name}: local header");
        let data = local + 30 + u(local + 26, 2) + u(local + 28, 2);
        let raw = &zip[data..data + csize];
        let body = match method {
            0 => raw.to_vec(),
            8 => {
                let mut v = Vec::new();
                std::io::Read::read_to_end(&mut flate2::read::DeflateDecoder::new(raw), &mut v).expect("inflate");
                v
            }
            m => panic!("{name}: method {m}"),
        };
        assert_eq!(body.len(), size, "{name}: size");
        assert!(parts.insert(name.clone(), body).is_none(), "{name} twice");
        at += 46 + name_len + extra + comment;
    }
    parts
}

/// A path relative to a part's folder, resolved (`..` and `/` absolute targets).
fn resolve(base: &str, target: &str) -> String {
    let mut out: Vec<&str> = if target.starts_with('/') { Vec::new() } else { base.split('/').filter(|s| !s.is_empty()).collect() };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// Check the package and return its parts.
fn check_package(zip: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let parts = unzip(zip);
    // Well-formed XML.
    let mut docs: BTreeMap<&str, String> = BTreeMap::new();
    for (name, body) in &parts {
        if name.ends_with(".xml") || name.ends_with(".rels") {
            let text = String::from_utf8(body.clone()).unwrap_or_else(|_| panic!("{name}: not UTF-8"));
            roxmltree::Document::parse(&text).unwrap_or_else(|e| panic!("{name}: {e}\n{text}"));
            docs.insert(name, text);
        }
    }
    // Content types.
    let types = roxmltree::Document::parse(docs.get("[Content_Types].xml").expect("[Content_Types].xml")).expect("types");
    let defaults: BTreeSet<String> =
        types.descendants().filter(|n| n.has_tag_name("Default")).filter_map(|n| n.attribute("Extension")).map(str::to_lowercase).collect();
    let overrides: BTreeSet<String> = types
        .descendants()
        .filter(|n| n.has_tag_name("Override"))
        .filter_map(|n| n.attribute("PartName"))
        .map(|p| p.trim_start_matches('/').to_string())
        .collect();
    for o in &overrides {
        assert!(parts.contains_key(o), "override for a missing part {o}");
    }
    for name in parts.keys().filter(|n| *n != "[Content_Types].xml") {
        let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
        assert!(overrides.contains(name) || defaults.contains(&ext), "{name} has no content type");
    }
    // Relationships.
    assert!(parts.contains_key("_rels/.rels"), "package relationships");
    for (name, text) in &docs {
        let Some((dir, file)) = name.rsplit_once("_rels/") else { continue };
        let source = format!("{dir}{}", file.trim_end_matches(".rels"));
        assert!(dir.is_empty() || parts.contains_key(&source), "{name} belongs to a missing part {source}");
        let rels = roxmltree::Document::parse(text).expect("rels");
        let mut ids = BTreeSet::new();
        for r in rels.descendants().filter(|n| n.has_tag_name("Relationship")) {
            let id = r.attribute("Id").expect("Id");
            assert!(ids.insert(id.to_string()), "{name}: {id} twice");
            if r.attribute("TargetMode") == Some("External") {
                continue;
            }
            let target = resolve(dir, r.attribute("Target").expect("Target"));
            assert!(parts.contains_key(&target), "{name}: {id} points at a missing {target}");
        }
        // Each r:id / r:embed the source part uses is one of them.
        if let Some(src) = docs.get(source.as_str()) {
            let d = roxmltree::Document::parse(src).expect("source");
            for n in d.descendants() {
                for a in n.attributes().filter(|a| a.namespace() == Some(REL_NS)) {
                    assert!(ids.contains(a.value()), "{source}: {} = {} has no relationship", a.name(), a.value());
                }
            }
        }
    }
    parts
}

fn text(parts: &BTreeMap<String, Vec<u8>>, name: &str) -> String {
    String::from_utf8(parts.get(name).unwrap_or_else(|| panic!("{name} missing")).clone()).expect("utf-8")
}

fn para(t: &str, rect: [f64; 4], size: f64, font: &str) -> Block {
    Block { text: t.into(), rect, size, font: font.into(), ..Default::default() }
}

/// A two-column page under a heading, with inline bold, a bulleted and a numbered list, a
/// picture and a heading in another font.
fn report() -> Page {
    let mut left = para("Left one has a bold word inside the column text.", [72.0, 560.0, 290.0, 650.0], 11.0, "Georgia");
    let at = left.text.find("bold").expect("bold");
    left.runs = vec![Run { start: at, end: at + 4, font: "Georgia".into(), size: 11.0, bold: true, ..Default::default() }];
    let mut heading = para("Quarterly Notes", [72.0, 700.0, 400.0, 724.0], 22.0, "Arial");
    heading.bold = true;
    let line = |t: &str, y: f64| para(t, [72.0, y, 300.0, y + 12.0], 11.0, "Georgia");
    Page {
        width: 612.0,
        height: 792.0,
        blocks: vec![
            para("Right two closes the second column of the page.", [322.0, 440.0, 540.0, 540.0], 11.0, "Georgia"),
            line("• Apples", 400.0),
            heading,
            para("Right one opens the second column of the page.", [322.0, 560.0, 540.0, 650.0], 11.0, "Georgia"),
            left,
            line("• Pears", 384.0),
            para("Left two closes the first column of the page text.", [72.0, 440.0, 290.0, 540.0], 11.0, "Georgia"),
            line("• Plums", 368.0),
            line("1. First step", 340.0),
            line("2. Second step", 324.0),
        ],
        images: vec![Image { ext: "png", bytes: PNG.to_vec(), rect: [72.0, 150.0, 272.0, 300.0] }],
    }
}

fn in_order(hay: &str, needles: &[&str]) {
    let at: Vec<usize> = needles.iter().map(|n| hay.find(n).unwrap_or_else(|| panic!("{n} missing in {hay}"))).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "out of order {needles:?} at {at:?}");
}

/// The text of each `w:p` of a Word body, and whether each of its runs is bold.
fn word_paragraphs(doc: &str) -> Vec<(String, Vec<(String, bool)>)> {
    let d = roxmltree::Document::parse(doc).expect("document");
    d.descendants()
        .filter(|n| n.has_tag_name("p"))
        .map(|p| {
            let runs: Vec<(String, bool)> = p
                .children()
                .filter(|r| r.has_tag_name("r"))
                .map(|r| {
                    let t: String = r.descendants().filter(|t| t.has_tag_name("t")).filter_map(|t| t.text()).collect();
                    (t, r.descendants().any(|b| b.has_tag_name("b")))
                })
                .collect();
            (runs.iter().map(|r| r.0.as_str()).collect(), runs)
        })
        .collect()
}

#[test]
fn word_reads_columns_in_order_with_inline_bold_fonts_lists_and_pictures() {
    let parts = check_package(&docx(&[report(), report()], "Report"));
    let doc = text(&parts, "word/document.xml");
    let paras = word_paragraphs(&doc);
    let all: String = paras.iter().map(|p| format!("{}\n", p.0)).collect();
    in_order(&all, &["Quarterly Notes", "Left one", "Left two", "Right one", "Right two", "Apples", "Pears", "Plums", "First step", "Second step"]);
    // The bold word stays in its paragraph, as its own bold run.
    let (_, runs) = paras.iter().find(|p| p.0.starts_with("Left one")).expect("left one");
    assert!(runs.len() >= 3 && runs.iter().any(|r| r.0 == "bold" && r.1) && runs.iter().filter(|r| r.0 != "bold").all(|r| !r.1), "{runs:?}");
    // Lists are Word lists: no typed markers, numbering on each item, both kinds defined.
    assert!(!all.contains('•') && !all.contains("1. First"), "{all}");
    assert_eq!(doc.matches("<w:numPr>").count(), 10, "five list items on each of two pages");
    let numbering = text(&parts, "word/numbering.xml");
    assert!(numbering.contains("w:val=\"bullet\"") && numbering.contains("w:val=\"decimal\""), "{numbering}");
    // Fonts: the body font is the default, the heading names its own.
    let styles = text(&parts, "word/styles.xml");
    assert!(styles.contains("<w:docDefaults>") && styles.contains("w:ascii=\"Georgia\""), "{styles}");
    assert!(doc.contains("w:ascii=\"Arial\"") && !doc.contains("w:ascii=\"Georgia\""), "only other fonts are named in runs");
    // Pictures.
    assert_eq!(parts.get("word/media/image1.png").map(Vec::as_slice), Some(PNG));
    assert!(parts.contains_key("word/media/image2.png"));
}

#[test]
fn html_and_rtf_keep_lists_fonts_and_inline_styles() {
    let h = html(&[report()], "Report");
    in_order(
        &h,
        &["Quarterly Notes", "Left one", "Left two", "Right one", "Right two", "<ul>", "Apples", "</ul>", "<ol>", "First step", "</ol>", "<img"],
    );
    assert_eq!(h.matches("<li>").count(), 5, "{h}");
    assert!(h.contains("<strong>bold</strong>") && h.contains("font-family:'Georgia',serif"), "{h}");
    assert!(h.contains("font-family:'Arial',sans-serif"), "the heading's own font: {h}");
    // Well-formed apart from the void elements HTML allows.
    let xml = h.replace("<!DOCTYPE html>", "").replace("<meta charset=\"utf-8\">", "").replace("<hr>", "");
    let xml = regex_free_void(&xml, "<meta ");
    let xml = regex_free_void(&xml, "<img ");
    roxmltree::Document::parse(&xml).unwrap_or_else(|e| panic!("{e}\n{xml}"));

    let r = rtf(&[report()]);
    assert!(r.starts_with("{\\rtf1") && r.contains("{\\fonttbl") && r.contains("Georgia") && r.contains("Arial"), "{r}");
    in_order(&r, &["Quarterly Notes", "Left one", "Left two", "Right one", "Right two"]);
    assert_eq!(r.matches('{').count(), r.matches('}').count(), "balanced groups");
}

/// Close a void element that starts with `open` (`<img …>` ▸ `<img …/>`).
fn regex_free_void(s: &str, open: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(open) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.find('>').expect("tag end");
        out.push_str(&tail[..end]);
        out.push_str("/>");
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

#[test]
fn excel_places_pictures_on_their_sheets() {
    let mut second = report();
    second.images.push(Image { ext: "png", bytes: PNG.to_vec(), rect: [300.0, 100.0, 1500.0, 140.0] });
    let parts = check_package(&xlsx(&[report(), second, Page::default()], "Report"));
    for n in [
        "xl/workbook.xml",
        "xl/worksheets/sheet1.xml",
        "xl/worksheets/sheet3.xml",
        "xl/drawings/drawing1.xml",
        "xl/drawings/drawing2.xml",
        "xl/media/image3.png",
    ] {
        assert!(parts.contains_key(n), "{n} missing: {:?}", parts.keys());
    }
    assert!(!parts.contains_key("xl/drawings/drawing3.xml"), "an empty page has no drawing");
    let sheet = text(&parts, "xl/worksheets/sheet1.xml");
    assert!(sheet.contains("<drawing r:id="), "{sheet}");
    // Wide pictures are scaled to fit (480 pt × 12700 EMU).
    let drawing = text(&parts, "xl/drawings/drawing2.xml");
    assert!(drawing.contains("cx=\"6096000\""), "{drawing}");
    // List items keep their markers as text.
    let strings = parts.get("xl/sharedStrings.xml").map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default();
    assert!(strings.contains("• Apples") || sheet.contains("• Apples"), "{sheet}");
}

#[test]
fn powerpoint_packages_are_sound() {
    let at = Placement { x: 10.0, y: 10.0, w: 200.0, h: 40.0, ..Default::default() };
    let slide = Slide {
        width: 612.0,
        height: 792.0,
        background: Some(PNG.to_vec()),
        pictures: vec![SlidePicture { bytes: PNG.to_vec(), at }],
        texts: vec![SlideText {
            text: "Hello & <world>".into(),
            font: "Georgia".into(),
            size: 12.0,
            bold: true,
            italic: false,
            color: [0.0; 3],
            lines: 2,
            at,
        }],
    };
    let (zip, stats) = pptx(&[slide.clone(), slide, Slide::default()], "Deck");
    assert_eq!(stats.slides, 3);
    let parts = check_package(&zip);
    assert!(parts.contains_key("ppt/presentation.xml") && parts.contains_key("ppt/slides/slide3.xml"), "{:?}", parts.keys());
}

#[test]
fn hostile_pages_still_make_sound_packages() {
    let nan = f64::NAN;
    let mut weird = para("Runs \u{1} out \u{FFFE} of range ünïcödé", [nan, 0.0, f64::INFINITY, -1e308], nan, "\"<&>'");
    weird.runs = vec![
        Run { start: 3, end: 1, bold: true, ..Default::default() },
        Run { start: 0, end: 10_000, ..Default::default() },
        Run { start: 27, end: 28, italic: true, ..Default::default() }, // inside 'ü'
        Run { start: usize::MAX - 1, end: usize::MAX, ..Default::default() },
    ];
    let mut blocks = vec![weird, para("", [0.0; 4], 0.0, ""), para("•", [0.0, 0.0, 0.0, 0.0], -5.0, "")];
    // Many tiny markers on one spot, and a deep staircase of indents.
    for k in 0..300 {
        let x = f64::from(k % 40) * 18.0;
        blocks.push(para(&format!("{}. item", k % 7 + 1), [x, 700.0 - f64::from(k), x + 50.0, 710.0 - f64::from(k)], 11.0, "Arial"));
    }
    let bad_image = Image { ext: "png", bytes: b"not a png".to_vec(), rect: [nan; 4] };
    let pages = vec![Page { width: nan, height: -1.0, blocks, images: vec![bad_image] }, Page::default()];
    check_package(&docx(&pages, "\u{0}title"));
    check_package(&xlsx(&pages, "\u{0}title"));
    let h = html(&pages, "t");
    assert!(h.ends_with("</html>\n"));
    let r = rtf(&pages);
    assert_eq!(r.matches('{').count() - r.matches("\\{").count(), r.matches('}').count() - r.matches("\\}").count());
}

/// LibreOffice (an external oracle, not a dependency) opens the Word, Excel and PowerPoint
/// files and finds the text in reading order. Skipped where `soffice` isn't installed.
#[test]
fn libreoffice_opens_the_exports() {
    let Ok(out) = std::process::Command::new("soffice").arg("--version").output() else { return };
    if !out.status.success() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("pdfcraft-export-oracle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(dir.join("report.docx"), docx(&[report()], "Report")).expect("write");
    std::fs::write(dir.join("report.xlsx"), xlsx(&[report()], "Report")).expect("write");
    let profile = format!("-env:UserInstallation=file://{}", dir.join("profile").display());
    let convert = |file: &str, to: &str| {
        let mut child = std::process::Command::new("soffice")
            .args([profile.as_str(), "--headless", "--norestore", "--convert-to", to, "--outdir"])
            .arg(&dir)
            .arg(dir.join(file))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("soffice");
        let start = std::time::Instant::now();
        while child.try_wait().expect("wait").is_none() {
            if start.elapsed().as_secs() > 180 {
                let _ = child.kill();
                panic!("soffice took too long on {file}");
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    };
    convert("report.docx", "txt:Text");
    let txt = std::fs::read_to_string(dir.join("report.txt")).expect("LibreOffice read the Word file");
    in_order(&txt, &["Quarterly Notes", "Left one has a bold word", "Left two", "Right one", "Right two", "Apples", "First step"]);
    convert("report.xlsx", "csv");
    let csv = std::fs::read_to_string(dir.join("report.csv")).expect("LibreOffice read the workbook");
    assert!(csv.contains("Quarterly Notes"), "{csv}");
    let _ = std::fs::remove_dir_all(&dir);
}

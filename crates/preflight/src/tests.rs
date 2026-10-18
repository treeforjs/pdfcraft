use super::*;

pub(crate) fn build(objs: &[&str], trailer_extra: &str) -> Document {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    Document::open(std::sync::Arc::new(out)).unwrap()
}

/// A page with embedded-font-free text, a note without the print flag, a link with a
/// JavaScript action, an interpolated image, and a document JavaScript.
fn messy() -> Document {
    build(
        &[
            "<< /Type /Catalog /Pages 2 0 R /Names << /JavaScript << /Names [(x) 9 0 R] >> >> /AA << /WC 9 0 R >> >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 200] >>",
            "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> /XObject << /Im 8 0 R >> >> /Annots [6 0 R 7 0 R] >>",
            "<< /Length 44 >>\nstream\n1 0 0 rg BT /F1 12 Tf 10 10 Td (Hi) Tj ET /Im Do\nendstream",
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
            "<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /P 3 0 R /AP << /N 10 0 R >> >>",
            "<< /Type /Annot /Subtype /Link /Rect [40 10 80 30] /P 3 0 R /F 4 /A 9 0 R >>",
            "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Interpolate true /Length 1 >>\nstream\n\u{0}\nendstream",
            "<< /Type /Action /S /JavaScript /JS (app.alert(1)) >>",
            "<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 0 >>\nstream\n\nendstream",
        ],
        "/Info << /Title (Report) /Producer (Test) /CreationDate (D:20240105093000Z) >>",
    )
}

fn clauses(issues: &[Issue]) -> Vec<&'static str> {
    let mut c: Vec<&str> = issues.iter().map(|i| i.clause).collect();
    c.dedup();
    c
}

#[test]
fn verify_reports_what_breaks_pdfa() {
    let issues = verify(&messy(), Level::A2b);
    let msgs: Vec<&str> = issues.iter().map(|i| i.message.as_str()).collect();
    for want in [
        "The document has no XMP metadata",
        "Device colour is used, but there is no PDF/A output intent",
        "The document has additional actions (/AA)",
        "The document has document-level JavaScript",
        "A JavaScript action isn't allowed",
        "A Text annotation isn't set to print, or is hidden",
        "The font Helvetica isn't embedded",
        "An image asks to be interpolated",
    ] {
        assert!(msgs.contains(&want), "missing {want:?} in {msgs:#?}");
    }
    assert!(issues.iter().find(|i| i.message.starts_with("The font")).is_some_and(|i| !i.fixable));
    assert!(clauses(&issues).contains(&"6.6.2.1"));
}

#[test]
fn convert_fixes_all_but_fonts() {
    let mut doc = messy();
    let r = convert(&mut doc, Level::A2b).unwrap();
    assert!(r.fixed.iter().any(|f| f.contains("output intent")), "{:?}", r.fixed);
    let left: Vec<&str> = r.remaining.iter().map(|i| i.message.as_str()).collect();
    assert_eq!(left, ["The font Helvetica isn't embedded"], "{:#?}", r.remaining);
    let d = declared(&doc);
    assert_eq!(d.pdfa, Some((2, "B".into())));
    assert_eq!(d.output_intents, ["sRGB IEC61966-2.1"]);
    // The metadata carries the document information.
    let x = metadata(&doc).unwrap();
    assert_eq!(xmp::value(&x, "dc:title").as_deref(), Some("Report"));
    assert_eq!(xmp::value(&x, "xmp:CreateDate").as_deref(), Some("2024-01-05T09:30:00Z"));
    // It survives a save.
    let bytes = pdfcraft_cos::write_full(&doc, &Default::default()).unwrap();
    let back = Document::open(std::sync::Arc::new(bytes)).unwrap();
    assert_eq!(verify(&back, Level::A2b).len(), 1);
}

#[test]
fn embedded_files_differ_between_parts() {
    let doc = build(
        &[
            "<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles << /Names [(a.txt) 4 0 R] >> >> >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 200] >>",
            "<< /Type /Page /Parent 2 0 R >>",
            "<< /Type /Filespec /F (a.txt) /EF << /F 5 0 R >> >>",
            "<< /Type /EmbeddedFile /Length 2 >>\nstream\nhi\nendstream",
        ],
        "",
    );
    assert!(verify(&doc, Level::A2b).iter().any(|i| i.clause == "6.8" && !i.fixable));
    let mut d3 = doc.clone();
    assert!(verify(&d3, Level::A3b).iter().any(|i| i.message.contains("AFRelationship")));
    let r = convert(&mut d3, Level::A3b).unwrap();
    assert!(r.remaining.is_empty(), "{:#?}", r.remaining);
}

fn stream(dict: &str, data: &str) -> String {
    format!("<< {dict} /Length {} >>\nstream\n{data}\nendstream", data.len())
}

fn xmp_for(part: u8, conformance: &str) -> String {
    stream(
        "/Type /Metadata /Subtype /XML",
        &format!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
             <rdf:Description rdf:about=\"\" xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\">\
             <pdfaid:part>{part}</pdfaid:part><pdfaid:conformance>{conformance}</pdfaid:conformance></rdf:Description></rdf:RDF></x:xmpmeta>"
        ),
    )
}

/// One page showing `content` with font F1 = object 5 (`font`), its descriptor 6 (embedded
/// program 7), and object 8 free for a ToUnicode stream or anything else.
fn text_doc(font: &str, content: &str, extra: &str, xmp: (u8, &str)) -> Document {
    build(
        &[
            "<< /Type /Catalog /Pages 2 0 R /Metadata 9 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 200] >>",
            "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
            &stream("", content),
            font,
            "<< /Type /FontDescriptor /FontName /ABCDEF+Sans /Flags 32 /FontFile2 7 0 R >>",
            &stream("", "fake font program"),
            extra,
            &xmp_for(xmp.0, xmp.1),
        ],
        "",
    )
}

const WIN_ANSI_FONT: &str = "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+Sans /Encoding /WinAnsiEncoding /FontDescriptor 6 0 R >>";

fn errors(issues: &[Issue]) -> Vec<&Issue> {
    issues.iter().filter(|i| i.is_error()).collect()
}

#[test]
fn levels_parse_and_label() {
    for (id, level) in
        [("1a", Level::A1a), ("PDF/A-2u", Level::A2u), ("pdfa-3a", Level::A3a), ("A2b", Level::A2b), ("3U", Level::A3u), ("1b", Level::A1b)]
    {
        assert_eq!(Level::from_id(id), Some(level), "{id}");
    }
    for bad in ["1u", "4", "2x", "", "pdfa", "22b", "\u{0}"] {
        assert_eq!(Level::from_id(bad), None, "{bad:?}");
    }
    assert_eq!(Level::new(1, Conformance::U), None);
    assert!(Level::A2u.needs_unicode() && !Level::A2u.needs_structure());
    assert!(Level::A1a.needs_unicode() && Level::A1a.needs_structure());
    assert!(!Level::A3b.needs_unicode());
    for l in Level::ALL {
        assert_eq!(Level::from_id(l.id()), Some(l));
        assert_eq!(Level::from_id(l.label()), Some(l));
    }
}

#[test]
fn a_clean_2u_file_conforms() {
    let doc = text_doc(WIN_ANSI_FONT, "BT /F1 12 Tf 10 10 Td (Caf\u{e9}) Tj [(A) -50 (B)] TJ ET", "null", (2, "U"));
    assert_eq!(declared_level(&doc), Some(Level::A2u));
    let issues = verify(&doc, Level::A2u);
    assert!(errors(&issues).is_empty(), "{issues:#?}");
    // The same file isn't 2b: its metadata says 2u.
    assert!(verify(&doc, Level::A2b).iter().any(|i| i.clause == "6.6.4"));
}

#[test]
fn an_unmapped_glyph_fails_level_u_and_conversion_refuses() {
    // Identity-H with a ToUnicode CMap that lacks the second code shown.
    let font = "<< /Type /Font /Subtype /Type0 /BaseFont /ABCDEF+Sans /Encoding /Identity-H /ToUnicode 8 0 R \
                /DescendantFonts [<< /Type /Font /Subtype /CIDFontType2 /BaseFont /ABCDEF+Sans /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 6 0 R >>] >>";
    let cmap = stream("", "1 begincodespacerange <0000> <FFFF> endcodespacerange 1 beginbfchar <0001> <0041> endbfchar");
    let doc = text_doc(font, "BT /F1 12 Tf 10 10 Td <00010002> Tj ET", &cmap, (2, "U"));
    let issues = verify(&doc, Level::A2u);
    let e = errors(&issues);
    assert_eq!(e.len(), 1, "{issues:#?}");
    assert_eq!(e[0].clause, "6.2.11.7.2");
    assert!(e[0].message.contains("no Unicode mapping") && e[0].message.contains("<0002>"), "{}", e[0].message);
    assert!(!e[0].fixable);
    assert_eq!(e[0].page, Some(0));
    // Level b doesn't care.
    assert!(errors(&verify(&doc, Level::A2b)).iter().all(|i| i.clause != "6.2.11.7.2"));
    let mut d = doc.clone();
    match convert(&mut d, Level::A2u) {
        Err(ConvertError::Refused { level, blocking }) => {
            assert_eq!(level, Level::A2u);
            assert_eq!(blocking.len(), 1);
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    // Nothing changed.
    assert_eq!(declared(&d).pdfa, Some((2, "U".into())));
    assert!(catalog(&d).get(b"OutputIntents").is_none());
    // A symbolic font with no encoding has no known meaning either.
    let sym = "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+Sans /FontDescriptor 8 0 R >>";
    let doc = text_doc(sym, "BT /F1 12 Tf (x) Tj ET", "<< /Type /FontDescriptor /Flags 4 /FontFile2 7 0 R >>", (2, "U"));
    assert!(errors(&verify(&doc, Level::A2u)).iter().any(|i| i.clause == "6.2.11.7.2" && !i.fixable));
}

#[test]
fn conversion_to_2u_writes_tounicode_from_the_encoding() {
    // The CMap maps H to U+FFFD and i to U+0000; the Differences say what they are.
    let font = "<< /Type /Font /Subtype /Type1 /BaseFont /ABCDEF+Sans /FontDescriptor 6 0 R /ToUnicode 8 0 R \
                /Encoding << /Type /Encoding /Differences [72 /H 105 /i 200 /uni263A] >> >>";
    let cmap = stream("", "1 begincodespacerange <00> <FF> endcodespacerange 2 beginbfchar <48> <FFFD> <69> <0000> endbfchar");
    let doc = text_doc(font, "BT /F1 12 Tf 10 10 Td (Hi) Tj <C8> Tj ET", &cmap, (2, "B"));
    let issues = verify(&doc, Level::A2u);
    let e = errors(&issues);
    assert!(e.iter().any(|i| i.message.contains("U+FFFD")), "{issues:#?}");
    assert!(e.iter().any(|i| i.message.contains("U+0000")), "{issues:#?}");
    assert!(e.iter().all(|i| i.fixable || i.clause == "6.6.4"), "{issues:#?}");
    let mut d = doc.clone();
    let r = convert(&mut d, Level::A2u).unwrap();
    assert!(r.fixed.iter().any(|f| f.contains("ToUnicode")), "{:?}", r.fixed);
    assert!(errors(&r.remaining).is_empty(), "{:#?}", r.remaining);
    assert_eq!(declared(&d).pdfa, Some((2, "U".into())));
    let f = d.get(ObjRef::new(5, 0)).as_dict().cloned().unwrap();
    let tu = pdfcraft_fonts::pdf::to_unicode_map(&d, &f).unwrap();
    assert_eq!(tu.get(&0x48).map(String::as_str), Some("H"));
    assert_eq!(tu.get(&0x69).map(String::as_str), Some("i"));
    assert_eq!(tu.get(&0xC8).map(String::as_str), Some("\u{263A}"));
    // It survives a save.
    let bytes = pdfcraft_cos::write_full(&d, &Default::default()).unwrap();
    let back = Document::open(std::sync::Arc::new(bytes)).unwrap();
    assert!(errors(&verify(&back, Level::A2u)).is_empty());
}

#[test]
fn private_use_values_warn() {
    let font = "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+Sans /Encoding /WinAnsiEncoding /FontDescriptor 6 0 R /ToUnicode 8 0 R >>";
    let cmap = stream("", "1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfchar <41> <E000> endbfchar");
    let doc = text_doc(font, "BT /F1 12 Tf (A) Tj ET", &cmap, (3, "U"));
    let issues = verify(&doc, Level::A3u);
    assert!(errors(&issues).is_empty(), "{issues:#?}");
    assert!(issues.iter().any(|i| i.severity == Severity::Warning && i.clause == "6.2.11.7.3"), "{issues:#?}");
}

#[test]
fn level_a_needs_the_structure_check() {
    let mut doc = text_doc(WIN_ANSI_FONT, "BT /F1 12 Tf (A) Tj ET", "null", (2, "B"));
    match convert(&mut doc, Level::A2a) {
        Err(ConvertError::Refused { blocking, .. }) => assert!(blocking[0].message.contains("logical structure")),
        other => panic!("expected a refusal, got {other:?}"),
    }
    // Word breaks made by positioning warn at level a.
    let doc = text_doc(WIN_ANSI_FONT, "BT /F1 12 Tf [(Hello) -400 (world) -400 ( again)] TJ ET", "null", (2, "A"));
    let w: Vec<Issue> = verify(&doc, Level::A2a).into_iter().filter(|i| i.message.contains("word break")).collect();
    assert_eq!(w.len(), 1, "{w:#?}");
    assert!(w[0].message.contains("1 word break "), "{}", w[0].message);
    assert_eq!(w[0].severity, Severity::Warning);
    assert!(!verify(&doc, Level::A2u).iter().any(|i| i.message.contains("word break")));
}

#[test]
fn pdfa1_rules_and_clauses() {
    let doc = build(
        &[
            "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [] /D << >> >> >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 200] >>",
            "<< /Type /Page /Parent 2 0 R /Group << /S /Transparency /CS /DeviceRGB >> /Resources << /ExtGState << /G << /ca 0.5 >> >> >> >>",
        ],
        "",
    );
    let issues = verify(&doc, Level::A1b);
    let c = clauses(&issues);
    assert!(c.contains(&"6.7.2"), "no XMP is 6.7.2 in part 1: {issues:#?}");
    assert!(c.contains(&"6.1.13") && c.contains(&"6.4"), "{issues:#?}");
    assert!(issues.iter().any(|i| i.severity == Severity::Warning && i.clause == "6.1"));
    assert!(!clauses(&verify(&doc, Level::A2b)).contains(&"6.4"));
    let mut d = doc.clone();
    let r = convert(&mut d, Level::A1b).unwrap();
    assert_eq!(declared(&d).pdfa, Some((1, "B".into())));
    assert!(r.remaining.iter().any(|i| i.clause == "6.4"));
}

#[test]
fn hostile_fonts_and_content_do_not_panic() {
    let fonts = [
        "<< /Type /Font /Subtype /Type1 /Encoding << /Differences [-5 /A 99999999999 /B /C (x) 3.5 /uniZZZZ /u110000] >> /ToUnicode 8 0 R >>",
        "<< /Type /Font /Subtype /Type0 /Encoding 8 0 R /DescendantFonts 7 >>",
        "<< /Type /Font /Subtype /Type3 /Encoding /Nope /ToUnicode 5 0 R >>",
        "<< /Type /Font /Subtype /TrueType /Encoding 42 /FontDescriptor 5 0 R >>",
        "(not a font)",
        "<< /Type /Font /Subtype /Type0 /DescendantFonts [ 5 0 R ] /ToUnicode 8 0 R >>",
    ];
    let extras = [
        stream("", "1 begincodespacerange <00> endcodespacerange 99999999 beginbfrange <00> <FF> [<D800> <DC00> <>] <0000> <FFFF> <0041> endbfrange"),
        stream("/Filter /FlateDecode", "garbage"),
        "<< /Type /XObject /Subtype /Form /Resources << /XObject << /X 8 0 R >> /Font << /F1 5 0 R >> >> >>".to_string(),
    ];
    let contents = [
        "BT /F1 Tf Tj TJ ' \" /F1 12 Tf [(a) -1e308 (b) 1e308 () -99999] TJ 1 2 (c) \" (d) ' ET /X Do q q q Q Q Q Q Q",
        "BT /F1 12 Tf [[(nested)] << /a 1 >> -500] TJ <0> Tj <> Tj (\\377\\376) Tj ET",
        "/F1 12 Tf (no BT) Tj /Missing 9 Tf (x) Tj",
    ];
    for font in fonts {
        for extra in &extras {
            for content in contents {
                let doc = text_doc(font, content, extra, (2, "U"));
                for level in Level::ALL {
                    let _ = verify(&doc, level);
                    let mut d = doc.clone();
                    let _ = convert(&mut d, level);
                }
            }
        }
    }
    // A form that draws itself.
    let doc = build(
        &[
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 200] >>",
            "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /XObject << /X 4 0 R >> >> /Annots [5 0 R] >>",
            &stream("/Type /XObject /Subtype /Form /Resources << /XObject << /X 4 0 R >> >>", "/X Do"),
            "<< /Type /Annot /Subtype /Square /Rect [0 0 1 1] /AP << /N << /On 4 0 R /Off 3 >> >> >>",
        ],
        "",
    );
    for level in Level::ALL {
        let _ = verify(&doc, level);
    }
}

#[test]
fn tounicode_cmaps_round_trip() {
    let map: std::collections::BTreeMap<u32, String> = (0..=255u32).map(|c| (c, if c == 1 { "\u{1F600}".into() } else { "fi".into() })).collect();
    let cmap = unicode::to_unicode_cmap(&map);
    let mut d = Dict::new();
    d.set(b"Subtype".to_vec(), Object::name("Type1"));
    let mut doc = Document::new_empty();
    let s = doc.add(Object::Stream(Stream::from_raw(Dict::new(), cmap)));
    d.set(b"ToUnicode".to_vec(), Object::Ref(s));
    let back = pdfcraft_fonts::pdf::to_unicode_map(&doc, &d).unwrap();
    assert_eq!(back.len(), 256);
    assert_eq!(back.get(&1).map(String::as_str), Some("\u{1F600}"));
    assert_eq!(back.get(&200).map(String::as_str), Some("fi"));
}

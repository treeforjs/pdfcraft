//! The optimizer on documents with images drawn at known sizes.

use std::sync::Arc;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, SaveOptions, Stream, write_full};
use pdfcraft_optimize::{Compression, ImageSettings, Settings, SpaceCategory, audit_space, audit_space_detailed, effective_resolutions, optimize};

fn image(doc: &mut Document, w: u32, h: u32, n: usize, jpeg: bool, smask: Option<ObjRef>) -> ObjRef {
    // A smooth gradient with a little texture (photo-like).
    let mut px = Vec::with_capacity((w * h) as usize * n);
    for y in 0..h {
        for x in 0..w {
            let v = ((x * 255 / w.max(1)) as u8).wrapping_add(((x ^ y) & 7) as u8);
            px.push(v);
            if n == 3 {
                px.push((y * 255 / h.max(1)) as u8);
                px.push(128);
            }
        }
    }
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("XObject"));
    d.set(b"Subtype".to_vec(), Object::name("Image"));
    d.set(b"Width".to_vec(), Object::Int(w as i64));
    d.set(b"Height".to_vec(), Object::Int(h as i64));
    d.set(b"BitsPerComponent".to_vec(), Object::Int(8));
    d.set(b"ColorSpace".to_vec(), Object::name(if n == 1 { "DeviceGray" } else { "DeviceRGB" }));
    if let Some(m) = smask {
        d.set(b"SMask".to_vec(), Object::Ref(m));
    }
    let s = if jpeg {
        let mut out = Vec::new();
        let ty = if n == 1 { image::ExtendedColorType::L8 } else { image::ExtendedColorType::Rgb8 };
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 98).encode(&px, w, h, ty).unwrap();
        d.set(b"Filter".to_vec(), Object::name("DCTDecode"));
        Stream::from_raw(d, out)
    } else {
        Stream::flate(d, &px)
    };
    doc.add(Object::Stream(s))
}

fn page(doc: &mut Document, xobjects: &[(&str, ObjRef)], content: &str) -> ObjRef {
    let pages = doc.root().and_then(|r| doc.get(r).as_dict().and_then(|d| d.reference(b"Pages"))).unwrap();
    let mut x = Dict::new();
    for (n, r) in xobjects {
        x.set(n.as_bytes().to_vec(), Object::Ref(*r));
    }
    let mut res = Dict::new();
    res.set(b"XObject".to_vec(), Object::Dict(x));
    let c = doc.add(Object::Stream(Stream::from_raw(Dict::new(), content.as_bytes().to_vec())));
    let mut p = Dict::new();
    p.set(b"Type".to_vec(), Object::name("Page"));
    p.set(b"Parent".to_vec(), Object::Ref(pages));
    p.set(b"MediaBox".to_vec(), Object::Array(vec![0.into(), 0.into(), 612.into(), 792.into()]));
    p.set(b"Resources".to_vec(), Object::Dict(res));
    p.set(b"Contents".to_vec(), Object::Ref(c));
    p.set(b"Thumb".to_vec(), Object::Ref(c));
    let r = doc.add(Object::Dict(p));
    doc.update_dict(pages, |d| {
        let mut kids = d.get(b"Kids").and_then(|k| k.as_array().cloned()).unwrap_or_default();
        kids.push(Object::Ref(r));
        d.set(b"Count".to_vec(), Object::Int(kids.len() as i64));
        d.set(b"Kids".to_vec(), Object::Array(kids));
    })
    .unwrap();
    r
}

fn stream(doc: &Document, r: ObjRef) -> Stream {
    match &*doc.get(r) {
        Object::Stream(s) => s.clone(),
        _ => panic!("not a stream"),
    }
}

#[test]
fn images_are_measured_where_drawn_and_downsampled() {
    let mut doc = Document::new_empty();
    // 1200 px drawn 144 pt (2 in) wide → 600 ppi.
    let big = image(&mut doc, 1200, 1200, 3, false, None);
    // 100 px at 144 pt → 50 ppi: left at its size.
    let small = image(&mut doc, 100, 100, 1, false, None);
    // With a soft mask, 800 px at 72 pt → 800 ppi.
    let mask = image(&mut doc, 800, 800, 1, false, None);
    let masked = image(&mut doc, 800, 800, 3, false, Some(mask));
    // A high-quality JPEG at 72 ppi: recompressed, not resized.
    let photo = image(&mut doc, 600, 400, 3, true, None);
    // Drawn twice: 300 ppi and 75 ppi → effective 75 (never downsampled below what a use needs).
    let twice = image(&mut doc, 300, 300, 3, false, None);
    page(&mut doc, &[("Im1", big), ("Im2", small)], "q 144 0 0 144 36 600 cm /Im1 Do Q q 144 0 0 144 300 600 cm /Im2 Do Q");
    page(
        &mut doc,
        &[("A", masked), ("B", photo), ("C", twice)],
        "q 72 0 0 72 36 36 cm /A Do Q q 1 0 0 1 100 100 cm 600 0 0 400 0 0 cm /B Do Q q 72 0 0 72 0 0 cm /C Do Q q 288 0 0 288 300 300 cm /C Do Q",
    );
    let pages = pdfcraft_annot::page_refs(&doc).unwrap();
    let ppi = effective_resolutions(&doc, &pages);
    assert_eq!(ppi[&big].round(), 600.0);
    assert_eq!(ppi[&small].round(), 50.0);
    assert_eq!(ppi[&masked].round(), 800.0);
    assert_eq!(ppi[&photo].round(), 72.0);
    assert_eq!(ppi[&twice].round(), 75.0, "the largest use decides");

    let before = write_full(&doc, &SaveOptions::default()).unwrap().len();
    let report = optimize(&mut doc, &Settings::default()).unwrap();
    assert_eq!(report.images, 5, "{report:?}");
    assert_eq!(report.images_resampled, 2, "{report:?}");
    assert!(report.thumbnails == 2);
    let b = stream(&doc, big);
    assert_eq!((b.dict.int(b"Width"), b.dict.int(b"Height"), b.dict.name(b"Filter")), (Some(300), Some(300), Some(&b"DCTDecode"[..])));
    let m = stream(&doc, masked);
    let sm = stream(&doc, mask);
    assert_eq!((m.dict.int(b"Width"), sm.dict.int(b"Width")), (Some(150), Some(150)), "the soft mask follows its image");
    assert_eq!(sm.dict.name(b"Filter"), Some(&b"FlateDecode"[..]));
    assert_eq!(stream(&doc, small).dict.int(b"Width"), Some(100));
    assert_eq!(stream(&doc, twice).dict.int(b"Width"), Some(300), "75 ppi is below the threshold");
    let p = stream(&doc, photo);
    assert_eq!(p.dict.int(b"Width"), Some(600));
    let after_bytes = write_full(&doc, &SaveOptions::default()).unwrap();
    assert!(after_bytes.len() * 3 < before, "{} → {}", before, after_bytes.len());

    // The optimized file opens, and the page still shows the picture where it was.
    let reopened = Document::open(Arc::new(after_bytes.clone())).unwrap();
    assert_eq!(pdfcraft_annot::page_refs(&reopened).unwrap().len(), 2);
    let mut r = pdfcraft_render::PageRenderer::new(Arc::new(after_bytes), pdfcraft_render::RenderConfig::default());
    let out = r.render(pdfcraft_render::RenderRequest { page: 0, scale: 1.0, ..Default::default() });
    assert!(out.error.is_none(), "{:?}", out.error);
    // Top-left image area (36..180 x 600..744 user → y from the top 48..192): drawn, not white.
    let i = ((100 * out.width + 100) * 4) as usize;
    assert_ne!(&out.rgba[i..i + 3], &[255, 255, 255]);
}

#[test]
fn settings_choose_what_happens() {
    let mut doc = Document::new_empty();
    let big = image(&mut doc, 1200, 600, 1, false, None);
    page(&mut doc, &[("Im1", big)], "q 144 0 0 72 36 600 cm /Im1 Do Q");
    let mut keep = doc.clone();
    // Off: nothing changes (but the thumbnail goes).
    let off = ImageSettings { downsample: false, target_ppi: 150.0, above_ppi: 225.0, compression: Compression::Retain };
    let r = optimize(&mut keep, &Settings { color: off, gray: off, ..Settings::default() }).unwrap();
    assert_eq!((r.images_resampled, r.images_recompressed), (0, 0));
    // Lossless: resampled to 300 ppi, Flate.
    let lossless = ImageSettings { downsample: true, target_ppi: 300.0, above_ppi: 450.0, compression: Compression::Flate };
    let r = optimize(&mut doc, &Settings { gray: lossless, ..Settings::default() }).unwrap();
    assert_eq!(r.images_resampled, 1);
    let s = stream(&doc, big);
    assert_eq!((s.dict.int(b"Width"), s.dict.int(b"Height"), s.dict.name(b"Filter")), (Some(600), Some(300), Some(&b"FlateDecode"[..])));
}

#[test]
fn discards_and_clean_up() {
    let mut doc = Document::new_empty();
    let pages = page(&mut doc, &[], "BT /F1 12 Tf 72 720 Td (Hello world, hello world, hello world, hello world) Tj ET");
    let root = doc.root().unwrap();
    let mut vp = Dict::new();
    vp.set(b"PrintScaling".to_vec(), Object::name("None"));
    vp.set(b"Duplex".to_vec(), Object::name("Simplex"));
    vp.set(b"HideToolbar".to_vec(), Object::Bool(true));
    let st = doc.add(Object::Dict(Dict::new()));
    doc.update_dict(root, |c| {
        c.set(b"ViewerPreferences".to_vec(), Object::Dict(vp));
        c.set(b"StructTreeRoot".to_vec(), Object::Ref(st));
        c.set(b"MarkInfo".to_vec(), Object::Dict(Dict::new()));
    })
    .unwrap();
    doc.update_dict(pages, |p| p.set(b"StructParents".to_vec(), Object::Int(0))).unwrap();
    let settings = Settings { discard_tags: true, discard_print_settings: true, ..Settings::default() };
    let r = optimize(&mut doc, &settings).unwrap();
    assert!(r.tags_removed);
    assert_eq!((r.print_settings, r.thumbnails, r.streams_compressed), (2, 1, 1));
    let c = doc.get(root).as_dict().cloned().unwrap();
    assert!(!c.contains(b"StructTreeRoot") && !c.contains(b"MarkInfo"));
    let vp = c.get(b"ViewerPreferences").and_then(|v| v.as_dict().cloned()).unwrap();
    assert!(vp.contains(b"HideToolbar") && !vp.contains(b"Duplex"), "other preferences stay");
}

#[test]
fn space_audit_shares_the_file_out_by_kind() {
    let mut doc = Document::new_empty();
    let img = image(&mut doc, 300, 300, 3, false, None);
    page(&mut doc, &[("Im0", img)], "q 300 0 0 300 0 0 cm /Im0 Do Q BT ET");
    let bytes = write_full(&doc, &SaveOptions { object_streams: false, ..SaveOptions::default() }).unwrap();
    let doc = Document::open(Arc::new(bytes.clone())).unwrap();
    let audit = audit_space(&doc, bytes.len() as u64);
    assert_eq!(audit.len(), SpaceCategory::ALL.len());
    let get = |c: SpaceCategory| audit.iter().find(|u| u.category == c).unwrap().clone();
    let (images, content, overhead) = (get(SpaceCategory::Images), get(SpaceCategory::ContentStreams), get(SpaceCategory::DocumentOverhead));
    assert!(images.percent > 80.0, "{audit:?}");
    assert!(content.bytes > 0 && overhead.bytes > 0, "{audit:?}");
    assert_eq!(audit.iter().map(|u| u.bytes).sum::<u64>(), bytes.len() as u64);
    assert!((audit.iter().map(|u| u.percent).sum::<f64>() - 100.0).abs() < 1e-6);
}

#[test]
fn space_audit_counts_images_a_content_stream_dictionary_points_at_as_images() {
    // Content added with Edit ▸ Add content records its image in the content stream's
    // dictionary (/PCAdded); the image is still an image, not part of the content stream.
    let mut doc = Document::new_empty();
    let img = image(&mut doc, 300, 300, 3, false, None);
    let p = page(&mut doc, &[("Im0", img)], "q 300 0 0 300 0 0 cm /Im0 Do Q");
    let c = doc.get(p).as_dict().and_then(|d| d.reference(b"Contents")).unwrap();
    let mut added = Dict::new();
    added.set(b"Kind".to_vec(), Object::name("Image"));
    added.set(b"Image".to_vec(), Object::Ref(img));
    doc.update_dict(c, |d| d.set(b"PCAdded".to_vec(), Object::Dict(added))).unwrap();
    let bytes = write_full(&doc, &SaveOptions { object_streams: false, ..SaveOptions::default() }).unwrap();
    let doc = Document::open(Arc::new(bytes.clone())).unwrap();
    let audit = audit_space(&doc, bytes.len() as u64);
    let images = audit.iter().find(|u| u.category == SpaceCategory::Images).unwrap();
    assert!(images.percent > 80.0, "{audit:?}");
}

#[test]
fn invalid_links_and_unreferenced_destinations_go() {
    let mut doc = Document::new_empty();
    let p = page(&mut doc, &[], "BT ET");
    let link = |doc: &mut Document, dest: Object| {
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name("Annot"));
        d.set(b"Subtype".to_vec(), Object::name("Link"));
        d.set(b"Rect".to_vec(), Object::Array(vec![0.into(), 0.into(), 10.into(), 10.into()]));
        d.set(b"Dest".to_vec(), dest);
        Object::Ref(doc.add(Object::Dict(d)))
    };
    let s = |t: &str| Object::String(pdfcraft_cos::PdfString::literal(t.as_bytes().to_vec()));
    let annots = vec![
        link(&mut doc, Object::Array(vec![Object::Ref(p), Object::name("Fit")])),
        link(&mut doc, Object::Array(vec![Object::Ref(ObjRef::new(999, 0)), Object::name("Fit")])),
        link(&mut doc, s("gone")),
        link(&mut doc, s("kept")),
    ];
    doc.update_dict(p, |d| d.set(b"Annots".to_vec(), Object::Array(annots))).unwrap();
    // Named destinations "kept" (used) and "unused"; a bookmark to a missing name.
    let fit = Object::Array(vec![Object::Ref(p), Object::name("Fit")]);
    let mut tree = Dict::new();
    tree.set(b"Names".to_vec(), Object::Array(vec![s("kept"), fit.clone(), s("unused"), fit]));
    let tree = doc.add(Object::Dict(tree));
    let mut names = Dict::new();
    names.set(b"Dests".to_vec(), Object::Ref(tree));
    let outlines = doc.add(Object::Null);
    let mut item = Dict::new();
    item.set(b"Title".to_vec(), s("Lost"));
    item.set(b"Parent".to_vec(), Object::Ref(outlines));
    item.set(b"Dest".to_vec(), s("nowhere"));
    let item = doc.add(Object::Dict(item));
    let mut o = Dict::new();
    o.set(b"First".to_vec(), Object::Ref(item));
    o.set(b"Last".to_vec(), Object::Ref(item));
    doc.set(outlines, Object::Dict(o));
    let root = doc.root().unwrap();
    doc.update_dict(root, |c| {
        c.set(b"Names".to_vec(), Object::Dict(names));
        c.set(b"Outlines".to_vec(), Object::Ref(outlines));
    })
    .unwrap();
    let report = optimize(&mut doc, &Settings::default()).unwrap();
    assert_eq!((report.invalid_links, report.invalid_bookmarks, report.unreferenced_dests), (2, 1, 1));
    let left = doc.get(p).as_dict().unwrap().get(b"Annots").unwrap().as_array().unwrap().len();
    assert_eq!(left, 2);
    assert!(doc.get(item).as_dict().unwrap().get(b"Dest").is_none(), "the bookmark stays, without its broken destination");
    // Off: nothing changes.
    let mut again = doc.clone();
    let off = Settings { remove_invalid_links: false, remove_unreferenced_dests: false, ..Settings::default() };
    assert_eq!(optimize(&mut again, &off).unwrap().invalid_links, 0);
}

#[test]
fn progress_is_reported_per_image_and_a_refusal_cancels() {
    use pdfcraft_optimize::{OptimizeError, Stage, optimize_with_progress};
    let build = || {
        let mut doc = Document::new_empty();
        let a = image(&mut doc, 300, 300, 3, false, None);
        let b = image(&mut doc, 300, 300, 1, false, None);
        let c = image(&mut doc, 300, 300, 3, true, None);
        page(&mut doc, &[("A", a), ("B", b), ("C", c)], "q 72 0 0 72 0 0 cm /A Do Q q 72 0 0 72 100 0 cm /B Do Q q 72 0 0 72 200 0 cm /C Do Q");
        doc
    };
    let mut seen = Vec::new();
    let report = optimize_with_progress(&mut build(), &Settings::default(), &mut |s| {
        seen.push(s);
        true
    })
    .unwrap();
    assert_eq!(report.images, 3);
    assert_eq!(
        seen,
        [
            Stage::Images { done: 0, total: 3 },
            Stage::Images { done: 1, total: 3 },
            Stage::Images { done: 2, total: 3 },
            Stage::Images { done: 3, total: 3 },
            Stage::CleanUp
        ]
    );

    // Stopping after the first image.
    let mut calls = 0;
    let r = optimize_with_progress(&mut build(), &Settings::default(), &mut |_| {
        calls += 1;
        calls < 2
    });
    assert!(matches!(r, Err(OptimizeError::Cancelled)), "{r:?}");
    assert_eq!(calls, 2, "nothing runs after the refusal");
}

// ── Space audit measured in the file ─────────────────────────────────────────────────────────

/// The bytes of object `num` in a classic file: from "num 0 obj" through "endobj\n".
fn object_span(bytes: &[u8], num: u32) -> usize {
    let head = format!("\n{num} 0 obj");
    let start = bytes.windows(head.len()).position(|w| w == head.as_bytes()).unwrap() + 1;
    let end = start + bytes[start..].windows(7).position(|w| w == b"endobj\n").unwrap() + 7;
    end - start
}

#[test]
fn space_audit_measures_each_object_in_the_file() {
    let mut doc = Document::new_empty();
    let img = image(&mut doc, 300, 300, 3, false, None);
    page(&mut doc, &[("Im0", img)], "q 300 0 0 300 0 0 cm /Im0 Do Q BT ET");
    let bytes = write_full(&doc, &SaveOptions { object_streams: false, ..SaveOptions::default() }).unwrap();
    let doc = Document::open(Arc::new(bytes.clone())).unwrap();
    let a = audit_space_detailed(&doc, bytes.len() as u64);
    assert_eq!(a.estimated_objects, 0, "{a:?}");
    assert_eq!(a.apportioned_bytes, 0, "no object streams");
    assert_eq!((a.file_bytes, a.total_bytes), (bytes.len() as u64, bytes.len() as u64));
    assert_eq!(a.rows.iter().map(|r| r.bytes).sum::<u64>(), bytes.len() as u64);
    // The image is exactly its bytes in the file, its data included.
    let image_num =
        (1..20).find(|n| matches!(&*doc.get(ObjRef::new(*n, 0)), Object::Stream(s) if s.dict.name(b"Subtype") == Some(b"Image"))).unwrap();
    let images = a.get(SpaceCategory::Images).unwrap();
    assert_eq!((images.bytes, images.objects), (object_span(&bytes, image_num) as u64, 1), "{a:?}");
    // The cross-reference table and trailer run from "xref" to the end of the file.
    let xref = bytes.windows(6).rposition(|w| w == b"\nxref\n").unwrap() + 1;
    assert_eq!(a.cross_reference_bytes, (bytes.len() - xref) as u64, "{a:?}");
    // The overhead is the cross-reference, the catalog/page tree/info objects and the header.
    let overhead = a.get(SpaceCategory::DocumentOverhead).unwrap().bytes;
    assert_eq!(overhead, a.cross_reference_bytes + a.unclaimed_object_bytes + a.other_overhead_bytes);
    assert!(a.other_overhead_bytes < 64, "only the header and line ends: {a:?}");
}

#[test]
fn space_audit_shares_object_streams_and_counts_every_revision() {
    let mut doc = Document::new_empty();
    let img = image(&mut doc, 200, 200, 3, false, None);
    let p = page(&mut doc, &[("Im0", img)], "q 200 0 0 200 0 0 cm /Im0 Do Q");
    // A font drawn under two names counts once.
    let mut f = Dict::new();
    f.set(b"Type".to_vec(), Object::name("Font"));
    f.set(b"Subtype".to_vec(), Object::name("Type1"));
    f.set(b"BaseFont".to_vec(), Object::name("Helvetica"));
    let font = doc.add(Object::Dict(f));
    let mut fonts = Dict::new();
    fonts.set(b"F1".to_vec(), Object::Ref(font));
    fonts.set(b"F2".to_vec(), Object::Ref(font));
    doc.update_dict(p, |d| {
        if let Some(Object::Dict(res)) = d.get_mut(b"Resources") {
            res.set(b"Font".to_vec(), Object::Dict(fonts));
        }
    })
    .unwrap();
    let bytes = write_full(&doc, &SaveOptions::default()).unwrap();
    let mut doc = Document::open(Arc::new(bytes.clone())).unwrap();
    let a = audit_space_detailed(&doc, bytes.len() as u64);
    assert_eq!(a.estimated_objects, 0, "{a:?}");
    assert!(a.apportioned_bytes > 0, "the font and other dictionaries are in an object stream: {a:?}");
    assert!(a.cross_reference_bytes > 0, "the cross-reference stream: {a:?}");
    assert_eq!(a.get(SpaceCategory::Fonts).unwrap().objects, 1, "{a:?}");
    assert_eq!(a.rows.iter().map(|r| r.bytes).sum::<u64>(), bytes.len() as u64);
    assert!(a.get(SpaceCategory::Images).unwrap().percent > 70.0, "{a:?}");

    // An unsaved edit is estimated; after an incremental update the old content stream stays
    // in the file as a superseded object.
    let page_ref = pdfcraft_model::pages(&doc)[0].obj;
    let c = doc.get(page_ref).as_dict().and_then(|d| d.reference(b"Contents")).unwrap();
    doc.set(c, Object::Stream(Stream::from_raw(Dict::new(), b"q 100 0 0 100 0 0 cm /Im0 Do Q".to_vec())));
    let edited = audit_space_detailed(&doc, bytes.len() as u64);
    assert_eq!(edited.estimated_objects, 1, "{edited:?}");
    let updated = pdfcraft_cos::write_incremental(&doc, &SaveOptions::default()).unwrap();
    let doc = Document::open(Arc::new(updated.clone())).unwrap();
    assert_eq!(doc.revisions().len(), 2);
    let b = audit_space_detailed(&doc, updated.len() as u64);
    assert_eq!(b.estimated_objects, 0, "{b:?}");
    assert_eq!(b.rows.iter().map(|r| r.bytes).sum::<u64>(), updated.len() as u64);
    assert!(b.cross_reference_bytes > a.cross_reference_bytes, "both revisions' cross-references: {a:?} {b:?}");
    assert_eq!(b.get(SpaceCategory::Images).unwrap().bytes, a.get(SpaceCategory::Images).unwrap().bytes, "the image did not change");
}

#[test]
fn space_audit_survives_deeply_nested_forms() {
    // A chain of 50,000 forms each drawing the next: claimed iteratively, never recursively.
    let mut doc = Document::new_empty();
    let mut inner: Option<ObjRef> = None;
    for _ in 0..50_000 {
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name("XObject"));
        d.set(b"Subtype".to_vec(), Object::name("Form"));
        if let Some(i) = inner {
            let mut x = Dict::new();
            x.set(b"X".to_vec(), Object::Ref(i));
            let mut res = Dict::new();
            res.set(b"XObject".to_vec(), Object::Dict(x));
            d.set(b"Resources".to_vec(), Object::Dict(res));
        }
        inner = Some(doc.add(Object::Stream(Stream::from_raw(d, b"/X Do".to_vec()))));
    }
    page(&mut doc, &[("X", inner.unwrap())], "/X Do");
    let a = audit_space_detailed(&doc, 0);
    assert_eq!(a.get(SpaceCategory::XObjectForms).unwrap().objects, 50_000);
}

// ── Fonts panel: unembedding ─────────────────────────────────────────────────────────────────

/// A TrueType font dictionary embedding an OFL font already in the repository.
fn truetype(doc: &mut Document, name: &str, flags: i64, encoding: Option<Object>, widths: bool) -> (ObjRef, ObjRef) {
    let program = include_bytes!("../../../assets/fonts/DancingScript.ttf");
    let mut fd = Dict::new();
    fd.set(b"Length1".to_vec(), Object::Int(program.len() as i64));
    let file = doc.add(Object::Stream(Stream::flate(fd, program)));
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("FontDescriptor"));
    d.set(b"FontName".to_vec(), Object::name(name));
    d.set(b"Flags".to_vec(), Object::Int(flags));
    d.set(b"FontBBox".to_vec(), Object::Array(vec![0.into(), (-200).into(), 1000.into(), 900.into()]));
    d.set(b"ItalicAngle".to_vec(), Object::Int(0));
    d.set(b"Ascent".to_vec(), Object::Int(900));
    d.set(b"Descent".to_vec(), Object::Int(-200));
    d.set(b"CapHeight".to_vec(), Object::Int(700));
    d.set(b"StemV".to_vec(), Object::Int(80));
    d.set(b"FontFile2".to_vec(), Object::Ref(file));
    let descriptor = doc.add(Object::Dict(d));
    let mut f = Dict::new();
    f.set(b"Type".to_vec(), Object::name("Font"));
    f.set(b"Subtype".to_vec(), Object::name("TrueType"));
    f.set(b"BaseFont".to_vec(), Object::name(name));
    if let Some(e) = encoding {
        f.set(b"Encoding".to_vec(), e);
    }
    if widths {
        f.set(b"FirstChar".to_vec(), Object::Int(32));
        f.set(b"LastChar".to_vec(), Object::Int(126));
        f.set(b"Widths".to_vec(), Object::Array((32..=126).map(|_| Object::Int(500)).collect()));
    }
    f.set(b"FontDescriptor".to_vec(), Object::Ref(descriptor));
    (doc.add(Object::Dict(f)), file)
}

fn with_fonts(doc: &mut Document, fonts: &[(&str, ObjRef)]) {
    let mut d = Dict::new();
    for (n, r) in fonts {
        d.set(n.as_bytes().to_vec(), Object::Ref(*r));
    }
    let content = fonts.iter().map(|(n, _)| format!("BT /{n} 12 Tf 72 700 Td (Hello) Tj ET")).collect::<Vec<_>>().join("\n");
    let p = page(doc, &[], &content);
    doc.update_dict(p, |pd| {
        if let Some(Object::Dict(res)) = pd.get_mut(b"Resources") {
            res.set(b"Font".to_vec(), Object::Dict(d));
        }
    })
    .unwrap();
}

#[test]
fn only_simple_complete_standard_encoded_fonts_can_be_unembedded() {
    use pdfcraft_optimize::{UnembedBlock, UnembedError, embedded_fonts, fonts::unembed};
    let mut doc = Document::new_empty();
    let win = Some(Object::name("WinAnsiEncoding"));
    let (ok_font, _) = truetype(&mut doc, "DancingScript", 32, win.clone(), true);
    let (subset, _) = truetype(&mut doc, "ABCDEF+DancingScript", 32, win.clone(), true);
    let (symbolic, _) = truetype(&mut doc, "SymbolicScript", 4, win.clone(), true);
    let (no_enc, _) = truetype(&mut doc, "BareScript", 32, None, true);
    let (no_widths, _) = truetype(&mut doc, "WidthlessScript", 32, win.clone(), false);
    let mut diffs = Dict::new();
    diffs.set(b"BaseEncoding".to_vec(), Object::name("WinAnsiEncoding"));
    diffs.set(b"Differences".to_vec(), Object::Array(vec![Object::Int(65), Object::name("B")]));
    let (diff_font, _) = truetype(&mut doc, "DiffScript", 32, Some(Object::Dict(diffs)), true);
    // A composite font whose CIDFont embeds the program (its descriptor is not used elsewhere).
    let (cid_simple, _) = truetype(&mut doc, "CidScript", 32, win, true);
    let cid_fd = doc.get(cid_simple).as_dict().and_then(|d| d.reference(b"FontDescriptor")).unwrap();
    doc.free(cid_simple);
    let mut cid = Dict::new();
    cid.set(b"Type".to_vec(), Object::name("Font"));
    cid.set(b"Subtype".to_vec(), Object::name("CIDFontType2"));
    cid.set(b"BaseFont".to_vec(), Object::name("CidScript"));
    cid.set(b"FontDescriptor".to_vec(), Object::Ref(cid_fd));
    let cid = doc.add(Object::Dict(cid));
    let mut t0 = Dict::new();
    t0.set(b"Type".to_vec(), Object::name("Font"));
    t0.set(b"Subtype".to_vec(), Object::name("Type0"));
    t0.set(b"BaseFont".to_vec(), Object::name("CidScript-Identity-H"));
    t0.set(b"Encoding".to_vec(), Object::name("Identity-H"));
    t0.set(b"DescendantFonts".to_vec(), Object::Array(vec![Object::Ref(cid)]));
    let t0 = doc.add(Object::Dict(t0));
    with_fonts(&mut doc, &[("F1", ok_font), ("F2", subset), ("F3", symbolic), ("F4", no_enc), ("F5", no_widths), ("F6", diff_font), ("F7", t0)]);

    let listed = embedded_fonts(&doc);
    assert_eq!(listed.len(), 7, "{listed:?}");
    let block = |r: ObjRef| listed.iter().find(|f| f.font == r).unwrap().blocked;
    assert_eq!(block(ok_font), None);
    assert_eq!(block(diff_font), None, "differences on a standard base keep codes meaningful");
    assert_eq!(block(subset), Some(UnembedBlock::Subset));
    assert_eq!(block(symbolic), Some(UnembedBlock::Symbolic));
    assert_eq!(block(no_enc), Some(UnembedBlock::Encoding));
    assert_eq!(block(no_widths), Some(UnembedBlock::Widths));
    assert_eq!(block(t0), Some(UnembedBlock::Composite));
    let f = listed.iter().find(|f| f.font == ok_font).unwrap();
    assert_eq!((f.program, f.subtype.as_str(), f.subset), ("FontFile2", "TrueType", false));
    assert!(f.program_bytes > 10_000, "{f:?}");

    // Asking for a blocked font changes nothing, even with a safe one alongside.
    let before = doc.modified_objects();
    let err = unembed(&mut doc, &["DancingScript".into(), "SymbolicScript".into()]).unwrap_err();
    assert!(matches!(err, UnembedError::Blocked { ref name, .. } if name == "SymbolicScript"), "{err}");
    assert!(matches!(unembed(&mut doc, &["Nope".into()]), Err(UnembedError::NotFound(_))));
    assert_eq!(doc.modified_objects(), before, "refused atomically");
    assert!(embedded_fonts(&doc).iter().any(|f| f.font == ok_font));

    // Unembedding through the optimizer: the program is gone from the written file.
    let full = write_full(&doc, &SaveOptions::default()).unwrap();
    let r = optimize(&mut doc, &Settings { unembed_fonts: vec!["DancingScript".into(), "DiffScript".into()], ..Settings::default() }).unwrap();
    assert_eq!(r.fonts_unembedded, 2);
    let fd = doc.get(ok_font).as_dict().and_then(|d| d.reference(b"FontDescriptor")).unwrap();
    let fd = doc.get(fd).as_dict().cloned().unwrap();
    assert!(!fd.contains(b"FontFile2") && fd.contains(b"FontName") && fd.contains(b"Flags"), "{fd:?}");
    let font_dict = doc.get(ok_font).as_dict().cloned().unwrap();
    assert!(font_dict.contains(b"Widths") && font_dict.contains(b"Encoding"), "metrics and encoding stay");
    let small = write_full(&doc, &SaveOptions::default()).unwrap();
    assert!(small.len() + 2 * 50_000 < full.len(), "{} → {}", full.len(), small.len());
    let reopened = Document::open(Arc::new(small)).unwrap();
    let names: Vec<String> = embedded_fonts(&reopened).into_iter().map(|f| f.name).collect();
    assert!(!names.contains(&"DancingScript".to_string()) && names.contains(&"SymbolicScript".to_string()), "{names:?}");
}

#[test]
fn a_shared_descriptor_is_copied_and_conformance_blocks_unembedding() {
    use pdfcraft_optimize::{UnembedBlock, embedded_fonts, fonts::unembed};
    let mut doc = Document::new_empty();
    let (a, file) = truetype(&mut doc, "DancingScript", 32, Some(Object::name("WinAnsiEncoding")), true);
    // A second font dictionary (another name) sharing the descriptor keeps its program.
    let mut b = doc.get(a).as_dict().cloned().unwrap();
    b.set(b"BaseFont".to_vec(), Object::name("DancingScriptAlias"));
    let b = doc.add(Object::Dict(b));
    with_fonts(&mut doc, &[("F1", a), ("F2", b)]);
    assert_eq!(unembed(&mut doc, &["DancingScript".into()]).unwrap(), 1);
    let fd_of = |doc: &Document, f: ObjRef| doc.get(f).as_dict().and_then(|d| d.reference(b"FontDescriptor")).unwrap();
    assert_ne!(fd_of(&doc, a), fd_of(&doc, b), "a copy for the unembedded font");
    assert_eq!(doc.get(fd_of(&doc, b)).as_dict().and_then(|d| d.reference(b"FontFile2")), Some(file));
    assert!(!doc.get(fd_of(&doc, a)).as_dict().unwrap().contains(b"FontFile2"));

    // A PDF/A claim blocks every font.
    let root = doc.root().unwrap();
    let xmp = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/" pdfaid:part="2" pdfaid:conformance="B"/></rdf:RDF></x:xmpmeta>"#;
    let mut md = Dict::new();
    md.set(b"Type".to_vec(), Object::name("Metadata"));
    md.set(b"Subtype".to_vec(), Object::name("XML"));
    let m = doc.add(Object::Stream(Stream::from_raw(md, xmp.to_vec())));
    doc.update_dict(root, |c| c.set(b"Metadata".to_vec(), Object::Ref(m))).unwrap();
    assert!(embedded_fonts(&doc).iter().all(|f| f.blocked == Some(UnembedBlock::Conformance)));
    assert!(unembed(&mut doc, &["DancingScriptAlias".into()]).is_err());
}

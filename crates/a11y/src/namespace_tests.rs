use std::sync::Arc;

use pdfcraft_cos::{Document, ObjRef, Object};

use super::*;
use crate::{BlockKind, TagChange, figures, headings, reading_blocks, set_tag, tags};

/// A PDF from object bodies (object n = bodies[n - 1]).
fn pdf(bodies: &[String]) -> Document {
    let mut out = b"%PDF-2.0\n".to_vec();
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

/// A tagged page with elements in the PDF 2.0 namespace (20), MathML (21), a custom namespace
/// role-mapped to PDF 2.0 (22) and a namespace whose mappings are circular (23).
fn objects() -> Vec<String> {
    let mut v = vec![String::from("null"); 23];
    let mut set = |n: usize, s: &str| v[n - 1] = s.to_string();
    set(1, "<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 5 0 R /Lang (en) >>");
    set(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    set(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /StructParents 0 >>");
    set(4, &stream("/Aside << /MCID 0 >> BDC EMC /Callout << /MCID 1 >> BDC EMC /P << /MCID 2 >> BDC EMC /H1 << /MCID 3 >> BDC EMC"));
    set(5, "<< /Type /StructTreeRoot /K 6 0 R /Namespaces [20 0 R 21 0 R 22 0 R 23 0 R] /RoleMap << /Para /P >> >>");
    set(6, "<< /Type /StructElem /S /Document /NS 20 0 R /K [7 0 R 8 0 R 9 0 R 10 0 R 11 0 R 12 0 R 13 0 R] >>");
    set(7, "<< /Type /StructElem /S /Aside /NS 20 0 R /P 6 0 R /Pg 3 0 R /K 0 >>");
    set(8, "<< /Type /StructElem /S /Callout /NS 22 0 R /P 6 0 R /Pg 3 0 R /K 1 >>");
    set(9, "<< /Type /StructElem /S /math /NS 21 0 R /P 6 0 R >>");
    set(10, "<< /Type /StructElem /S /A /NS 23 0 R /P 6 0 R >>");
    set(11, "<< /Type /StructElem /S /Para /P 6 0 R /Pg 3 0 R /K 2 >>");
    set(12, "<< /Type /StructElem /S /Heading /NS 22 0 R /P 6 0 R /Pg 3 0 R /K 3 /ActualText (Results) >>");
    set(13, "<< /Type /StructElem /S /Pic /NS 22 0 R /P 6 0 R /Alt (A chart) >>");
    set(20, "<< /Type /Namespace /NS (http://iso.org/pdf2/ssn) >>");
    set(21, "<< /Type /Namespace /NS (http://www.w3.org/1998/Math/MathML) >>");
    set(
        22,
        "<< /Type /Namespace /NS (http://example.com/report) /Schema << /Type /Filespec /F (report.xsd) /UF (report.xsd) >> \
         /RoleMapNS << /Callout [/Aside 20 0 R] /Heading [/H1 20 0 R] /Pic [/Figure 20 0 R] /Old /Note >> >>",
    );
    set(23, "<< /Type /Namespace /NS (http://example.com/loop) /RoleMapNS << /A [/B 23 0 R] /B [/A 23 0 R] >> >>");
    v
}

fn r(n: u32) -> ObjRef {
    ObjRef::new(n, 0)
}

fn node(t: &crate::Tags, obj: u32) -> &crate::TagNode {
    &t.nodes[t.find(r(obj)).unwrap()]
}

#[test]
fn tags_report_namespaces_and_standard_types_across_namespaces() {
    let doc = pdf(&objects());
    let t = tags(&doc);
    let custom = "http://example.com/report";
    let aside = node(&t, 7);
    assert_eq!(
        (aside.standard.as_deref(), aside.namespace.as_deref(), aside.standard_namespace.as_deref()),
        (Some("Aside"), Some(NS_PDF20), Some(NS_PDF20))
    );
    let callout = node(&t, 8);
    assert_eq!(
        (callout.standard.as_deref(), callout.namespace.as_deref(), callout.standard_namespace.as_deref()),
        (Some("Aside"), Some(custom), Some(NS_PDF20))
    );
    let math = node(&t, 9);
    assert_eq!((math.standard.as_deref(), math.namespace.as_deref()), (Some("math"), Some(NS_MATHML)));
    assert_eq!(node(&t, 10).standard, None, "a RoleMapNS cycle maps to nothing");
    let para = node(&t, 11);
    assert_eq!((para.standard.as_deref(), para.namespace.as_deref(), para.standard_namespace.as_deref()), (Some("P"), None, None));
    assert!(!para.namespace_missing);

    assert_eq!(t.namespaces.len(), 4);
    let kinds: Vec<NamespaceKind> = t.namespaces.iter().map(|n| n.kind).collect();
    assert_eq!(kinds, [NamespaceKind::Pdf20, NamespaceKind::MathMl, NamespaceKind::Other, NamespaceKind::Other]);
    let ns = &t.namespaces[2];
    assert_eq!((ns.uri.as_str(), ns.schema.as_deref(), ns.listed), (custom, Some("report.xsd"), true));
    assert_eq!(
        ns.role_map,
        [
            NsMapping { from: "Callout".into(), to: "Aside".into(), namespace: Some(NS_PDF20.into()) },
            NsMapping { from: "Heading".into(), to: "H1".into(), namespace: Some(NS_PDF20.into()) },
            NsMapping { from: "Old".into(), to: "Note".into(), namespace: None },
            NsMapping { from: "Pic".into(), to: "Figure".into(), namespace: Some(NS_PDF20.into()) },
        ]
    );
    assert_eq!(namespaces(&doc), t.namespaces);
}

#[test]
fn headings_reading_order_and_figures_honour_namespaces() {
    let doc = pdf(&objects());
    let t = tags(&doc);
    let h = headings(&t);
    assert_eq!(h.len(), 1);
    assert_eq!((h[0].node, h[0].level, h[0].text.as_deref()), (t.find(r(12)).unwrap(), 1, Some("Results")));
    let blocks = reading_blocks(&t);
    assert!(blocks.iter().any(|b| b.kind == BlockKind::Heading(1) && b.node == t.find(r(12))), "{blocks:#?}");
    assert!(blocks.iter().any(|b| b.kind == BlockKind::Paragraph && b.node == t.find(r(8))), "Callout reads as an Aside: {blocks:#?}");
    let f = figures(&doc);
    assert_eq!(f.iter().map(|f| f.obj).collect::<Vec<_>>(), [r(13)], "Pic maps to Figure in the PDF 2.0 namespace");
    // The full check's structure tree resolves the same way.
    let tree = crate::structure::Tree::read(&doc, &pdfcraft_model::pages(&doc));
    let tys: Vec<&[u8]> = tree.elems.iter().map(|e| e.ty.as_slice()).collect();
    assert_eq!(tys, [&b"Document"[..], b"Aside", b"Aside", b"math", b"A", b"P", b"H1", b"Figure"]);
}

#[test]
fn a_missing_namespace_object_reads_in_the_default_namespace() {
    let mut objs = objects();
    objs[10] = "<< /Type /StructElem /S /P /NS 99 0 R /P 6 0 R >>".into();
    objs[6] = "<< /Type /StructElem /S /Aside /NS (not a reference) /P 6 0 R >>".into();
    let doc = pdf(&objs);
    let t = tags(&doc);
    let p = node(&t, 11);
    assert!(p.namespace_missing);
    assert_eq!((p.standard.as_deref(), p.namespace.as_deref()), (Some("P"), None));
    assert!(node(&t, 7).namespace_missing);
    let resolver = Resolver::for_doc(&doc);
    let strict = resolver.resolve(&doc, b"P", NsRef::Obj(r(99)), Mode::Pdf20);
    assert_eq!(strict.outcome, Outcome::MissingNamespace(NsRef::Obj(r(99))));
}

#[test]
fn set_tag_moves_an_element_between_namespaces() {
    let mut doc = pdf(&objects());
    // Para (default namespace) becomes an Aside in the PDF 2.0 namespace already listed.
    set_tag(&mut doc, r(11), &TagChange { ty: Some("Aside".into()), namespace: Some(Some("pdf-2.0".into())), ..Default::default() }).unwrap();
    assert_eq!(doc.get(r(11)).as_dict().unwrap().get(b"NS"), Some(&Object::Ref(r(20))));
    assert_eq!(namespaces(&doc).len(), 4, "the existing namespace is reused");
    // A type of a custom namespace, mapped there.
    set_tag(
        &mut doc,
        r(7),
        &TagChange { ty: Some("Heading".into()), namespace: Some(Some("http://example.com/report".into())), ..Default::default() },
    )
    .unwrap();
    let t = tags(&doc);
    assert_eq!((node(&t, 7).standard.as_deref(), node(&t, 7).standard_namespace.as_deref()), (Some("H1"), Some(NS_PDF20)));
    // Back to the default namespace.
    set_tag(&mut doc, r(7), &TagChange { ty: Some("P".into()), namespace: Some(None), ..Default::default() }).unwrap();
    assert!(doc.get(r(7)).as_dict().unwrap().get(b"NS").is_none());

    // Refused, and nothing changes: a type unmapped in a new namespace, a cycle, Artifact in
    // the default namespace, a bad URI.
    let before = tags(&doc);
    let bad = [
        TagChange { namespace: Some(Some("http://example.com/new".into())), ..Default::default() },
        TagChange { ty: Some("A".into()), namespace: Some(Some("http://example.com/loop".into())), ..Default::default() },
        TagChange { ty: Some("Artifact".into()), namespace: Some(None), ..Default::default() },
        TagChange { ty: Some("Bogus".into()), ..Default::default() },
        TagChange { namespace: Some(Some("http://exa mple.com".into())), ..Default::default() },
        TagChange { namespace: Some(Some("x".repeat(5000))), ..Default::default() },
        TagChange { ty: Some("Has Space".into()), namespace: Some(Some("pdf-2.0".into())), ..Default::default() },
    ];
    for change in &bad {
        assert!(matches!(set_tag(&mut doc, r(11), change), Err(TagError::Invalid(_))), "{change:?}");
    }
    assert_eq!(tags(&doc), before);
    // Artifact is a PDF 2.0 element type.
    set_tag(&mut doc, r(13), &TagChange { ty: Some("Artifact".into()), namespace: Some(Some(NS_PDF20.into())), ..Default::default() }).unwrap();
    assert_eq!(node(&tags(&doc), 13).standard.as_deref(), Some("Artifact"));
}

#[test]
fn add_namespace_with_role_mappings_and_round_trip() {
    let mut doc = pdf(&objects());
    let spec = NamespaceSpec {
        uri: "http://example.com/new".into(),
        schema: Some("new.xsd".into()),
        role_map: vec![
            NsMapping { from: "Box".into(), to: "Sect".into(), namespace: Some("pdf-2.0".into()) },
            NsMapping { from: "Inner".into(), to: "Box".into(), namespace: Some("http://example.com/new".into()) },
            NsMapping { from: "Plain".into(), to: "P".into(), namespace: None },
            NsMapping { from: "Formula".into(), to: "math".into(), namespace: Some("mathml".into()) },
        ],
    };
    let ns = add_namespace(&mut doc, &spec).unwrap();
    let list = namespaces(&doc);
    assert_eq!(list.len(), 5);
    let added = list.iter().find(|n| n.obj == ns).unwrap();
    assert_eq!((added.uri.as_str(), added.kind, added.schema.as_deref()), ("http://example.com/new", NamespaceKind::Other, Some("new.xsd")));
    assert_eq!(added.role_map.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(), ["Box", "Formula", "Inner", "Plain"]);
    set_tag(&mut doc, r(11), &TagChange { ty: Some("Inner".into()), namespace: Some(Some("http://example.com/new".into())), ..Default::default() })
        .unwrap();
    let t = tags(&doc);
    assert_eq!((node(&t, 11).standard.as_deref(), node(&t, 11).standard_namespace.as_deref()), (Some("Sect"), Some(NS_PDF20)));

    // Changing the namespace again merges; an empty target removes a mapping.
    add_namespace(
        &mut doc,
        &NamespaceSpec {
            uri: "http://example.com/new".into(),
            role_map: vec![NsMapping { from: "Plain".into(), ..Default::default() }],
            ..Default::default()
        },
    )
    .unwrap();
    let added = namespaces(&doc).into_iter().find(|n| n.obj == ns).unwrap();
    assert_eq!(added.role_map.len(), 3);

    // Save and reopen: namespaces, /NS and the resolved types survive.
    let bytes = pdfcraft_cos::write_full(&doc, &Default::default()).unwrap();
    let re = Document::open(Arc::new(bytes)).unwrap();
    let (a, b) = (tags(&doc), tags(&re));
    assert_eq!(
        a.namespaces.iter().map(|n| (&n.uri, &n.role_map, &n.schema)).collect::<Vec<_>>(),
        b.namespaces.iter().map(|n| (&n.uri, &n.role_map, &n.schema)).collect::<Vec<_>>()
    );
    let summary = |t: &crate::Tags| {
        t.nodes.iter().map(|n| (n.ty.clone(), n.standard.clone(), n.namespace.clone(), n.standard_namespace.clone())).collect::<Vec<_>>()
    };
    assert_eq!(summary(&a), summary(&b));
}

#[test]
fn add_namespace_refuses_cycles_dead_ends_and_hostile_input() {
    let mut doc = pdf(&objects());
    let before = namespaces(&doc);
    let m = |from: &str, to: &str, ns: Option<&str>| NsMapping { from: from.into(), to: to.into(), namespace: ns.map(str::to_owned) };
    let own = "http://example.com/c";
    let bad = [
        NamespaceSpec { uri: own.into(), role_map: vec![m("X", "Y", Some(own)), m("Y", "X", Some(own))], ..Default::default() },
        NamespaceSpec { uri: own.into(), role_map: vec![m("X", "Nothing", None)], ..Default::default() },
        NamespaceSpec { uri: own.into(), role_map: vec![m("X", "Undefined", Some("http://example.com/elsewhere"))], ..Default::default() },
        NamespaceSpec { uri: "pdf-2.0".into(), role_map: vec![m("P", "Span", Some("pdf-2.0"))], ..Default::default() },
        NamespaceSpec { uri: "".into(), ..Default::default() },
        NamespaceSpec { uri: "   ".into(), ..Default::default() },
        NamespaceSpec { uri: "http://a b".into(), ..Default::default() },
        NamespaceSpec { uri: format!("http://{}", "x".repeat(4000)), ..Default::default() },
        NamespaceSpec { uri: own.into(), role_map: vec![m("Has Space", "P", None)], ..Default::default() },
        NamespaceSpec { uri: own.into(), role_map: vec![m("", "P", None)], ..Default::default() },
        NamespaceSpec { uri: own.into(), role_map: vec![m("X", "/P", None)], ..Default::default() },
        NamespaceSpec { uri: own.into(), role_map: vec![m(&"L".repeat(200), "P", None)], ..Default::default() },
        NamespaceSpec { uri: own.into(), role_map: vec![m("X", "P", Some("bad uri"))], ..Default::default() },
        NamespaceSpec { uri: own.into(), role_map: (0..=MAX_NS_MAPPINGS).map(|i| m(&format!("T{i}"), "P", None)).collect(), ..Default::default() },
        NamespaceSpec { uri: own.into(), schema: Some("s".repeat(3000)), ..Default::default() },
    ];
    for spec in &bad {
        assert!(matches!(add_namespace(&mut doc, spec), Err(TagError::Invalid(_))), "{spec:?}");
    }
    assert_eq!(namespaces(&doc), before, "refusals change nothing");

    // An untagged document, and a structure tree root written directly in the catalog.
    let mut objs = objects();
    objs[0] = "<< /Type /Catalog /Pages 2 0 R >>".into();
    let mut untagged = pdf(&objs);
    assert_eq!(add_namespace(&mut untagged, &NamespaceSpec { uri: own.into(), ..Default::default() }), Err(TagError::Untagged));
    objs[0] = "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot << /Type /StructTreeRoot >> >>".into();
    let mut direct = pdf(&objs);
    assert!(matches!(add_namespace(&mut direct, &NamespaceSpec { uri: own.into(), ..Default::default() }), Err(TagError::Invalid(_))));

    // /Namespaces as an indirect array is extended in place.
    let mut objs = objects();
    objs[4] = "<< /Type /StructTreeRoot /K 6 0 R /Namespaces 14 0 R >>".into();
    objs[13] = "[20 0 R 21 0 R 22 0 R 23 0 R]".into();
    let mut doc = pdf(&objs);
    add_namespace(&mut doc, &NamespaceSpec { uri: own.into(), ..Default::default() }).unwrap();
    assert_eq!(doc.get(r(14)).as_array().map(Vec::len), Some(5));
}

#[test]
fn hostile_namespace_structures_do_not_panic() {
    let variants: Vec<(usize, &str)> = vec![
        (5, "<< /Type /StructTreeRoot /K 6 0 R /Namespaces 7 >>"),
        (5, "<< /Type /StructTreeRoot /K 6 0 R /Namespaces << /A 1 >> >>"),
        (5, "<< /Type /StructTreeRoot /K 6 0 R /Namespaces [1 0 R 5 0 R 6 0 R null (x) 99 0 R] >>"),
        (20, "<< /Type /NotNamespace /NS (http://iso.org/pdf2/ssn) >>"),
        (20, "<< /Type /Namespace /NS 42 >>"),
        (20, "<< /Type /Namespace /NS () >>"),
        (22, "<< /Type /Namespace /NS (http://example.com/report) /RoleMapNS 3 >>"),
        (
            22,
            "<< /Type /Namespace /NS (http://example.com/report) /RoleMapNS << /Callout [] /Heading [5] /Pic [/Figure (x)] /Old [/P 22 0 R /extra] >> >>",
        ),
        (22, "<< /Type /Namespace /NS (http://example.com/report) /Schema 5 /RoleMapNS << /Callout [/Callout 22 0 R] >> >>"),
        (8, "<< /Type /StructElem /S /Callout /NS [22 0 R] >>"),
        (8, "<< /Type /StructElem /S /Callout /NS 8 0 R >>"),
    ];
    for (n, body) in variants {
        let mut objs = objects();
        objs[n - 1] = body.into();
        let mut doc = pdf(&objs);
        let t = tags(&doc);
        let _ = (headings(&t), reading_blocks(&t), figures(&doc), namespaces(&doc));
        let _ = crate::verify_pdfua(&doc, &crate::UaOptions { part: crate::UaPart::Ua2, ..Default::default() });
        let _ = crate::check(&doc, &crate::Options::default());
        let _ = add_namespace(
            &mut doc,
            &NamespaceSpec {
                uri: "http://example.com/report".into(),
                role_map: vec![NsMapping { from: "Z".into(), to: "P".into(), namespace: None }],
                ..Default::default()
            },
        );
        let _ = set_tag(&mut doc, r(8), &TagChange { namespace: Some(Some("pdf-2.0".into())), ty: Some("P".into()), ..Default::default() });
    }
}

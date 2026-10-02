use std::sync::Arc;

use printcraft_cos::{Document, Object, SaveOptions, write_incremental};

use super::*;

/// One page with a form: text fields (plain, multiline, comb, password, read-only, nested),
/// a check box with appearances, one without, a radio group, a combo and a multi-select list,
/// and a push button.
fn fixture() -> Document {
    let objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>".into(),                                             // 1
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 600 800] >>".into(),                              // 2
        "<< /Type /Page /Parent 2 0 R /Annots [10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 16 0 R 17 0 R 21 0 R 22 0 R 23 0 R 24 0 R 25 0 R 26 0 R] >>".into(), // 3
        "<< /Fields [10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 15 0 R 17 0 R 20 0 R 23 0 R 24 0 R 25 0 R 26 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 5 0 R /ZaDb 6 0 R >> >> >>".into(), // 4
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),               // 5
        "<< /Type /Font /Subtype /Type1 /BaseFont /ZapfDingbats >>".into(),                                      // 6
        "<< /Length 0 >>\nstream\n\nendstream".into(),                                                            // 7 (empty appearance)
        "<< /Length 0 >>\nstream\n\nendstream".into(),                                                            // 8
        "null".into(),                                                                                            // 9
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [50 700 250 720] /P 3 0 R /V (Ada) /DV (Ada) /MK << /BG [1 1 0.9] /BC [0 0 0] >> >>".into(), // 10
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (notes) /Ff 4096 /DA (/Helv 10 Tf 0 0 1 rg) /Rect [50 600 250 680] /P 3 0 R >>".into(), // 11
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (zip) /Ff 16777216 /MaxLen 5 /Rect [50 560 150 580] /P 3 0 R >>".into(), // 12
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (pin) /Ff 8192 /Rect [50 530 150 550] /P 3 0 R >>".into(), // 13
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (id) /Ff 1 /V (A-1) /Rect [50 500 150 520] /P 3 0 R >>".into(), // 14
        "<< /T (address) /Kids [16 0 R] >>".into(),                                                               // 15
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (city) /Parent 15 0 R /Q 1 /Rect [50 470 250 490] /P 3 0 R >>".into(), // 16
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /V /Off /AS /Off /Rect [300 700 315 715] /P 3 0 R /AP << /N << /Yes 7 0 R /Off 8 0 R >> >> >>".into(), // 17
        "null".into(),                                                                                            // 18
        "null".into(),                                                                                            // 19
        "<< /FT /Btn /T (size) /Ff 49152 /V /Off /DV /S /Kids [21 0 R 22 0 R] >>".into(),                        // 20 radio, NoToggleToOff
        "<< /Type /Annot /Subtype /Widget /Parent 20 0 R /AS /Off /Rect [300 650 315 665] /P 3 0 R /AP << /N << /S 7 0 R /Off 8 0 R >> >> >>".into(), // 21
        "<< /Type /Annot /Subtype /Widget /Parent 20 0 R /AS /Off /Rect [330 650 345 665] /P 3 0 R /AP << /N << /L 7 0 R /Off 8 0 R >> >> >>".into(), // 22
        "<< /Type /Annot /Subtype /Widget /FT /Ch /T (country) /Ff 131072 /Opt [[(ca) (Canada)] [(fr) (France)]] /Rect [300 600 450 620] /P 3 0 R >>".into(), // 23
        "<< /Type /Annot /Subtype /Widget /FT /Ch /T (toppings) /Ff 2097152 /Opt [(Cheese) (Ham) (Olives)] /Rect [300 500 450 580] /P 3 0 R >>".into(), // 24
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (go) /Ff 65536 /Rect [300 450 380 470] /P 3 0 R >>".into(), // 25
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (bare) /Rect [400 700 415 715] /P 3 0 R /Foo (kept) >>".into(), // 26 check box without AP
    ];
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
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    Document::open(Arc::new(out)).expect("opens")
}

fn reopen(doc: &Document) -> Document {
    let bytes = write_incremental(doc, &SaveOptions::default()).expect("writes");
    hayro_syntax::Pdf::new(bytes.clone()).expect("hayro-syntax parses the output");
    Document::open(Arc::new(bytes)).expect("reopens")
}

fn field<'a>(all: &'a [Field], name: &str) -> &'a Field {
    all.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("no field {name}"))
}

fn ap(doc: &Document, w: &Widget) -> String {
    let wd = doc.get(w.obj).as_dict().cloned().unwrap();
    let n = wd.get(b"AP").unwrap().as_dict().unwrap().reference(b"N").expect("normal appearance stream");
    let Object::Stream(s) = &*doc.get(n) else { panic!() };
    String::from_utf8_lossy(&s.decoded().unwrap()).into_owned()
}

#[test]
fn the_field_tree_is_read_with_inheritance() {
    let doc = fixture();
    let all = fields(&doc);
    let names: Vec<&str> = all.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["name", "notes", "zip", "pin", "id", "address.city", "agree", "size", "country", "toppings", "go", "bare"]);
    let name = field(&all, "name");
    assert_eq!((name.kind, name.value.as_slice(), name.da.as_str()), (FieldKind::Text, &["Ada".to_string()][..], "/Helv 0 Tf 0 g"));
    assert_eq!(name.widgets[0].page, Some(0));
    assert_eq!(field(&all, "zip").max_len, Some(5));
    assert!(field(&all, "id").read_only());
    let size = field(&all, "size");
    assert_eq!(size.kind, FieldKind::Radio);
    assert_eq!(size.widgets.iter().map(|w| w.on_state.clone().unwrap()).collect::<Vec<_>>(), ["S", "L"]);
    assert!(size.value.is_empty(), "/Off is no value");
    assert_eq!(size.default, ["S"]);
    assert_eq!(field(&all, "country").options, [("ca".into(), "Canada".into()), ("fr".into(), "France".into())]);
    assert_eq!(field(&all, "toppings").kind, FieldKind::List);
    assert_eq!(field(&all, "go").kind, FieldKind::PushButton);
    assert_eq!(field(&all, "address.city").quadding, 1);
}

#[test]
fn text_fields_get_new_appearances() {
    let mut doc = fixture();
    set_value(&mut doc, "name", &FieldValue::Text("Grace (Hopper)".into())).unwrap();
    set_value(&mut doc, "notes", &FieldValue::Text("A long note that has to wrap over several lines of the box".into())).unwrap();
    set_value(&mut doc, "zip", &FieldValue::Text("12345".into())).unwrap();
    set_value(&mut doc, "pin", &FieldValue::Text("1234".into())).unwrap();
    set_value(&mut doc, "address.city", &FieldValue::Text("Zürich".into())).unwrap();
    let doc = reopen(&doc);
    let all = fields(&doc);
    assert_eq!(field(&all, "name").value, ["Grace (Hopper)"]);
    let name_ap = ap(&doc, &field(&all, "name").widgets[0]);
    assert!(name_ap.contains("/Tx BMC") && name_ap.contains("EMC"), "{name_ap}");
    assert!(name_ap.contains("(Grace \\(Hopper\\)) Tj"), "{name_ap}");
    assert!(name_ap.contains("1 1 0.9 rg"), "background from /MK: {name_ap}");
    let notes = ap(&doc, &field(&all, "notes").widgets[0]);
    assert!(notes.matches(" Tj").count() >= 2, "wrapped: {notes}");
    assert!(notes.contains("/Helv 10 Tf") && notes.contains("0 0 1 rg"));
    let zip = ap(&doc, &field(&all, "zip").widgets[0]);
    assert_eq!(zip.matches(" Tj").count(), 5, "one cell per character: {zip}");
    let pin = ap(&doc, &field(&all, "pin").widgets[0]);
    assert!(pin.contains("(****) Tj") && !pin.contains("1234"), "{pin}");
    // WinAnsi bytes for non-ASCII text.
    let w = &field(&all, "address.city").widgets[0];
    let n = doc.get(w.obj).as_dict().unwrap().get(b"AP").unwrap().as_dict().unwrap().reference(b"N").unwrap();
    let Object::Stream(s) = &*doc.get(n) else { panic!() };
    let raw = s.decoded().unwrap();
    assert!(raw.windows(8).any(|x| x == b"(Z\xfcrich)"));
}

#[test]
fn check_boxes_and_radios_switch_states() {
    let mut doc = fixture();
    set_value(&mut doc, "agree", &FieldValue::Check(true)).unwrap();
    set_value(&mut doc, "size", &FieldValue::Radio(Some("L".into()))).unwrap();
    set_value(&mut doc, "bare", &FieldValue::Text("yes".into())).unwrap();
    let doc2 = reopen(&doc);
    let all = fields(&doc2);
    assert_eq!(field(&all, "agree").value, ["Yes"]);
    assert_eq!(field(&all, "agree").widgets[0].state.as_deref(), Some("Yes"));
    let size = field(&all, "size");
    assert_eq!(size.value, ["L"]);
    assert_eq!(size.widgets.iter().map(|w| w.state.clone().unwrap()).collect::<Vec<_>>(), ["Off", "L"]);
    // The bare check box got PrintCraft's own appearances and kept its other keys.
    let bare = field(&all, "bare");
    assert_eq!(bare.value, ["Yes"]);
    assert_eq!(bare.widgets[0].on_state.as_deref(), Some("Yes"));
    assert!(doc2.get(bare.widgets[0].obj).as_dict().unwrap().contains(b"Foo"));
    let mut doc = doc2;
    // Radio validation and NoToggleToOff.
    assert!(matches!(set_value(&mut doc, "size", &FieldValue::Radio(Some("XL".into()))), Err(FormError::Invalid(_))));
    assert!(matches!(set_value(&mut doc, "size", &FieldValue::Radio(None)), Err(FormError::Invalid(_))));
    set_value(&mut doc, "agree", &FieldValue::Check(false)).unwrap();
    assert!(field(&fields(&doc), "agree").value.is_empty());
}

#[test]
fn choices_accept_exports_or_display_text() {
    let mut doc = fixture();
    set_value(&mut doc, "country", &FieldValue::Text("France".into())).unwrap();
    set_value(&mut doc, "toppings", &FieldValue::Choice(vec!["Ham".into(), "Olives".into()])).unwrap();
    let doc = reopen(&doc);
    let all = fields(&doc);
    let country = field(&all, "country");
    assert_eq!(country.value, ["fr"]);
    assert_eq!(country.display_value(), "France");
    assert!(ap(&doc, &country.widgets[0]).contains("(France) Tj"));
    let toppings = field(&all, "toppings");
    assert_eq!(toppings.value, ["Ham", "Olives"]);
    let list = ap(&doc, &toppings.widgets[0]);
    assert_eq!(list.matches("0.6 0.75 0.86 rg").count(), 2, "two highlighted rows: {list}");
    assert_eq!(doc.get(toppings.obj).as_dict().unwrap().get(b"I").unwrap().as_array().unwrap().len(), 2);
    let mut doc = doc;
    assert!(matches!(set_value(&mut doc, "country", &FieldValue::Text("Spain".into())), Err(FormError::Invalid(_))));
    assert!(matches!(set_value(&mut doc, "country", &FieldValue::Choice(vec!["ca".into(), "fr".into()])), Err(FormError::Invalid(_))));
}

#[test]
fn errors_are_specific_and_change_nothing() {
    let mut doc = fixture();
    assert_eq!(set_value(&mut doc, "nope", &FieldValue::Text("x".into())), Err(FormError::NoSuchField("nope".into())));
    assert_eq!(set_value(&mut doc, "id", &FieldValue::Text("x".into())), Err(FormError::ReadOnly("id".into())));
    assert!(matches!(set_value(&mut doc, "zip", &FieldValue::Text("123456".into())), Err(FormError::Invalid(_))));
    assert!(matches!(set_value(&mut doc, "go", &FieldValue::Text("x".into())), Err(FormError::Invalid(_))));
    assert!(matches!(set_value(&mut doc, "agree", &FieldValue::Text("maybe".into())), Err(FormError::Invalid(_))));
    assert!(!doc.is_modified());
}

#[test]
fn reset_restores_defaults() {
    let mut doc = fixture();
    set_value(&mut doc, "name", &FieldValue::Text("Grace".into())).unwrap();
    set_value(&mut doc, "agree", &FieldValue::Check(true)).unwrap();
    set_value(&mut doc, "size", &FieldValue::Radio(Some("L".into()))).unwrap();
    assert_eq!(reset(&mut doc, Some(&["agree".to_string()])).unwrap(), 1);
    assert!(field(&fields(&doc), "agree").value.is_empty());
    assert_eq!(field(&fields(&doc), "name").value, ["Grace"], "only the named field");
    let n = reset(&mut doc, None).unwrap();
    assert!(n >= 2);
    let all = fields(&reopen(&doc));
    assert_eq!(field(&all, "name").value, ["Ada"]);
    assert_eq!(field(&all, "size").value, ["S"], "back to /DV");
    assert!(matches!(reset(&mut doc, Some(&["nope".to_string()])), Err(FormError::NoSuchField(_))));
}

#[test]
fn documents_without_forms_say_so() {
    let mut doc = Document::new_empty();
    assert!(fields(&doc).is_empty());
    assert_eq!(set_value(&mut doc, "x", &FieldValue::Text("y".into())), Err(FormError::NoForm));
}

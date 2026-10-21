use super::*;
use std::sync::Arc;

fn form() -> (Session, DocId) {
    let mut s = Session::new().with_clock(|| 1_700_000_000);
    let id = s.open("script-budget.pdf", None, Arc::new(crate::tests::fixture(1)), None).unwrap();
    s.apply(
        id,
        Edit::AddField {
            page: 0,
            rect: [20.0, 20.0, 180.0, 40.0],
            kind: pdfcraft_forms::NewField::Text { multiline: false },
            name: Some("value".into()),
        },
    )
    .unwrap();
    (s, id)
}

#[test]
fn automatic_field_batch_stops_after_refusal_and_a_new_batch_remains_usable() {
    let (s, id) = form();
    let cos = &s.get(id).unwrap().editor.as_ref().unwrap().cos;
    let fields = pdfcraft_forms::fields(cos);
    let field = &fields[0];
    let mut runner = JsRunner::new(cos, "script-budget.pdf");
    let oversized = " ".repeat(256 * 1024 + 1);
    let first = runner.run(FieldEvent::Validate, &oversized, field, "original", &fields);
    assert_eq!(first.value, "original");
    assert!(runner.output.limited);
    let next = runner.run(FieldEvent::Calculate, "event.value='must not run'", field, "original", &fields);
    assert_eq!(next.value, "original");
    assert!(next.changes.is_empty());

    let mut explicit = JsRunner::new(cos, "script-budget.pdf");
    explicit.run(FieldEvent::Validate, "event.value = ;", field, "original", &fields);
    assert!(!explicit.output.limited, "ordinary syntax errors do not stop later field scripts");
    assert_eq!(explicit.run(FieldEvent::Calculate, "event.value='ready'", field, "original", &fields).value, "ready");
}

#[test]
fn widget_metadata_without_names_is_refused_before_copying_field_state() {
    let (s, id) = form();
    let cos = &s.get(id).unwrap().editor.as_ref().unwrap().cos;
    let mut fields = pdfcraft_forms::fields(cos);
    let mut widget = fields[0].widgets[0].clone();
    widget.on_state = None;
    fields[0].widgets = vec![widget; budget::SNAPSHOT_ITEMS];
    let mut runner = JsRunner::new(cos, "script-budget.pdf");
    let out = runner.run(FieldEvent::Calculate, "event.value='must not run'", &fields[0], "original", &fields);
    assert_eq!(out.value, "original");
    assert!(runner.output.limited && runner.output.errors.iter().any(|e| e == budget::SNAPSHOT_NOTICE));
}

#[test]
fn refused_action_stops_the_chain_without_replaying_and_keeps_later_explicit_calls() {
    use pdfcraft_forms::{FieldAction, FieldProps, Trigger};
    let (mut s, id) = form();
    let props = FieldProps {
        actions: Some(vec![
            (Trigger::MouseUp, FieldAction::JavaScript(" ".repeat(256 * 1024 + 1))),
            (Trigger::MouseUp, FieldAction::JavaScript("getField('value').value='must not run'".into())),
        ]),
        ..Default::default()
    };
    s.apply(id, Edit::SetFieldProps { name: "value".into(), props: Box::new(props) }).unwrap();
    let before = s.get(id).unwrap().form[0].value.clone();
    let out = s.run_field_trigger(id, "value", Trigger::MouseUp).unwrap();
    assert!(out.limited);
    assert_eq!(out.ran.len(), 1);
    assert_eq!(s.get(id).unwrap().form[0].value, before);
    assert_eq!(s.run_javascript(id, "6*7", None).unwrap().result.as_deref(), Some("42"));
}

#[test]
fn document_output_across_repeated_events_stays_bounded_until_taken() {
    use pdfcraft_forms::DocTrigger;
    let (mut s, id) = form();
    s.apply(id, Edit::SetDocumentAction { trigger: DocTrigger::WillSave, script: Some("console.println('x'.repeat(262144))".into()) }).unwrap();
    for _ in 0..32 {
        assert!(s.run_document_action(id, DocTrigger::WillSave).unwrap());
    }
    let out = s.take_js_output(id);
    assert!(out.limited);
    assert!(out.console.iter().chain(&out.alerts).chain(&out.errors).map(String::len).sum::<usize>() <= MAX_OUTPUT_BYTES);
    assert!(out.console.len() + out.alerts.len() + out.errors.len() + out.requests.len() <= MAX_OUTPUT_ITEMS);
    assert_eq!(out.errors.len(), 1, "one concise retention notice");
    s.run_document_action(id, DocTrigger::WillSave).unwrap();
    let next = s.take_js_output(id);
    assert!(!next.limited && next.errors.is_empty());
    assert_eq!(next.console.len(), 1);
}

#[test]
fn wizard_refusal_stops_later_steps_but_syntax_errors_remain_reported() {
    use crate::actions::{Action, Step, run_on};
    let mut action = Action {
        name: "Bounded script steps".into(),
        description: String::new(),
        builtin: false,
        steps: vec![Step::RunJavaScript(" ".repeat(256 * 1024 + 1)), Step::SetTitle("must not run".into())],
    };
    let mut visited = Vec::new();
    assert!(run_on(&action, "script-budget.pdf", Arc::new(crate::tests::fixture(1)), |step, _| visited.push(step)).is_err());
    assert_eq!(visited, [0]);
    action.steps[0] = Step::RunJavaScript("event.value = ;".into());
    let result = run_on(&action, "script-budget.pdf", Arc::new(crate::tests::fixture(1)), |_, _| {}).unwrap();
    assert!(result.log[0].contains("SyntaxError"), "{:?}", result.log);
    assert_eq!(result.log.len(), 2, "ordinary exceptions are reported without stopping subsequent steps");
}

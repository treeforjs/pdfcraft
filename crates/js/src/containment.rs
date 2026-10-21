//! Real-kernel regressions: every refusal is followed by a valid run in a fresh store.
use super::*;

fn eval(script: &str) -> Outcome {
    run(script, &Event::field("Calculate", "total", "unchanged"), &DocInfo::default(), &[], &[], Limits { loop_iterations: u64::MAX, recursion: 256 })
}

#[test]
fn builtin_allocation_regex_and_instruction_work_are_contained() {
    for script in [
        "event.value='partial'; 'x'.repeat(1000000000)",
        "event.value='partial'; new Array(1000000000).fill('large')",
        "event.value='partial'; while(true) {}",
        "event.value='partial'; /^(a+)+$/.test('a'.repeat(40)+'!')",
        "event.value='partial'; function f(){ return f(); } f()",
    ] {
        let out = eval(script);
        assert!(out.error.is_some(), "{script}: {out:?}");
        assert_eq!(out.value, "unchanged", "trapped effects must not escape: {out:?}");
        assert!(out.changed.is_empty() && out.requests.is_empty() && out.annots.is_empty());
        let next = eval("event.value=6*7");
        assert_eq!(next.error, None, "{next:?}");
        assert_eq!(next.value, "42");
    }
}

#[test]
fn messages_and_encoded_input_output_have_hard_budgets() {
    let oversized = "x".repeat(4 * 1024 * 1024 + 1);
    let doc = DocInfo { file_name: oversized, ..Default::default() };
    let out = run("event.value='ran'", &Event::doc("Open"), &doc, &[], &[], Limits::default());
    assert!(out.error.as_deref().is_some_and(|e| e.contains("budget")), "{out:?}");
    for script in [
        "for(var i=0;i<10001;i++)app.beep();",
        "for(var i=0;i<101;i++)app.alert('hello');",
        "for(var i=0;i<1001;i++)console.println('hello');",
        "event.value='x'.repeat(5000000)",
    ] {
        let out = eval(script);
        assert!(out.error.is_some(), "{script}: {out:?}");
        assert!(out.requests.is_empty() && out.console.is_empty() && out.alerts.is_empty());
        assert_eq!(eval("event.value='ready'").value, "ready");
    }
}

#[test]
fn xfa_and_formcalc_traps_stop_without_background_workers() {
    let root = xfa::XfaNode { name: "form1".into(), som: "form1[0]".into(), kind: Some(xfa::XfaKind::Subform), ..Default::default() };
    let event = xfa::XfaEvent { target: "form1[0]".into(), ..Default::default() };
    let doc = xfa::XfaDoc::default();
    let limits = Limits { loop_iterations: u64::MAX, recursion: 256 };
    let out = xfa::run_xfa("xfa.host.messageBox('partial'); new Array(1000000000).fill(1)", &event, &doc, &root, limits);
    assert!(out.error.is_some() && out.effects.is_empty() && !out.abandoned, "{out:?}");
    let out = formcalc::run_formcalc_within("while (1) do endwhile", &event, &doc, root.clone(), limits, std::time::Duration::from_millis(10));
    assert!(out.error.is_some() && out.effects.is_empty() && !out.abandoned, "{out:?}");
    let out = xfa::run_xfa("1+1", &event, &doc, &root, limits);
    assert_eq!(out.error, None, "{out:?}");
    assert_eq!(out.result.as_deref(), Some("2"));
    let out = formcalc::run_formcalc("1+1", &event, &doc, &root, limits);
    assert_eq!(out.error, None, "{out:?}");
    assert_eq!(out.result.as_deref(), Some("2"));
}

#[test]
fn protocol_preserves_platform_and_rejects_deep_snapshots() {
    assert_eq!(
        eval("event.value=app.platform").value,
        if cfg!(target_os = "windows") {
            "WIN"
        } else if cfg!(target_os = "macos") {
            "MAC"
        } else {
            "UNIX"
        }
    );
    let mut root = xfa::XfaNode::default();
    for _ in 0..65 {
        root = xfa::XfaNode { children: vec![root], ..Default::default() };
    }
    let out = xfa::run_xfa("1", &xfa::XfaEvent::default(), &xfa::XfaDoc::default(), &root, Limits::default());
    assert!(out.error.as_deref().is_some_and(|e| e.contains("nested too deeply")), "{out:?}");
    assert_eq!(eval("event.value='ok'").value, "ok");
}

#[test]
fn syntax_failures_are_distinct_from_resource_refusals() {
    assert!(!eval("event.value = ;").limited);
    let out = run(
        "getField('x').value='partial'; while(true){}",
        &Event::doc("Open"),
        &DocInfo::default(),
        &[FieldState::new("x", FieldType::Text, vec!["keep".into()])],
        &[],
        Limits { loop_iterations: 100, recursion: 32 },
    );
    assert!(out.limited && out.error.is_some() && out.changed.is_empty(), "{out:?}");
    assert_eq!(eval("event.value='ready'").value, "ready");
}

use super::*;

fn fields() -> Vec<FieldState> {
    let mut agree = FieldState::new("agree", FieldType::CheckBox, vec![]);
    agree.options = vec![("Yes".into(), "Yes".into())];
    let mut size = FieldState::new("size", FieldType::ComboBox, vec!["m".into()]);
    size.options = vec![("s".into(), "Small".into()), ("m".into(), "Medium".into()), ("l".into(), "Large".into())];
    vec![
        FieldState::new("price", FieldType::Text, vec!["12.5".into()]),
        FieldState::new("qty", FieldType::Text, vec!["4".into()]),
        FieldState::new("total", FieldType::Text, vec![]),
        FieldState::new("zip", FieldType::Text, vec!["02134".into()]),
        agree,
        size,
    ]
}

fn doc() -> DocInfo {
    DocInfo { file_name: "order.pdf".into(), num_pages: 3, page: 0, info: vec![("title".into(), "Order".into())], ..Default::default() }
}

fn go(script: &str, event: &Event) -> Outcome {
    run(script, event, &doc(), &fields(), &[], Limits::default())
}

#[test]
fn calculate_scripts_read_fields_and_set_the_value() {
    let o = go("event.value = this.getField('price').value * getField('qty').value;", &Event::field("Calculate", "total", ""));
    assert_eq!(o.error, None);
    assert_eq!(o.value, "50");
    assert!(o.rc);
}

#[test]
fn numbers_with_leading_zeros_stay_text() {
    let o = go("event.value = typeof getField('zip').value + ':' + typeof getField('qty').value;", &Event::field("Calculate", "total", ""));
    assert_eq!(o.value, "string:number");
}

#[test]
fn validate_scripts_reject_with_rc_and_alert() {
    let o = go("if (event.value > 10) { app.alert('Too many'); event.rc = false; }", &Event::field("Validate", "qty", "11"));
    assert!(!o.rc);
    assert_eq!(o.alerts, ["Too many"]);
    let o = go("if (event.value > 10) { event.rc = false; }", &Event::field("Validate", "qty", "3"));
    assert!(o.rc);
}

#[test]
fn keystroke_scripts_see_the_change() {
    let mut e = Event::field("Keystroke", "qty", "1");
    e.change = "x".into();
    e.will_commit = false;
    let o = go("event.rc = /^[0-9]*$/.test(event.change);", &e);
    assert!(!o.rc);
}

#[test]
fn scripts_change_other_fields_and_their_properties() {
    let o = go(
        "var t = getField('total'); t.value = 99; t.readonly = true; t.display = display.hidden; \
         getField('agree').checkThisBox(0, true); getField('size').value = 'l'; t.textColor = color.red;",
        &Event::field("Mouse Up", "agree", ""),
    );
    assert_eq!(o.error, None);
    let total = o.changed.iter().find(|f| f.name == "total").unwrap();
    assert_eq!(total.value, ["99"]);
    assert!(total.readonly);
    assert_eq!(total.display, DISPLAY_HIDDEN);
    assert_eq!(total.text_color.as_deref(), Some(&["RGB".to_string(), "1".into(), "0".into(), "0".into()][..]));
    assert_eq!(o.changed.iter().find(|f| f.name == "agree").unwrap().value, ["Yes"]);
    assert_eq!(o.changed.iter().find(|f| f.name == "size").unwrap().value, ["l"]);
}

#[test]
fn choice_and_check_box_object_model() {
    let o = go(
        "var s = getField('size'); event.value = [s.numItems, s.getItemAt(2, false), s.currentValueIndices, s.valueAsString, \
         getField('agree').value, getField('agree').isBoxChecked(0), s.type].join('|');",
        &Event::field("Calculate", "total", ""),
    );
    assert_eq!(o.value, "3|Large|1|m|Off|false|combobox");
}

#[test]
fn util_printf_printd_and_printx() {
    let o = go(
        "event.value = [util.printf('%,0.2f', 1234567.891), util.printf('%05d|%s|%x', 42, 'hi', 255), util.printf('%,2.2f', 1234.5), \
         util.printd('mmm d, yyyy HH:MM', new Date(2024, 0, 5, 9, 7)), util.printd('dddd', new Date(2024, 0, 5)), \
         util.printx('(999) 999-9999', '5551234567'), util.printx('>AAA', 'abc')].join('|');",
        &Event::field("Calculate", "total", ""),
    );
    assert_eq!(o.error, None);
    assert_eq!(o.value, "1,234,567.89|00042|hi|FF|1.234,50|Jan 5, 2024 09:07|Friday|(555) 123-4567|ABC");
}

#[test]
fn document_requests_and_console() {
    let o = go(
        "console.println('hello ' + this.documentFileName + ' ' + numPages + ' ' + info.title); this.pageNum = 2; \
         this.resetForm(['qty']); this.print(); app.launchURL('https://example.org'); this.submitForm({cURL: 'https://example.org/f'});",
        &Event::field("Mouse Up", "agree", ""),
    );
    assert_eq!(o.error, None);
    assert_eq!(o.console, ["hello order.pdf 3 Order"]);
    assert_eq!(
        o.requests,
        [
            Request::GoToPage(2),
            Request::Reset(vec!["qty".into()]),
            Request::Print,
            Request::LaunchUrl("https://example.org".into()),
            Request::Submit("https://example.org/f".into())
        ]
    );
}

#[test]
fn document_level_functions_are_available() {
    let o = run(
        "event.value = double(getField('qty').value);",
        &Event::field("Calculate", "total", ""),
        &doc(),
        &fields(),
        &["function double(x) { return x * 2; }".into()],
        Limits::default(),
    );
    assert_eq!(o.value, "8");
}

#[test]
fn errors_and_runaway_scripts_are_contained() {
    let o = go("event.value = nosuch.thing;", &Event::field("Calculate", "total", "keep"));
    assert!(o.error.is_some());
    let o = run("while (true) {}", &Event::doc("Open"), &doc(), &fields(), &[], Limits { loop_iterations: 10_000, recursion: 64 });
    assert!(o.error.is_some(), "the loop limit stops it");
    let o = go("function f() { return f(); } f();", &Event::doc("Open"));
    assert!(o.error.is_some(), "the recursion limit stops it");
    let o = go("getField('nope').value", &Event::doc("Open"));
    assert!(o.error.as_deref().unwrap_or("").contains("null") || o.error.is_some());
}

#[test]
fn the_sandbox_has_no_host_access() {
    let o = go(
        "event.value = [typeof require, typeof process, typeof fetch, typeof XMLHttpRequest, typeof importScripts].join(',');",
        &Event::doc("Open"),
    );
    assert_eq!(o.value, "undefined,undefined,undefined,undefined,undefined");
}

/// Hostile nesting used to overflow the stack in boa's parser and compiler, aborting the app
/// (about 100 nested parentheses were enough on a 2 MiB stack).
#[test]
fn deeply_nested_scripts_fail_instead_of_crashing() {
    let n = 100_000;
    let hostile = [
        format!("event.value = {}1{};", "(".repeat(n), ")".repeat(n)),
        format!("var a = {}1{};", "[".repeat(n), "]".repeat(n)),
        format!("{}{}", "{".repeat(n), "}".repeat(n)),
        format!("var a = {}1;", "!".repeat(n)),
        format!("var a = {}1;", "- ".repeat(n)),
        format!("var a = {}1;", "1?1:".repeat(n)),
        format!("var f = {}1;", "a=>".repeat(n)),
    ];
    for s in &hostile {
        let o = go(s, &Event::doc("Open"));
        assert!(o.error.is_some(), "{}…", &s[..20]);
    }
    // A hostile document-level script is refused too, before the field script runs.
    let o = run("event.value = 'ran';", &Event::field("Calculate", "a", ""), &doc(), &fields(), &[hostile[0].clone()], Limits::default());
    assert!(o.error.is_some());
}

#[test]
fn ordinary_nesting_still_runs() {
    let n = 40;
    let s = format!("event.value = {}1{} + [[[2]]][0][0][0] + (1 ? 2 : 3);", "(".repeat(n), ")".repeat(n));
    let o = go(&s, &Event::field("Calculate", "a", ""));
    assert_eq!(o.error, None);
    assert_eq!(o.value, "5");
    // Brackets in strings and comments don't count.
    let s = format!("// {}\nevent.value = '{}'.length;", "(".repeat(1000), "[".repeat(1000));
    assert_eq!(go(&s, &Event::field("Calculate", "a", "")).value, "1000");
}

fn assert_xfa_resource_refusal(out: &xfa::XfaOutcome) {
    assert!(out.limited && out.error.is_some(), "{out:?}");
    assert!(out.effects.is_empty() && out.console.is_empty() && out.notes.is_empty(), "{out:?}");
    assert!(out.result.is_none() && out.result_bool.is_none() && !out.abandoned, "{out:?}");
}

fn assert_added_instances(out: &xfa::XfaOutcome, added: usize) {
    assert_eq!(out.error, None, "{out:?}");
    assert!(!out.limited && !out.abandoned, "{out:?}");
    let count = (added + 2).to_string();
    assert_eq!(out.result.as_deref(), Some(count.as_str()), "{out:?}");
    let effects: Vec<_> =
        (2..added + 2).map(|index| xfa::XfaEffect::AddInstance { som: format!("form1[0].page1[0].table[0].row[{index}]") }).collect();
    assert_eq!(out.effects, effects);
    assert!(out.console.is_empty(), "{out:?}");
}

mod xfa_model {
    use crate::Limits;
    use crate::xfa::*;

    pub(super) fn field(name: &str, som: &str, value: &str, numeric: bool) -> XfaNode {
        XfaNode {
            name: name.into(),
            som: som.into(),
            kind: Some(XfaKind::Field),
            value: value.into(),
            numeric,
            presence: "visible".into(),
            ..Default::default()
        }
    }

    /// form1 > page1 > { qty, price, total, details (subform, hidden) > note, table > row[0..2] > { what, amount }, go (button) }
    pub(super) fn form() -> XfaNode {
        let row = |i: usize| XfaNode {
            name: "row".into(),
            som: format!("form1[0].page1[0].table[0].row[{i}]"),
            kind: Some(XfaKind::Subform),
            repeatable: true,
            occur_min: 1,
            occur_max: None,
            index: i,
            children: vec![
                field("what", &format!("form1[0].page1[0].table[0].row[{i}].what[0]"), if i == 0 { "Rent" } else { "" }, false),
                field("amount", &format!("form1[0].page1[0].table[0].row[{i}].amount[0]"), if i == 0 { "10" } else { "5" }, true),
            ],
            ..Default::default()
        };
        XfaNode {
            name: "form1".into(),
            som: "form1[0]".into(),
            kind: Some(XfaKind::Subform),
            children: vec![XfaNode {
                name: "page1".into(),
                som: "form1[0].page1[0]".into(),
                kind: Some(XfaKind::Subform),
                children: vec![
                    field("qty", "form1[0].page1[0].qty[0]", "4", true),
                    field("price", "form1[0].page1[0].price[0]", "2.5", true),
                    field("total", "form1[0].page1[0].total[0]", "", true),
                    XfaNode {
                        name: "details".into(),
                        som: "form1[0].page1[0].details[0]".into(),
                        kind: Some(XfaKind::Subform),
                        presence: "hidden".into(),
                        children: vec![field("note", "form1[0].page1[0].details[0].note[0]", "", false)],
                        ..Default::default()
                    },
                    XfaNode {
                        name: "table".into(),
                        som: "form1[0].page1[0].table[0]".into(),
                        kind: Some(XfaKind::Subform),
                        children: vec![row(0), row(1)],
                        ..Default::default()
                    },
                    field("go", "form1[0].page1[0].go[0]", "", false),
                ],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn go(script: &str, target: &str, activity: &str) -> XfaOutcome {
        let ev = XfaEvent { activity: activity.into(), target: target.into(), ..Default::default() };
        let doc = XfaDoc { file_name: "f.pdf".into(), page: 1, page_count: 3 };
        run_xfa(script, &ev, &doc, &form(), Limits::default())
    }

    #[test]
    fn calculate_scripts_see_siblings_by_name_and_yield_their_last_expression() {
        let o = go("qty.rawValue * price.rawValue", "form1[0].page1[0].total[0]", "calculate");
        assert_eq!(o.error, None, "{o:?}");
        assert_eq!(o.result.as_deref(), Some("10"));
        let o = go("this.rawValue = xfa.form.form1.page1.qty.rawValue + 1;", "form1[0].page1[0].total[0]", "calculate");
        assert_eq!(o.effects, vec![XfaEffect::SetValue { som: "form1[0].page1[0].total[0]".into(), value: "5".into() }]);
    }

    #[test]
    fn long_values_and_results_are_cut_where_they_are_stored() {
        // The value kept for later reads is the one the effect saves.
        let o = go("details.note.rawValue = 'x'.repeat(100000); details.note.rawValue.length", "form1[0].page1[0].total[0]", "calculate");
        assert_eq!(o.error, None, "{o:?}");
        assert!(!o.limited, "{o:?}");
        assert_eq!(o.result.as_deref(), Some("65536"));
        assert_eq!(o.effects, vec![XfaEffect::SetValue { som: "form1[0].page1[0].details[0].note[0]".into(), value: "x".repeat(65_536) }]);
        assert!(o.notes.iter().any(|n| n.contains("cut")), "{:?}", o.notes);
        // A calculate's result is cut like a value set, and noted.
        let o = go("'x'.repeat(100000)", "form1[0].page1[0].details[0].note[0]", "calculate");
        assert_eq!(o.error, None, "{o:?}");
        assert!(!o.limited, "{o:?}");
        assert_eq!(o.result, Some("x".repeat(65_536)));
        assert!(o.effects.is_empty());
        assert!(o.notes.iter().any(|n| n.contains("result")), "{:?}", o.notes);
    }

    #[test]
    fn long_array_join_workloads_complete_exactly_or_refuse_atomically() {
        let clipped = "x".repeat(65_536);
        let cases = [
            (
                "details.note.rawValue = new Array(100001).join('x'); details.note.rawValue.length",
                "form1[0].page1[0].total[0]",
                "65536".to_string(),
                vec![XfaEffect::SetValue { som: "form1[0].page1[0].details[0].note[0]".into(), value: clipped.clone() }],
            ),
            ("new Array(100001).join('x')", "form1[0].page1[0].details[0].note[0]", clipped, Vec::new()),
            (
                "xfa.host.messageBox(new Array(100000).join('x'));",
                "form1[0].page1[0]",
                "1".to_string(),
                vec![XfaEffect::MessageBox(format!("{}…", "x".repeat(4_096)))],
            ),
        ];
        for (script, target, result, effects) in cases {
            let out = go(script, target, "calculate");
            if out.limited {
                super::assert_xfa_resource_refusal(&out);
            } else {
                assert_eq!(out.error, None, "{out:?}");
                assert!(!out.abandoned, "{out:?}");
                assert_eq!(out.result.as_deref(), Some(result.as_str()), "{script}");
                assert_eq!(out.effects, effects, "{script}");
                assert!(out.console.is_empty(), "{out:?}");
            }
            let next = go("1+1", target, "calculate");
            assert_eq!(next.error, None, "{next:?}");
            assert!(!next.limited);
            assert_eq!(next.result.as_deref(), Some("2"));
        }
    }

    #[test]
    fn values_read_as_numbers_or_strings_and_null_when_empty() {
        let o = go(
            "typeof qty.rawValue + ':' + typeof this.rawValue + ':' + (total.rawValue === null) + ':' + typeof table.row.what.rawValue",
            "form1[0].page1[0].price[0]",
            "click",
        );
        assert_eq!(o.result.as_deref(), Some("number:number:true:string"));
    }

    #[test]
    fn navigation_parent_resolve_node_and_nodes() {
        let o = go(
            "this.parent.name + '/' + this.parent.parent.className + '/' + this.resolveNode('qty').rawValue + '/' + xfa.resolveNode('$form.form1.page1.table.row[1].amount').rawValue + '/' + this.parent.nodes.length + '/' + xfa.resolveNodes('form1.page1.table.row[*]').length + '/' + xfa.resolveNode('$form..amount').somExpression",
            "form1[0].page1[0].price[0]",
            "click",
        );
        assert_eq!(o.error, None, "{o:?}");
        assert_eq!(o.result.as_deref(), Some("page1/subform/4/5/6/2/form1[0].page1[0].table[0].row[0].amount[0]"));
    }

    #[test]
    fn presence_and_access_changes_are_effects() {
        let o = go("details.presence = 'visible'; details.note.access = 'readOnly'; this.presence = 'nonsense';", "form1[0].page1[0].go[0]", "click");
        assert_eq!(o.error, None, "{o:?}");
        assert_eq!(
            o.effects,
            vec![
                XfaEffect::SetPresence { som: "form1[0].page1[0].details[0]".into(), presence: "visible".into() },
                XfaEffect::SetAccess { som: "form1[0].page1[0].details[0].note[0]".into(), access: "readOnly".into() },
            ]
        );
    }

    #[test]
    fn instance_managers_add_remove_and_count() {
        let o = go(
            "var n = table._row.count; var r = table._row.addInstance(1); r.what.rawValue = 'New'; n + '/' + table._row.count + '/' + r.somExpression + '/' + r.index",
            "form1[0].page1[0].go[0]",
            "click",
        );
        assert_eq!(o.error, None, "{o:?}");
        assert_eq!(o.result.as_deref(), Some("2/3/form1[0].page1[0].table[0].row[2]/2"));
        assert_eq!(
            o.effects,
            vec![
                XfaEffect::AddInstance { som: "form1[0].page1[0].table[0].row[2]".into() },
                XfaEffect::SetValue { som: "form1[0].page1[0].table[0].row[2].what[0]".into(), value: "New".into() },
            ]
        );
        // Through the instance manager property, from inside a row; removing renumbers.
        let o = go(
            "this.parent.instanceManager.removeInstance(0); xfa.resolveNode('table.row[0]').amount.rawValue",
            "form1[0].page1[0].table[0].row[1].amount[0]",
            "click",
        );
        assert_eq!(o.error, None, "{o:?}");
        assert_eq!(o.result.as_deref(), Some("5"), "the second row is now the first");
        assert_eq!(o.effects, vec![XfaEffect::RemoveInstance { som: "form1[0].page1[0].table[0].row[0]".into() }]);
        // setInstances, and the floor of one instance.
        let o = go(
            "table._row.setInstances(4); var a = table._row.count; table._row.setInstances(0); a + '/' + table._row.count",
            "form1[0].page1[0].go[0]",
            "click",
        );
        assert_eq!(o.result.as_deref(), Some("4/1"));
    }

    #[test]
    fn host_layout_event_and_app_calls_become_effects() {
        let o = go(
            "xfa.host.messageBox('Hi', 'T', 3, 1); xfa.host.resetData('qty, price'); xfa.host.print(1, '0', '2', 0, 0, 0, 0, 0); app.execMenuItem('SaveAs'); app.launchURL('https://x.test'); xfa.host.setFocus('qty'); xfa.form.recalculate(1); console.println('p' + xfa.layout.page(this) + '/' + xfa.layout.pageCount() + '/' + xfa.host.currentPage + '/' + xfa.event.name); this.border.fill.color.value = '255,0,0'; this.fillColor = 'x'; util.printf('%d', 7)",
            "form1[0].page1[0].go[0]",
            "click",
        );
        assert_eq!(o.error, None, "{o:?}");
        assert_eq!(
            o.effects,
            vec![
                XfaEffect::MessageBox("Hi".into()),
                XfaEffect::ResetData(vec!["qty".into(), "price".into()]),
                XfaEffect::Print,
                XfaEffect::SaveAs,
                XfaEffect::LaunchUrl("https://x.test".into()),
                XfaEffect::SetFocus("form1[0].page1[0].qty[0]".into()),
                XfaEffect::Recalculate,
            ]
        );
        assert_eq!(o.console, vec!["p2/3/1/click".to_string()]);
        assert_eq!(o.result.as_deref(), Some("7"));
    }

    #[test]
    fn field_names_never_shadow_javascript_builtins() {
        let mut f = form();
        f.children[0].children.push(field("Date", "form1[0].page1[0].Date[0]", "x", false));
        f.children[0].children.push(field("eval", "form1[0].page1[0].eval[0]", "y", false));
        let ev = XfaEvent { activity: "click".into(), target: "form1[0].page1[0].go[0]".into(), ..Default::default() };
        let o = run_xfa(
            "typeof new Date().getTime() + ':' + this.parent.Date.rawValue + ':' + this.parent.eval.rawValue",
            &ev,
            &XfaDoc::default(),
            &f,
            Limits::default(),
        );
        assert_eq!(o.error, None, "{o:?}");
        assert_eq!(o.result.as_deref(), Some("number:x:y"));
    }

    #[test]
    fn validate_scripts_give_a_verdict_and_errors_are_reported() {
        let o = go("this.rawValue === null || this.rawValue <= 3", "form1[0].page1[0].qty[0]", "validate");
        assert_eq!(o.result_bool, Some(false));
        let o = go("nosuch.rawValue = 1", "form1[0].page1[0].qty[0]", "click");
        assert!(o.error.is_some() && o.effects.is_empty());
        let o = go("while (true) {}", "form1[0].page1[0].qty[0]", "click");
        assert!(o.error.is_some(), "the loop limit stops it");
        let o = go(&"(".repeat(200), "form1[0].page1[0].qty[0]", "click");
        assert!(o.error.is_some());
        // Ordinary instance creation must complete and return every exact effect.
        let o = go("for (var i = 0; i < 500; i++) table._row.addInstance(1); table._row.count", "form1[0].page1[0].go[0]", "click");
        super::assert_added_instances(&o, 500);
        // The original stress workload either reaches the instance cap exactly or
        // exhausts the isolated resource budget without any partial effects.
        let o = go("for (var i = 0; i < 5000; i++) table._row.addInstance(1); table._row.count", "form1[0].page1[0].go[0]", "click");
        if o.limited {
            super::assert_xfa_resource_refusal(&o);
        } else {
            super::assert_added_instances(&o, 1000);
        }
        let next = go("1+1", "form1[0].page1[0].go[0]", "click");
        assert_eq!(next.error, None, "{next:?}");
        assert!(!next.limited);
        assert_eq!(next.result.as_deref(), Some("2"));
    }

    #[test]
    fn values_set_again_merge_and_hostile_loops_are_capped() {
        // A calculate that sets its own value in a loop leaves one effect: the last value.
        let o = go("for (var i = 0; i < 500; i++) this.rawValue = i;", "form1[0].page1[0].total[0]", "calculate");
        assert_eq!(o.effects, vec![XfaEffect::SetValue { som: "form1[0].page1[0].total[0]".into(), value: "499".into() }], "{:?}", o.error);
        // The original stress workload may exceed a slow machine's deadline. It must
        // either produce the exact final value or refuse atomically, without a worker left.
        let stress = go("for (var i = 0; i < 50000; i++) this.rawValue = i;", "form1[0].page1[0].total[0]", "calculate");
        if stress.limited {
            assert!(stress.error.is_some() && stress.effects.is_empty() && !stress.abandoned, "{stress:?}");
        } else {
            assert_eq!(stress.error, None, "{stress:?}");
            assert_eq!(stress.effects, vec![XfaEffect::SetValue { som: "form1[0].page1[0].total[0]".into(), value: "49999".into() }]);
        }
        // Order-dependent effects in between keep the values on either side.
        let o = go("qty.rawValue = 1; xfa.host.resetData(); qty.rawValue = 2; qty.rawValue = 3;", "form1[0].page1[0]", "click");
        assert_eq!(o.effects.len(), 3, "{:?}", o.effects);
        assert_eq!(o.effects.last(), Some(&XfaEffect::SetValue { som: "form1[0].page1[0].qty[0]".into(), value: "3".into() }));
        // Message boxes and console lines in a loop stop at their caps, and say so.
        let o = go("for (var i = 0; i < 1500; i++) { xfa.host.messageBox('m' + i); console.println('c' + i); }", "form1[0].page1[0]", "initialize");
        let alerts = o.effects.iter().filter(|e| matches!(e, XfaEffect::MessageBox(_))).count();
        assert_eq!(alerts, MAX_ALERTS);
        assert_eq!(o.console.len(), MAX_CONSOLE);
        assert!(o.notes.iter().any(|n| n.contains("messages")) && o.notes.iter().any(|n| n.contains("console")), "{:?}", o.notes);
        let stress =
            go("for (var i = 0; i < 100000; i++) { xfa.host.messageBox('m' + i); console.println('c' + i); }", "form1[0].page1[0]", "initialize");
        if stress.limited {
            assert!(stress.error.is_some() && stress.effects.is_empty() && stress.console.is_empty(), "{stress:?}");
        } else {
            assert_eq!(stress.console.len(), MAX_CONSOLE);
            assert!(stress.effects.len() <= MAX_EFFECTS);
        }
        // Distinct effects stop at the effect cap.
        let o = go("for (var i = 0; i < 30000; i++) xfa.host.beep();", "form1[0].page1[0]", "click");
        if o.limited {
            assert!(o.error.is_some() && o.effects.is_empty(), "{o:?}");
        } else {
            assert_eq!(o.effects.len(), MAX_EFFECTS);
            assert!(o.notes.iter().any(|n| n.contains("changes")), "{:?}", o.notes);
        }
        // Long messages are cut.
        let o = go("xfa.host.messageBox('x'.repeat(99999));", "form1[0].page1[0]", "click");
        assert_eq!(o.error, None, "{o:?}");
        assert!(!o.limited, "{o:?}");
        assert_eq!(o.effects, vec![XfaEffect::MessageBox(format!("{}…", "x".repeat(4_096)))]);
    }
}

mod formcalc {
    use super::xfa_model::form;
    use crate::Limits;
    use crate::formcalc::run_formcalc;
    use crate::xfa::*;

    fn go(script: &str, target: &str, activity: &str) -> XfaOutcome {
        let ev = XfaEvent { activity: activity.into(), target: target.into(), ..Default::default() };
        let doc = XfaDoc { file_name: "f.pdf".into(), page: 1, page_count: 3 };
        run_formcalc(script, &ev, &doc, &form(), Limits::default())
    }

    /// The value of a script run as a calculate on `total`.
    fn calc(script: &str) -> String {
        let o = go(script, "form1[0].page1[0].total[0]", "calculate");
        assert_eq!(o.error, None, "{script}");
        o.result.unwrap_or_default()
    }

    #[test]
    fn dates_and_numeric_text_round_trip_through_the_contained_kernel() {
        assert_eq!(calc(r#"Date2Num("1970-01-01")"#), "25568");
        assert_eq!(calc(r#"Date2Num("2001-02-03","YYYY-MM-DD")"#), "36924");
        assert_eq!(calc(r#"Num2Date(Date2Num("Feb 3, 2001","MMM D, YYYY"),"MMMM D, YYYY")"#), "February 3, 2001");
        assert_eq!(calc(r#"Num2Date(Date2Num("2/3/01","M/D/YY"),"YYYY-MM-DD")"#), "2001-02-03");
        assert_eq!(calc("WordNum(1234.5,2)"), "One Thousand Two Hundred Thirty-Four Dollars And 50 Cents");
        assert_eq!(calc("3.0"), "3");
        assert_eq!(calc("2.5"), "2.5");
        assert_eq!(calc("0.1+0.2"), "0.3");
        assert_eq!(calc(r#"1 + "12abc""#), "13");
    }

    #[test]
    fn arithmetic_strings_and_comparisons_follow_formcalc_rules() {
        assert_eq!(calc("1 + 2 * 3"), "7");
        assert_eq!(calc("(1 + 2) * 3"), "9");
        assert_eq!(calc("10 / 4"), "2.5");
        assert_eq!(calc("-3 + +4"), "1");
        assert_eq!(calc(r#""12" + 3"#), "15");
        assert_eq!(calc(r#""abc" + 3"#), "3");
        assert_eq!(calc(r#"Concat("a", "b", 1.50)"#), "ab1.5");
        assert_eq!(calc(r##""say ""hi""""##), "say \"hi\"");
        assert_eq!(calc("1 < 2 and 2 <= 2"), "1");
        assert_eq!(calc("1 == 1.0 & 3 <> 4"), "1");
        assert_eq!(calc(r#""a" lt "b" | 0"#), "1");
        assert_eq!(calc("not 0"), "1");
        assert_eq!(calc("!0 + !1"), "1");
        assert_eq!(calc("table.row[0]..amount"), "10");
        assert_eq!(calc(r#""abc" == "ABC""#), "0");
        // Logical operands are numbers: text that is not a number is false.
        assert_eq!(calc(r#"if ("abc") then 1 else 0 endif"#), "0");
        assert_eq!(calc(r#"if ("2") then 1 else 0 endif"#), "1");
        assert_eq!(calc("null + 1"), "1");
        assert_eq!(calc("null == null"), "1");
        assert_eq!(calc("1 == null"), "0");
        assert_eq!(calc("1 <> null"), "1");
        assert!(go("1 / 0", "form1[0].page1[0].total[0]", "calculate").error.is_some());
        // Comments and newlines inside brackets.
        assert_eq!(calc("; a comment\n// another\nSum(1,\n 2,\n 3)"), "6");
    }

    #[test]
    fn built_in_functions_cover_the_common_set() {
        assert_eq!(calc("Abs(-2.5)"), "2.5");
        assert_eq!(calc("Ceil(2.1)"), "3");
        assert_eq!(calc("Floor(-2.1)"), "-3");
        assert_eq!(calc("Mod(7, 3)"), "1");
        assert_eq!(calc("Round(2.345, 2)"), "2.35");
        assert_eq!(calc("Sum(1, 2, 3)"), "6");
        assert_eq!(calc("Avg(1, 2, 3, null)"), "2");
        assert_eq!(calc("Count(1, null, 3)"), "2");
        assert_eq!(calc("Max(1, 9, 3)"), "9");
        assert_eq!(calc("Min(4, 2, 3)"), "2");
        assert_eq!(calc(r#"Choose(2, "a", "b", "c")"#), "b");
        assert_eq!(calc(r#"Oneof(3, 1, 2, 3)"#), "1");
        assert_eq!(calc(r#"Within(5, 1, 10)"#), "1");
        assert_eq!(calc(r#"HasValue("")"#), "0");
        assert_eq!(calc(r#"HasValue("x")"#), "1");
        assert_eq!(calc(r#"At("abcd", "c")"#), "3");
        assert_eq!(calc(r#"Left("hello", 2)"#), "he");
        assert_eq!(calc(r#"Right("hello", 2)"#), "lo");
        assert_eq!(calc(r#"Len("héllo")"#), "5");
        assert_eq!(calc(r#"Lower("ABC")"#), "abc");
        assert_eq!(calc(r#"Upper("abc")"#), "ABC");
        assert_eq!(calc(r#"Ltrim("  a ")"#), "a ");
        assert_eq!(calc(r#"Rtrim("  a ")"#), "  a");
        assert_eq!(calc(r#"Space(3)"#), "   ");
        assert_eq!(calc(r#"Str(3.14159, 6, 2)"#), "  3.14");
        assert_eq!(calc(r#"Substr("abcdef", 2, 3)"#), "bcd");
        assert_eq!(calc(r#"Stuff("abcdef", 2, 3, "XY")"#), "aXYef");
        assert_eq!(calc(r#"Replace("a-b-c", "-", "+")"#), "a+b+c");
        assert_eq!(calc(r#"WordNum(42)"#), "Forty-Two");
        assert_eq!(calc(r#"WordNum(1.999, 2)"#), "Two Dollars And 00 Cents");
        assert_eq!(calc(r#"WordNum(1.01, 2)"#), "One Dollar And 01 Cent");
        assert_eq!(calc(r#"Format("zzz,zz9.99", 1234.5)"#), "1,234.50");
        assert_eq!(calc(r#"Format("$z,zz9.99", -5)"#), "-$5.00");
        assert_eq!(calc(r#"Format("MM/DD/YYYY", "2024-02-29")"#), "02/29/2024");
        assert_eq!(calc(r#"Parse("zzz,zz9.99", "1,234.50")"#), "1234.5");
        assert_eq!(calc(r#"Parse("MMM D, YYYY", "Mar 15, 1996")"#), "1996-03-15");
        assert_eq!(calc(r#"Date2Num("1900-01-01")"#), "1");
        assert_eq!(calc(r#"Date2Num("2024-02-29")"#), "45350");
        assert_eq!(calc(r#"Date2Num("29/02/2024", "DD/MM/YYYY")"#), "45350");
        assert_eq!(calc(r#"Num2Date(45350, "YYYY-MM-DD")"#), "2024-02-29");
        assert_eq!(calc(r#"Num2Date(Date2Num("2024-02-29") + 1, "MMMM D, YYYY")"#), "March 1, 2024");
        assert_eq!(calc(r#"Time2Num("01:00:00")"#), "3600000");
        assert_eq!(calc(r#"Num2Time(3661000)"#), "01:01:01");
        assert_eq!(calc("Round(Pmt(10000, 0.01, 12), 2)"), "888.49");
        assert_eq!(calc("Round(FV(100, 0.01, 12), 2)"), "1268.25");
        assert_eq!(calc("Round(PV(100, 0.01, 12), 2)"), "1125.51");
        assert_eq!(calc(r#"UnitValue("2in", "mm")"#), "50.8");
        assert_eq!(calc(r#"UnitType("2.5cm")"#), "cm");
        assert_eq!(calc(r#"Encode("a b&c", "url")"#), "a%20b%26c");
        assert_eq!(calc(r#"Decode("a%20b", "url")"#), "a b");
        assert!(go(r#"Get("http://example.com")"#, "form1[0].page1[0].total[0]", "calculate").error.unwrap().contains("network"));
        assert!(go("Nope(1)", "form1[0].page1[0].total[0]", "calculate").error.unwrap().contains("unknown function"));
    }

    #[test]
    fn control_flow_variables_and_functions_work() {
        assert_eq!(calc("var x = 3\nif (x > 2) then 10 elseif (x > 1) then 20 else 30 endif"), "10");
        // An assignment is an expression: its value is the script's result.
        assert_eq!(calc("qty = 7"), "7");
        assert_eq!(calc("var y = 8"), "8");
        assert_eq!(calc("var x = 1\nif (x > 2) then 10 elseif (x > 0) then 20 else 30 endif"), "20");
        assert_eq!(calc("var i = 0\nvar s = 0\nwhile (i < 5) do\n i = i + 1\n if (i == 3) then continue endif\n s = s + i\nendwhile\ns"), "12");
        assert_eq!(calc("var s = 0\nfor i = 1 upto 10 step 3 do s = s + i endfor\ns"), "22");
        assert_eq!(calc("var s = 0\nfor i = 5 downto 1 do\n if (i == 2) then break endif\n s = s + i\nendfor\ns"), "12");
        assert_eq!(
            calc(
                r#"var s = ""
foreach w in ("a", "b", "c") do s = Concat(s, w) endfor
s"#
            ),
            "abc"
        );
        assert_eq!(calc("func sq(n) do n * n endfunc\nsq(4) + sq(3)"), "25");
        assert_eq!(calc("func Sq(n) do n * n endfunc\nSQ(4)"), "16");
        // `exit` inside a function ends the whole script.
        let o = go("func f() do exit endfunc\nf()\n$host.messageBox(\"still ran\")\n5", "form1[0].page1[0].go[0]", "click");
        assert_eq!(o.error, None);
        assert!(o.effects.is_empty(), "{:?}", o.effects);
        // Rows can be walked as objects.
        assert_eq!(calc("var s = 0\nforeach r in (table.row[*]) do s = s + r.amount endfor\ns"), "15");
        // A long sum is not deep nesting.
        let long = format!("1{}", " + 1".repeat(500));
        let ev = XfaEvent { activity: "calculate".into(), target: "form1[0].page1[0].total[0]".into(), ..Default::default() };
        let o = run_formcalc(&long, &ev, &XfaDoc::default(), &form(), Limits { loop_iterations: 1000, recursion: 64 });
        assert_eq!(o.result.as_deref(), Some("501"), "{o:?}");
        // Null is an empty result with no truth value, as in JavaScript.
        let o = go("if (0) then 5 endif", "form1[0].page1[0].total[0]", "calculate");
        assert_eq!(o.result.as_deref(), Some(""));
        assert_eq!(o.result_bool, None);
        assert_eq!(calc("func f(n) do if (n < 2) then return 1 endif\n return n * f(n - 1) endfunc\nf(5)"), "120");
        assert_eq!(calc("IF (1) THEN 5 ENDIF"), "5");
        let o = go(r#"throw "stop here""#, "form1[0].page1[0].total[0]", "calculate");
        assert_eq!(o.error.as_deref(), Some("stop here"));
        let o = go("if (1) then 1", "form1[0].page1[0].total[0]", "calculate");
        assert!(o.error.unwrap().contains("endif"));
    }

    #[test]
    fn form_objects_resolve_assign_and_record_effects() {
        // Fields deref to their values; `$` is the current field.
        assert_eq!(calc("qty * price"), "10");
        assert_eq!(calc("$.name"), "total");
        assert_eq!(calc("$form.form1.page1.qty + 1"), "5");
        assert_eq!(calc("xfa.form.form1.page1.qty"), "4");
        assert_eq!(calc("$record.page1.qty"), "4");
        assert_eq!(calc("Sum(table.row[*].amount)"), "15");
        assert_eq!(calc("Count(table.row[*])"), "2");
        assert_eq!(calc("Count(table.row[*].what)"), "1");
        // Lists hold each object once, however they were reached.
        assert_eq!(calc("Count((table.row[*].parent..row).parent..row)"), "2");
        assert_eq!(calc("Count(xfa.resolveNodes(\"table.row[*].parent.row[*].parent.row[*]\"))"), "2");
        // Bare names in a row see every sibling row, and `[-1]`/`[+1]` are relative to the row
        // the script runs in.
        let in_row = |s: &str| {
            let o = go(s, "form1[0].page1[0].table[0].row[1].amount[0]", "calculate");
            assert_eq!(o.error, None, "{s}");
            o.result.unwrap_or_default()
        };
        assert_eq!(in_row("Count(row[*])"), "2");
        assert_eq!(in_row("Sum(row[*].amount)"), "15");
        assert_eq!(in_row("row[1].amount"), "5");
        assert_eq!(in_row("row[-1].amount"), "10");
        // Past the ends there is no object, so a property of it is an error (as in PDF).
        let past = go("row[+1].amount", "form1[0].page1[0].table[0].row[1].amount[0]", "calculate");
        assert!(past.error.unwrap().contains("no object"));
        assert_eq!(in_row("Exists(row[-5])"), "0");
        let o = go("row[+1].amount", "form1[0].page1[0].table[0].row[0].amount[0]", "calculate");
        assert_eq!(o.result.as_deref(), Some("5"));
        assert_eq!(calc("table.row[1].amount"), "5");
        assert_eq!(calc("table.row.amount"), "10");
        assert_eq!(calc("$form..amount[1]"), "5");
        assert_eq!(calc("table._row.count"), "2");
        assert_eq!(calc("Exists(qty) & not Exists(nothing)"), "1");
        assert_eq!(calc("HasValue(total)"), "0");
        assert_eq!(calc("$host.numPages"), "3");
        assert_eq!(calc("$layout.pageCount()"), "3");
        assert_eq!(calc("$event.name"), "calculate");
        assert_eq!(calc("xfa.resolveNode(\"page1.price\").rawValue"), "2.5");
        assert_eq!(calc("$.border.fill.color.value"), "");
        assert!(go("nothing + 1", "form1[0].page1[0].total[0]", "calculate").error.unwrap().contains("unknown name"));
        // Assignment to a field (by name and by path), a property and a variable.
        let o = go(
            r#"page1.qty = 6
xfa.form.form1.page1.qty = 7
details.presence = "visible"
price.access = "readOnly"
$.border.fill.color.value = "255,0,0"
$host.messageBox(Concat("Total ", qty * price))
$host.setFocus("qty")
table._row.addInstance(1)
table._row.count"#,
            "form1[0].page1[0].go[0]",
            "click",
        );
        assert_eq!(o.error, None, "{o:?}");
        assert_eq!(o.result.as_deref(), Some("3"));
        assert_eq!(
            o.effects,
            vec![
                XfaEffect::SetValue { som: "form1[0].page1[0].qty[0]".into(), value: "7".into() },
                XfaEffect::SetPresence { som: "form1[0].page1[0].details[0]".into(), presence: "visible".into() },
                XfaEffect::SetAccess { som: "form1[0].page1[0].price[0]".into(), access: "readOnly".into() },
                XfaEffect::MessageBox("Total 17.5".into()),
                XfaEffect::SetFocus("form1[0].page1[0].qty[0]".into()),
                XfaEffect::AddInstance { som: "form1[0].page1[0].table[0].row[2]".into() },
            ]
        );
        // Instance managers: remove, setInstances, count assignment, and the floor of one.
        let o = go(
            "table._row.removeInstance(0)\ntable._row.setInstances(1)\ntable._row.count = 0\ntable._row.count",
            "form1[0].page1[0].go[0]",
            "click",
        );
        assert_eq!(o.error, None, "{o:?}");
        assert_eq!(o.result.as_deref(), Some("1"));
        // Validate scripts give a truth value; a value the script sets wins over the result.
        let o = go("$ == null or $ <= 100", "form1[0].page1[0].qty[0]", "validate");
        assert_eq!(o.result_bool, Some(true));
        let o = go("$ > 100", "form1[0].page1[0].qty[0]", "validate");
        assert_eq!(o.result_bool, Some(false));
        // Reset, print and URLs are recorded as requests, not performed.
        let o = go(
            r#"$host.resetData("qty, price")
$host.print()
$host.gotoURL("https://example.org")"#,
            "form1[0].page1[0].go[0]",
            "click",
        );
        assert_eq!(
            o.effects,
            vec![XfaEffect::ResetData(vec!["qty".into(), "price".into()]), XfaEffect::Print, XfaEffect::LaunchUrl("https://example.org".into())]
        );
    }

    #[test]
    fn formcalc_values_results_and_errors_stay_bounded() {
        let total = "form1[0].page1[0].total[0]";
        let fast = Limits { loop_iterations: 10_000, recursion: 32 };
        let ev = XfaEvent { activity: "calculate".into(), target: total.into(), ..Default::default() };
        let doc = XfaDoc::default();
        let run = |s: &str| run_formcalc(s, &ev, &doc, &form(), fast);
        // A long value is cut where it is stored, so it reads back as saved.
        let o = run("details.note = Space(100000)\nLen(details.note)");
        assert_eq!(o.error, None);
        assert_eq!(o.result.as_deref(), Some("65536"));
        // A calculate's result is cut as values are, with a note.
        let o = run("Space(100000)");
        assert_eq!(o.result.map(|r| r.chars().count()), Some(65_536));
        assert!(o.notes.iter().any(|n| n.contains("result")), "{:?}", o.notes);
        // A thrown value is the error message: cut like one.
        let e = run("throw Space(100000)").error.unwrap();
        assert!(e.chars().count() <= 4_097, "{}", e.len());
        // Copying and comparing long strings costs steps, so it ends without a clock (wasm).
        let e = run("var s = Space(1000000)\nfor i = 1 upto 3000 do s endfor").error;
        assert!(e.is_some_and(|e| e.contains("too long")));
        let e = run("var s = Space(1000000)\nfor i = 1 upto 3000 do s == \"x\" endfor").error;
        assert!(e.is_some_and(|e| e.contains("too long")));
        // Each period IPmt and PPmt walk costs a step.
        let e = run("for i = 1 upto 600 do IPmt(1e9, 0.01, 1, 1, 10000) endfor").error;
        assert!(e.is_some_and(|e| e.contains("too long")));
        // Exists answers no for a name that isn't there, but a limit inside it still stops the script.
        assert_eq!(run("Exists(nothing.here)").result.as_deref(), Some("0"));
        assert_eq!(run("Exists(qty)").result.as_deref(), Some("1"));
        assert!(run("func f(n) do Exists(f(n + 1)) endfunc\nf(1)").error.unwrap().contains("nested"));
        // An elseif condition's chain counts from zero, not from the last statement of the arm before.
        let script = format!("if 0 then\nvar x = 1{}\nelseif 1{} then\n2\nendif", "+1".repeat(900), "+1".repeat(200));
        assert_eq!(run(&script).result.as_deref(), Some("2"));
    }

    #[test]
    fn hostile_formcalc_scripts_end_with_an_error_and_bounded_output() {
        let total = "form1[0].page1[0].total[0]";
        let fast = Limits { loop_iterations: 10_000, recursion: 32 };
        let ev = XfaEvent { activity: "calculate".into(), target: total.into(), ..Default::default() };
        let doc = XfaDoc::default();
        let run = |s: &str| run_formcalc(s, &ev, &doc, &form(), fast);
        assert!(run("while (1) do endwhile").error.unwrap().contains("looped"));
        assert!(run("for i = 1 upto 1000000 do endfor").error.unwrap().contains("looped"));
        assert!(run("func f(n) do f(n + 1) endfunc\nf(1)").error.unwrap().contains("nested"));
        assert!(run(&format!("{}1{}", "(".repeat(500), ")".repeat(500))).error.is_some());
        // A 120k-term chain is refused at parse time (its tree would be as deep as it is long).
        assert!(run(&format!("1{}", "+1".repeat(120_000))).error.unwrap().contains("too long"));
        assert!(run(&format!("$form{}", ".a".repeat(120_000))).error.unwrap().contains("too long"));
        assert!(run("nothing.x = 1").error.unwrap().contains("unknown name"));
        assert!(run("$form.nothing = 1").error.unwrap().contains("no nothing"));
        // Values go into fields only: a subform or the form itself is refused, nothing is recorded.
        let o = run("details = \"junk\"");
        assert!(o.error.unwrap().contains("not a field"));
        assert!(o.effects.is_empty());
        assert!(run("$form = \"junk\"").error.unwrap().contains("not a field"));
        // Hostile numbers into the string functions.
        assert_eq!(run(r#"Stuff("abc", 2, 1e300, "X")"#).result.as_deref(), Some("aX"));
        assert_eq!(run(r#"Stuff("abc", -1e300, 1, "X")"#).result.as_deref(), Some("Xbc"));
        assert_eq!(run(r#"Substr("abc", 1e300, 1e300)"#).result.as_deref(), Some(""));
        assert_eq!(run(r#"Left("abc", -1e300)"#).result.as_deref(), Some(""));
        assert!(run(r#"Str(1e300, 1e300, 1e300)"#).error.is_none());
        assert!(run(r#"Round(1e300, 1e300)"#).error.is_none());
        assert!(run(r#"Choose(1e300, 1)"#).error.is_none());
        // Strings can't grow past the cap through any function, and messages and values are clipped.
        assert!(
            run("var s = Space(1000000)\nfor i = 1 upto 10 do s = Encode(Replace(s, \" \", \"&\"), \"html\") endfor\nLen(s)")
                .error
                .unwrap()
                .contains("too long")
        );
        assert!(run("Upper(Concat(Space(600000), Space(600000)))").error.unwrap().contains("too long"));
        let o = run("$host.messageBox(Space(100000))");
        assert!(matches!(o.effects.as_slice(), [XfaEffect::MessageBox(m)] if m.chars().count() <= 4100), "{:?}", o.effects.len());
        let o = run("qty = Space(100000)");
        assert!(matches!(o.effects.as_slice(), [XfaEffect::SetValue { value, .. }] if value.chars().count() == 65_536), "{:?}", o.effects.len());
        assert!(o.notes.iter().any(|n| n.contains("cut")), "{:?}", o.notes);
        // A comment of dashes is not deep nesting.
        assert_eq!(run(&format!("; {}\n1 + 1", "-".repeat(70))).result.as_deref(), Some("2"));
        // Parentheses don't reset the chain count: a tree is never deeper than the cap.
        let mut nested = "1+1+1+1+1+1+1+1+1+1".to_string();
        for _ in 0..30 {
            nested = format!("({nested}{})", "+1".repeat(40));
        }
        assert!(run(&nested).error.unwrap().contains("too long"));
        assert_eq!(run("(1+2)*(3+4) + (5+6)").result.as_deref(), Some("32"));
        // Nested statements inside recursion count against the depth limit.
        let deep = format!("func f(n) do\n{}return f(n - 1)\n{}endfunc\nf(1000)", "if (n > 0) then\n".repeat(20), "endif\n".repeat(20));
        assert!(run(&deep).error.unwrap().contains("nested"));
        // A deadline already passed stops the script at its first look at the clock.
        let o = crate::formcalc::run_formcalc_at(
            "for i = 1 upto 100000 do Eval(\"1\") endfor",
            &ev,
            &doc,
            form(),
            Limits { loop_iterations: u64::MAX, recursion: 32 },
            Some(std::time::Duration::ZERO),
        );
        assert!(o.error.unwrap().contains("too long"));
        assert!(run(&format!("{}1{}", "-".repeat(5000), "")).error.is_some());
        assert!(run("var s = \"x\"\nfor i = 1 upto 100 do s = Concat(s, s) endfor\nLen(s)").error.unwrap().contains("too long"));
        assert!(run("Space(1e12)").error.is_none());
        assert!(run("Replace(Space(100000), \" \", Space(100000))").error.unwrap().contains("too long"));
        assert_eq!(run(r#"Format("YYYY-MM-DD", 1e300)"#).error, None);
        assert_eq!(run(r#"Date2Num("99999999999999999999-01-01", "YYYYYYYYYYYYYYYYYYYY-MM-DD")"#).error, None);
        assert_eq!(run(r#"Time2Num("99999999999999999999:0:0")"#).error, None);
        assert_eq!(run(r#"Num2Date(1e300, "YYYY")"#).result.as_deref(), Some(""));
        assert!(run("for i = 1 upto 1000 do nothing endfor").error.unwrap().contains("unknown name"));
        assert!(run("\"unterminated").error.unwrap().contains("unterminated"));
        assert!(run("1 ) 2").error.is_some());
        assert!(run("Eval(\"Eval(\"\"Eval(\"\"\"\"1\"\"\"\")\"\")\")").error.is_none());
        // A small instance workload must complete with its exact result and effects.
        let o = run("for i = 1 upto 500 do table._row.addInstance(1) endfor\ntable._row.count");
        super::assert_added_instances(&o, 500);
        // A tiny inner loop budget must still report its own diagnostic, and the
        // host boundary discards the partial instance effects before returning it.
        let inner = run_formcalc(
            "for i = 1 upto 100000 do table._row.addInstance(1) endfor\ntable._row.count",
            &ev,
            &doc,
            &form(),
            Limits { loop_iterations: 10, recursion: 32 },
        );
        super::assert_xfa_resource_refusal(&inner);
        assert!(inner.error.as_deref().is_some_and(|error| error.contains("looped")), "{inner:?}");
        assert_eq!(run("1+1").result.as_deref(), Some("2"));
        // Either the inner iteration counter or the outer fuel/deadline may win
        // on the stress workload. Every resource refusal must remain atomic.
        let o = run("for i = 1 upto 100000 do table._row.addInstance(1) endfor\ntable._row.count");
        super::assert_xfa_resource_refusal(&o);
        assert_eq!(run("1+1").result.as_deref(), Some("2"));
        let o = run("for i = 1 upto 5000 do table._row.addInstance(1) endfor\ntable._row.count");
        if o.limited {
            super::assert_xfa_resource_refusal(&o);
        } else {
            super::assert_added_instances(&o, 1000);
        }
        let next = run("1+1");
        assert_eq!(next.error, None, "{next:?}");
        assert!(!next.limited);
        assert_eq!(next.result.as_deref(), Some("2"));
        // Message boxes past the cap are dropped and noted.
        let o = run("for i = 1 upto 500 do $host.messageBox(i) endfor");
        assert_eq!(o.effects.iter().filter(|e| matches!(e, XfaEffect::MessageBox(_))).count(), 100);
        assert!(!o.notes.is_empty());
        // On a small stack (the wasm build's), the limits stop the script before the stack does.
        let small = std::thread::Builder::new()
            .stack_size(1 << 20)
            .spawn(move || {
                let fast = Limits { loop_iterations: 10_000, recursion: 256 };
                let ev = XfaEvent { activity: "click".into(), target: "form1[0].page1[0].go[0]".into(), ..Default::default() };
                let doc = XfaDoc::default();
                let run = |s: &str| run_formcalc(s, &ev, &doc, &form(), fast);
                assert!(run("func f(n) do f(n + 1) endfunc\nf(1)").error.unwrap().contains("nested"));
                let deep = format!("func f(n) do\n{}return f(n - 1)\n{}endfunc\nf(1000)", "if (n > 0) then\n".repeat(20), "endif\n".repeat(20));
                assert!(run(&deep).error.unwrap().contains("nested"));
                let mut nested = "1".to_string();
                for _ in 0..30 {
                    nested = format!("({nested}{})", "+1".repeat(40));
                }
                assert!(run(&nested).error.is_some());
                let chain = format!("1{}", "+1".repeat(999));
                assert_eq!(run(&chain).result.as_deref(), Some("1000"));
                let dots = format!("$form{}", ".parent".repeat(60));
                assert!(run(&dots).error.unwrap().contains("no object"));
                let dots = format!("${}", ".parent".repeat(3));
                assert_eq!(run(&dots).result.as_deref(), Some(""));
            })
            .unwrap();
        small.join().expect("no stack overflow on a 1 MiB stack");
        // Abandoned when it runs past the deadline.
        let o = crate::formcalc::run_formcalc_within(
            "while (1) do endwhile",
            &ev,
            &doc,
            form(),
            Limits { loop_iterations: u64::MAX, recursion: 32 },
            std::time::Duration::from_millis(200),
        );
        assert!(o.abandoned || o.error.is_some(), "{o:?}");
    }
}

fn annotated() -> DocInfo {
    let note = |index: usize, name: &str, locked: bool| AnnotState {
        page: 0,
        index,
        name: name.into(),
        kind: "Text".into(),
        author: "Ann".into(),
        contents: format!("note {index}"),
        rect: [10.0, 10.0, 30.0, 30.0],
        locked,
    };
    let mut on_two = note(0, "c", false);
    on_two.page = 1;
    DocInfo {
        annots: vec![note(0, "a", false), note(1, "b", true), on_two],
        layers: vec![LayerState { name: "Prices".into(), on: true }, LayerState { name: "Notes".into(), on: false }],
        ..doc()
    }
}

fn ask(script: &str, answers: Vec<Answer>) -> Outcome {
    let dialogs = Dialogs { interactive: true, answers };
    run_with(script, &Event::doc("Console"), &doc(), &fields(), &[], Limits::default(), &dialogs)
}

#[test]
fn scripts_read_and_change_comments() {
    let d = annotated();
    let o = run(
        "var all = this.getAnnots(); var first = this.getAnnots({ nPage: 0 });
         var a = this.getAnnot(0, 'a'); a.contents = 'edited'; a.author = 'Bo';
         this.getAnnot(1, 'c').destroy();
         [all.length, first.length, a.type, a.page, a.rect[2], this.getAnnot(0, 'b').readOnly, this.getAnnots(2)].join('|')",
        &Event::doc("Console"),
        &d,
        &[],
        &[],
        Limits::default(),
    );
    assert_eq!(o.error, None);
    assert_eq!(o.result.as_deref(), Some("3|2|Text|0|30|true|"));
    assert_eq!(
        o.annots,
        [
            AnnotChange::Set { page: 0, index: 0, contents: Some("edited".into()), author: Some("Bo".into()) },
            AnnotChange::Delete { page: 1, index: 0 },
        ]
    );
    // Locked comments refuse changes; a deleted one is gone.
    for bad in ["this.getAnnot(0, 'b').contents = 'x'", "this.getAnnot(0, 'b').destroy()", "var c = this.getAnnot(1, 'c'); c.destroy(); c.contents"] {
        let o = run(bad, &Event::doc("Console"), &d, &[], &[], Limits::default());
        assert!(o.error.is_some(), "{bad}");
    }
    assert_eq!(go("String(this.getAnnots())", &Event::doc("Console")).result.as_deref(), Some("null"));
}

#[test]
fn scripts_add_comments() {
    let o = go(
        "var n = this.addAnnot({ type: 'Text', page: 1, point: [100, 700], contents: 'Check this', author: 'Bo' });
         this.addAnnot({ type: 'Square', rect: [50, 60, 10, 20], strokeColor: color.red });
         this.addAnnot({ type: 'Highlight', quads: [0, 10, 40, 10, 0, 0, 40, 0] });
         var gone = this.addAnnot({ type: 'Line', points: [[0, 0], [10, 10]] }); gone.destroy();
         n.contents += '!'; this.getAnnots(1).length",
        &Event::doc("Console"),
    );
    assert_eq!(o.error, None);
    assert_eq!(o.result.as_deref(), Some("1"));
    let added: Vec<&NewAnnot> = o.annots.iter().filter_map(|c| if let AnnotChange::Add(a) = c { Some(a) } else { None }).collect();
    assert_eq!(added.len(), 3);
    assert_eq!((added[0].page, added[0].rect, added[0].contents.as_str()), (1, [100.0, 680.0, 120.0, 700.0], "Check this!"));
    assert_eq!((added[1].rect, added[1].color), ([10.0, 20.0, 50.0, 60.0], Some([1.0, 0.0, 0.0])));
    assert_eq!((added[2].quads.len(), added[2].rect), (1, [0.0, 0.0, 40.0, 10.0]));
    for bad in [
        "this.addAnnot({ type: 'Stamp', rect: [0, 0, 10, 10] })",
        "this.addAnnot({ type: 'Square', page: 3, rect: [0, 0, 10, 10] })",
        "this.addAnnot({ type: 'Square', rect: [0, 0, 10] })",
        "this.addAnnot({ type: 'Square', rect: [0, 0, NaN, 10] })",
        "this.addAnnot({ type: 'Highlight', quads: [1, 2, 3] })",
        "this.addAnnot({ type: 'Line', points: [[[0, 0]], [1, 1]] })",
        "var p = [0]; for (var i = 0; i < 100000; i++) p = [p]; this.addAnnot({ type: 'Line', points: p })",
        "for (var i = 0; i < 1001; i++) this.addAnnot({ type: 'Text', point: [0, 0] })",
    ] {
        assert!(go(bad, &Event::doc("Console")).error.is_some(), "{bad}");
    }
}

#[test]
fn scripts_show_and_hide_layers() {
    let o = run(
        "var l = this.getOCGs(); l[0].state = false; l[1].state = true; l[1].state = true; l[0].name + ':' + l[0].state",
        &Event::doc("Console"),
        &annotated(),
        &[],
        &[],
        Limits::default(),
    );
    assert_eq!(o.result.as_deref(), Some("Prices:false"));
    assert_eq!(o.requests, [Request::SetLayer { index: 0, on: false }, Request::SetLayer { index: 1, on: true }]);
    assert_eq!(go("String(this.getOCGs())", &Event::doc("Console")).result.as_deref(), Some("null"));
}

#[test]
fn dialogs_wait_for_answers() {
    let script = "var r = app.alert({ cMsg: 'Save?', nType: 2, cTitle: 'Order' });
                  var name = r == 4 ? app.response('Your name?', 'Order', 'Ann') : 'nobody';
                  getField('total').value = name; app.alert('Thanks ' + name); name";
    // The first question stops the run, with nothing done yet.
    let o = ask(script, vec![]);
    assert_eq!(o.question, Some(Question::Alert { message: "Save?".into(), title: "Order".into(), icon: 0, buttons: 2 }));
    assert!(o.changed.is_empty() && o.alerts.is_empty() && o.error.is_none());
    let o = ask(script, vec![Answer::Button(4)]);
    assert!(matches!(&o.question, Some(Question::Response { question, default, .. }) if question == "Your name?" && default == "Ann"));
    let o = ask(script, vec![Answer::Button(4), Answer::Text(Some("Bo".into()))]);
    assert_eq!((o.question, o.result.as_deref(), o.alerts), (None, Some("Bo"), vec!["Thanks Bo".to_string()]));
    assert_eq!(o.changed.first().map(|f| f.value.clone()), Some(vec!["Bo".to_string()]));
    // No, or a button the alert doesn't have, takes the other branch.
    for no in [Answer::Button(3), Answer::Button(1), Answer::Text(None)] {
        assert_eq!(ask(script, vec![no]).result.as_deref(), Some("nobody"));
    }
    // A script that catches the stop still waits.
    let o = ask("try { app.response('?') } catch (e) {} getField('total').value = 1;", vec![]);
    assert!(o.question.is_some() && o.changed.is_empty());
    // Without anyone to ask, alerts are listed and answer No or Cancel; response is null.
    let o =
        go("[app.alert('a', 1, 1), app.alert('b', 2, 2), app.alert('c', 0, 3), app.alert('d'), app.response('?')].join()", &Event::doc("Console"));
    assert_eq!((o.result.as_deref(), o.alerts.len(), o.question), (Some("2,3,2,1,"), 4, None));
}

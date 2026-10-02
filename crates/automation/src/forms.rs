//! Form tools: list fields, fill them (several at once, one undo step) and clear the form.

use printcraft_engine::{Edit, FieldValue, FormField, FormFieldKind, field_flags};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed};

fn kind_name(k: FormFieldKind) -> &'static str {
    match k {
        FormFieldKind::Text => "text",
        FormFieldKind::CheckBox => "checkbox",
        FormFieldKind::Radio => "radio",
        FormFieldKind::PushButton => "button",
        FormFieldKind::Combo => "combo",
        FormFieldKind::List => "list",
        FormFieldKind::Signature => "signature",
    }
}

/// A JSON value → the value for this field (booleans for check boxes, arrays for lists…).
fn value_for(f: &FormField, v: &Value) -> Result<FieldValue> {
    let wrong = |what: &str| ToolError::InvalidArgs(format!("{}: {what}", f.name));
    Ok(match (f.kind, v) {
        (FormFieldKind::CheckBox, Value::Bool(b)) => FieldValue::Check(*b),
        (FormFieldKind::Radio, Value::Null) => FieldValue::Radio(None),
        (FormFieldKind::Radio, Value::String(s)) => FieldValue::Radio(Some(s.clone())),
        (FormFieldKind::Combo | FormFieldKind::List, Value::Array(a)) => FieldValue::Choice(
            a.iter().map(|x| x.as_str().map(str::to_owned).ok_or_else(|| wrong("list values must be strings"))).collect::<Result<_>>()?,
        ),
        (FormFieldKind::Combo | FormFieldKind::List, Value::String(s)) => FieldValue::Choice(if s.is_empty() { Vec::new() } else { vec![s.clone()] }),
        (_, Value::String(s)) => FieldValue::Text(s.clone()),
        (_, Value::Number(n)) => FieldValue::Text(n.to_string()),
        (_, Value::Bool(b)) => FieldValue::Text(b.to_string()),
        _ => return Err(wrong("use a string (text, radio option, choice), true/false (check box) or an array (multi-select list)")),
    })
}

impl Automation {
    pub(crate) fn form_fields(&self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let out: Vec<Value> = doc
            .form
            .iter()
            .map(|f| {
                let w = f.widgets.first();
                let rect = w.and_then(|w| {
                    let p = doc.info.pages.get(w.page?)?;
                    let (a, b) = (p.user_to_view(w.rect[0] as f32, w.rect[1] as f32), p.user_to_view(w.rect[2] as f32, w.rect[3] as f32));
                    Some([a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])].map(|v| (v * 100.0).round() / 100.0))
                });
                let mut v = json!({
                    "name": f.name,
                    "type": kind_name(f.kind),
                    "value": match f.kind {
                        FormFieldKind::CheckBox => json!(!f.value.is_empty()),
                        FormFieldKind::List if f.has(field_flags::MULTI_SELECT) => json!(f.value),
                        _ => json!(f.value.first()),
                    },
                    "page": w.and_then(|w| w.page).map(|p| p + 1),
                    "rect": rect,
                    "read_only": f.read_only(),
                    "required": f.has(field_flags::REQUIRED),
                });
                let o = v.as_object_mut().expect("object");
                if let Some(t) = &f.tooltip {
                    o.insert("tooltip".into(), json!(t));
                }
                match f.kind {
                    FormFieldKind::Radio => {
                        o.insert("options".into(), json!(f.widgets.iter().filter_map(|w| w.on_state.clone()).collect::<Vec<_>>()));
                    }
                    FormFieldKind::Combo | FormFieldKind::List => {
                        o.insert("options".into(), f.options.iter().map(|(e, d)| json!({ "value": e, "label": d })).collect());
                        o.insert("multi_select".into(), json!(f.has(field_flags::MULTI_SELECT)));
                        o.insert("editable".into(), json!(f.has(field_flags::EDIT)));
                    }
                    FormFieldKind::Text => {
                        o.insert("multiline".into(), json!(f.has(field_flags::MULTILINE)));
                        if let Some(m) = f.max_len {
                            o.insert("max_length".into(), json!(m));
                        }
                    }
                    _ => {}
                }
                v
            })
            .collect();
        Ok(json!({ "count": out.len(), "fields": out }))
    }

    pub(crate) fn form_fill(&mut self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let values = a
            .get("values")
            .and_then(Value::as_object)
            .ok_or_else(|| ToolError::InvalidArgs("values must be an object of field name → value".into()))?;
        if values.is_empty() {
            return Err(ToolError::InvalidArgs("values is empty".into()));
        }
        let mut edits = Vec::new();
        for (name, v) in values {
            let f = doc.form.iter().find(|f| &f.name == name).ok_or_else(|| failed(format!("there is no field named {name:?} (see form_fields)")))?;
            edits.push(Edit::SetFieldValue { name: name.clone(), value: value_for(f, v)? });
        }
        let edit = if edits.len() == 1 { edits.remove(0) } else { Edit::Batch { label: "Fill in form".into(), edits } };
        let mut out = self.apply(a, edit)?;
        out["filled"] = json!(values.len());
        Ok(out)
    }

    pub(crate) fn form_reset(&mut self, a: &Args) -> Result<Value> {
        let names = a.get("fields").map(|_| a.strs("fields").map(|v| v.into_iter().map(str::to_owned).collect::<Vec<_>>())).transpose()?;
        self.apply(a, Edit::ResetForm { names })
    }
}

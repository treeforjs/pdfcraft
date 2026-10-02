//! Form tools: list fields, fill them (several at once, one undo step), clear the form, and
//! prepare a form (add, change and delete fields).

use printcraft_engine::{Edit, FieldProps, FieldValue, FormField, FormFieldKind, NewField, field_flags};
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

    pub(crate) fn form_add_field(&mut self, a: &Args) -> Result<Value> {
        let page = self.page(a)?;
        let doc = self.doc(a)?;
        let options = || -> Result<Vec<String>> {
            Ok(a.get("options").map(|_| a.strs("options")).transpose()?.unwrap_or_default().into_iter().map(str::to_owned).collect())
        };
        let kind = match a.str("type")? {
            "text" => NewField::Text { multiline: a.opt_bool("multiline")?.unwrap_or(false) },
            "date" => NewField::Date,
            "checkbox" => NewField::CheckBox,
            "radio" => {
                NewField::Radio { group: a.opt_str("group")?.map(str::to_owned), export: a.opt_str("export")?.unwrap_or("Choice1").to_owned() }
            }
            "combo" => NewField::Combo { options: options()?, editable: a.opt_bool("editable")?.unwrap_or(false) },
            "list" => NewField::List { options: options()?, multi: a.opt_bool("multi_select")?.unwrap_or(false) },
            "button" => NewField::Button { caption: a.opt_str("caption")?.unwrap_or("").to_owned() },
            "signature" => NewField::Signature,
            t => return Err(ToolError::InvalidArgs(format!("unknown field type {t:?}"))),
        };
        let r: Vec<f64> = a.get("rect").and_then(Value::as_array).map(|x| x.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
        let r = <[f64; 4]>::try_from(r).map_err(|_| ToolError::InvalidArgs("rect must be 4 numbers".into()))?;
        // Top-left-origin points on the displayed page → user space.
        let p = &doc.info.pages[page];
        let (u0, u1) = (p.view_to_user(r[0] as f32, r[1] as f32), p.view_to_user(r[2] as f32, r[3] as f32));
        let rect = [u0[0].min(u1[0]) as f64, u0[1].min(u1[1]) as f64, u0[0].max(u1[0]) as f64, u0[1].max(u1[1]) as f64];
        if rect[2] - rect[0] < 1.0 || rect[3] - rect[1] < 1.0 {
            return Err(ToolError::InvalidArgs("rect is empty".into()));
        }
        let before: Vec<String> = doc.form.iter().map(|f| f.name.clone()).collect();
        let name = a.opt_str("name")?.map(str::to_owned);
        let mut out = self.apply(a, Edit::AddField { page, rect, kind, name })?;
        let doc = self.doc(a)?;
        let added = doc
            .form
            .iter()
            .map(|f| &f.name)
            .find(|n| !before.contains(n))
            .or_else(|| a.opt_str("group").ok().flatten().and_then(|g| doc.form.iter().map(|f| &f.name).find(|n| *n == g)));
        out["field"] = json!(added);
        Ok(out)
    }

    pub(crate) fn form_set_props(&mut self, a: &Args) -> Result<Value> {
        let name = a.str("field")?.to_owned();
        let doc = self.doc(a)?;
        let f = doc.form.iter().find(|f| f.name == name).ok_or_else(|| failed(format!("there is no field named {name:?} (see form_fields)")))?;
        // Position: top-left-origin points on the displayed page → user space.
        let rect = match a.get("rect") {
            None => None,
            Some(v) => {
                let r: Vec<f64> = v.as_array().map(|x| x.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
                let r = <[f64; 4]>::try_from(r).map_err(|_| ToolError::InvalidArgs("rect must be 4 numbers".into()))?;
                let page = f.widgets.first().and_then(|w| w.page).ok_or_else(|| failed(format!("{name} is not on a page")))?;
                let p = &doc.info.pages[page];
                let (u0, u1) = (p.view_to_user(r[0] as f32, r[1] as f32), p.view_to_user(r[2] as f32, r[3] as f32));
                Some((0, [u0[0].min(u1[0]) as f64, u0[1].min(u1[1]) as f64, u0[0].max(u1[0]) as f64, u0[1].max(u1[1]) as f64]))
            }
        };
        let props = FieldProps {
            rect,
            name: a.opt_str("name")?.map(str::to_owned),
            tooltip: a.opt_str("tooltip")?.map(str::to_owned),
            read_only: a.opt_bool("read_only")?,
            required: a.opt_bool("required")?,
            multiline: a.opt_bool("multiline")?,
            max_len: match a.opt_int("max_length")? {
                None => None,
                Some(0) => Some(None),
                Some(n) if n > 0 => Some(Some(n as usize)),
                Some(_) => return Err(ToolError::InvalidArgs("max_length must be 0 (no limit) or more".into())),
            },
            options: a.get("options").map(|_| a.strs("options")).transpose()?.map(|v| v.into_iter().map(str::to_owned).collect()),
            font_size: a.opt_num("font_size")?,
        };
        if props == FieldProps::default() {
            return Err(ToolError::InvalidArgs("nothing to change".into()));
        }
        let new_name = props.name.clone().unwrap_or(name.clone());
        let mut out = self.apply(a, Edit::SetFieldProps { name, props })?;
        out["field"] = json!(new_name);
        Ok(out)
    }

    pub(crate) fn form_delete_field(&mut self, a: &Args) -> Result<Value> {
        let name = a.str("field")?.to_owned();
        if !self.doc(a)?.form.iter().any(|f| f.name == name) {
            return Err(failed(format!("there is no field named {name:?} (see form_fields)")));
        }
        self.apply(a, Edit::DeleteField { name })
    }
}

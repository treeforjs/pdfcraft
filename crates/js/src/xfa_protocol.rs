//! XFA form messages shared with the isolated script kernel.

/// What kind of template object a node is.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum XfaKind {
    /// `xfa.form` itself.
    Form,
    Subform,
    Field,
    ExclGroup,
    Draw,
    Area,
}

impl XfaKind {
    #[cfg(target_os = "wasi")]
    pub(crate) fn class_name(self) -> &'static str {
        match self {
            XfaKind::Form => "form",
            XfaKind::Subform => "subform",
            XfaKind::Field => "field",
            XfaKind::ExclGroup => "exclGroup",
            XfaKind::Draw => "draw",
            XfaKind::Area => "area",
        }
    }
}

/// One object of the form, as the caller snapshots it.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct XfaNode {
    pub name: String,
    /// The SOM expression naming this instance (`form[0].head[0].familyName[0]`).
    pub som: String,
    pub kind: Option<XfaKind>,
    /// Fields: the raw value (ISO dates, on values for buttons); empty when null.
    pub value: String,
    /// Numeric fields give numbers to scripts.
    pub numeric: bool,
    /// `visible`, `invisible`, `hidden` or `inactive`.
    pub presence: String,
    /// `open`, `readOnly`, `protected` or `nonInteractive`.
    pub access: String,
    /// Subforms: may have several instances (`occur max` other than 1).
    pub repeatable: bool,
    pub occur_min: usize,
    pub occur_max: Option<usize>,
    /// This instance's index among its same-named siblings.
    pub index: usize,
    pub children: Vec<XfaNode>,
}

/// The event a script runs for.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct XfaEvent {
    /// `click`, `calculate`, `validate`, `initialize`, `change`, `exit`, …
    pub activity: String,
    /// The SOM of `this`.
    pub target: String,
    pub new_text: String,
    pub prev_text: String,
}

/// About the document.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct XfaDoc {
    pub file_name: String,
    /// The current page, 0-based.
    pub page: usize,
    pub page_count: usize,
}

/// Something a script changed or asked for.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub enum XfaEffect {
    SetValue {
        som: String,
        value: String,
    },
    SetPresence {
        som: String,
        presence: String,
    },
    SetAccess {
        som: String,
        access: String,
    },
    /// A new instance of a repeating subform, named by its own SOM (`…row[3]`).
    AddInstance {
        som: String,
    },
    /// The instance at this SOM is removed.
    RemoveInstance {
        som: String,
    },
    MessageBox(String),
    /// `xfa.host.resetData()`: the named fields, or every field when empty.
    ResetData(Vec<String>),
    Print,
    SaveAs,
    LaunchUrl(String),
    SetFocus(String),
    Beep,
    /// `xfa.form.recalculate(true)`.
    Recalculate,
    /// `xfa.form.remerge()`, `xfa.layout.relayout()`.
    Relayout,
}

/// What running a script did.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct XfaOutcome {
    /// The isolated run exhausted a resource budget; stop the current form-event batch.
    #[serde(default)]
    pub limited: bool,
    pub effects: Vec<XfaEffect>,
    pub console: Vec<String>,
    /// The script's error, if it failed.
    pub error: Option<String>,
    /// The script's completion value (a calculate script's result; a validate script's verdict).
    pub result: Option<String>,
    /// The completion value as a boolean, when it was one.
    pub result_bool: Option<bool>,
    /// Limits the script ran into (too many effects, message boxes or console lines): what
    /// was left out, for the caller to report.
    pub notes: Vec<String>,
    /// Compatibility flag for old callers. Isolated runs always stop and discard their store;
    /// no background script is ever abandoned, so this remains false.
    pub abandoned: bool,
}

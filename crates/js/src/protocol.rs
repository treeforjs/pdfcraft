//! Shared, versioned messages used by the isolated script kernel.

/// The kinds of field, as `field.type` names them.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    Text,
    CheckBox,
    RadioButton,
    ComboBox,
    ListBox,
    Button,
    Signature,
}

impl FieldType {
    pub fn name(self) -> &'static str {
        match self {
            FieldType::Text => "text",
            FieldType::CheckBox => "checkbox",
            FieldType::RadioButton => "radiobutton",
            FieldType::ComboBox => "combobox",
            FieldType::ListBox => "listbox",
            FieldType::Button => "button",
            FieldType::Signature => "signature",
        }
    }
}

/// `display.*`: visible, hidden, noPrint, noView.
pub const DISPLAY_VISIBLE: i32 = 0;
pub const DISPLAY_HIDDEN: i32 = 1;
pub const DISPLAY_NO_PRINT: i32 = 2;
pub const DISPLAY_NO_VIEW: i32 = 3;

/// A field as scripts see it.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct FieldState {
    pub name: String,
    pub kind: FieldType,
    /// Text: the text. Check box / radio: the selected export value (empty when off). Choice:
    /// the selected export values.
    pub value: Vec<String>,
    pub default: Vec<String>,
    pub readonly: bool,
    pub required: bool,
    pub display: i32,
    /// Choice options (export value, display text); check boxes and radios: their export
    /// values, one per widget.
    pub options: Vec<(String, String)>,
    pub char_limit: Option<usize>,
    /// `textColor` / `fillColor` as PDF colour arrays (`["RGB", r, g, b]` etc.), when set.
    pub text_color: Option<Vec<String>>,
    pub fill_color: Option<Vec<String>>,
}

impl FieldState {
    pub fn new(name: impl Into<String>, kind: FieldType, value: Vec<String>) -> FieldState {
        FieldState {
            name: name.into(),
            kind,
            value,
            default: Vec::new(),
            readonly: false,
            required: false,
            display: DISPLAY_VISIBLE,
            options: Vec::new(),
            char_limit: None,
            text_color: None,
            fill_color: None,
        }
    }

    #[cfg(target_os = "wasi")]
    pub(crate) fn single(&self) -> String {
        self.value.first().cloned().unwrap_or_default()
    }
}

/// The event a script runs for.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Event {
    /// `Keystroke`, `Validate`, `Calculate`, `Format`, `Mouse Up`, `Open`, …
    pub name: String,
    /// `Field`, `Doc`, `Page`, …
    pub kind: String,
    /// The field the event is for.
    pub target: Option<String>,
    pub value: String,
    pub change: String,
    pub will_commit: bool,
}

impl Event {
    /// A field event (`event.type` = "Field").
    pub fn field(name: &str, target: &str, value: &str) -> Event {
        Event { name: name.into(), kind: "Field".into(), target: Some(target.into()), value: value.into(), change: String::new(), will_commit: true }
    }

    /// A document event (`event.type` = "Doc"), e.g. Open or WillSave.
    pub fn doc(name: &str) -> Event {
        Event { name: name.into(), kind: "Doc".into(), target: None, value: String::new(), change: String::new(), will_commit: false }
    }
}

/// About the document, for `this.numPages`, `this.info` and the like.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct DocInfo {
    pub file_name: String,
    pub num_pages: usize,
    /// The current page, 0-based.
    pub page: usize,
    /// Document information (`this.info.title` etc.): lower-case key, value.
    pub info: Vec<(String, String)>,
    /// The document's comments, for `this.getAnnots()`.
    pub annots: Vec<AnnotState>,
    /// The document's layers (optional content groups), in the Layers panel's order.
    pub layers: Vec<LayerState>,
}

/// A comment as scripts see it.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct AnnotState {
    /// 0-based page.
    pub page: usize,
    /// Position in the page's `/Annots`.
    pub index: usize,
    /// `/NM`.
    pub name: String,
    /// The subtype: `Text`, `FreeText`, `Square`, `Highlight`, …
    pub kind: String,
    pub author: String,
    pub contents: String,
    /// In user space: [x0, y0, x1, y1].
    pub rect: [f64; 4],
    /// The Locked flag: scripts can't change or delete it.
    pub locked: bool,
}

/// A comment a script added with `this.addAnnot`.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct NewAnnot {
    /// One of [`ADDABLE_ANNOTS`].
    pub kind: String,
    pub page: usize,
    pub rect: [f64; 4],
    /// A line's two ends.
    pub points: Vec<[f64; 2]>,
    /// Text markup quadrilaterals (8 numbers each).
    pub quads: Vec<[f64; 8]>,
    pub contents: String,
    pub author: String,
    /// `strokeColor` as RGB.
    pub color: Option<[f64; 3]>,
    /// A text box's `textSize`.
    pub text_size: Option<f64>,
}

/// The types `this.addAnnot` can add.
pub const ADDABLE_ANNOTS: [&str; 9] = ["Text", "FreeText", "Square", "Circle", "Line", "Highlight", "Underline", "StrikeOut", "Squiggly"];

/// A change a script made to the comments.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub enum AnnotChange {
    /// A comment's text or author changed (each `None` when unchanged).
    Set {
        page: usize,
        index: usize,
        contents: Option<String>,
        author: Option<String>,
    },
    /// `annot.destroy()`.
    Delete {
        page: usize,
        index: usize,
    },
    Add(NewAnnot),
}

/// A layer as scripts see it.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct LayerState {
    pub name: String,
    pub on: bool,
}

/// A dialog that waits for the user ([`Dialogs`]).
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub enum Question {
    /// `app.alert` with buttons: `buttons` is `nType` (1 OK/Cancel, 2 Yes/No, 3 Yes/No/Cancel),
    /// `icon` is `nIcon` (0 error, 1 warning, 2 question, 3 status).
    Alert { message: String, title: String, icon: i32, buttons: i32 },
    /// `app.response`: a line of text.
    Response { question: String, title: String, default: String, label: String, password: bool },
}

impl Question {
    /// The button numbers an alert offers, as `app.alert` returns them (1 OK, 2 Cancel, 3 No,
    /// 4 Yes), in reading order.
    pub fn buttons(&self) -> &'static [i32] {
        match self {
            Question::Alert { buttons: 1, .. } => &[1, 2],
            Question::Alert { buttons: 2, .. } => &[4, 3],
            Question::Alert { buttons: 3, .. } => &[4, 3, 2],
            Question::Alert { .. } => &[1],
            Question::Response { .. } => &[],
        }
    }
}

/// The user's answer to a [`Question`].
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub enum Answer {
    /// The button pressed (see [`Question::buttons`]).
    Button(i32),
    /// The text entered; `None` for Cancel.
    Text(Option<String>),
}

/// How dialogs that wait for an answer behave.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Dialogs {
    /// Stop at the first unanswered question ([`Outcome::question`]). Otherwise every
    /// question gets the Cancel or No answer and alerts are listed in [`Outcome::alerts`].
    pub interactive: bool,
    /// Answers to the questions so far, in the order the script asks them.
    pub answers: Vec<Answer>,
}

/// Something a script asked the viewer to do.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub enum Request {
    /// `this.resetForm([names])` (empty: every field).
    Reset(Vec<String>),
    Print,
    /// `this.pageNum = n` (0-based).
    GoToPage(usize),
    LaunchUrl(String),
    /// `this.submitForm(url)`: never sent on PdfCraft's own.
    Submit(String),
    /// `field.setFocus()`.
    Focus(String),
    Beep,
    /// `app.execMenuItem("SaveAs")` (XFA forms' Save buttons).
    SaveAs,
    /// `ocg.state = …`: show or hide layer `index` (in [`DocInfo::layers`]).
    SetLayer {
        index: usize,
        on: bool,
    },
}

/// What running a script did.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    /// The isolated run exhausted a resource budget; callers must stop the current batch.
    #[serde(default)]
    pub limited: bool,
    /// `event.rc`: `false` rejects the keystroke or value.
    pub rc: bool,
    /// `event.value` after the script.
    pub value: String,
    pub change: String,
    /// Fields whose value or properties the script changed, as they are now.
    pub changed: Vec<FieldState>,
    pub alerts: Vec<String>,
    pub console: Vec<String>,
    pub requests: Vec<Request>,
    /// The script's error (syntax error, exception, or a limit reached), if it failed.
    pub error: Option<String>,
    /// The script's completion value (what the console prints), unless undefined.
    pub result: Option<String>,
    /// Comments the script changed, deleted or added, in that order.
    pub annots: Vec<AnnotChange>,
    /// The dialog the script is waiting on ([`Dialogs::interactive`]); when set, the run did
    /// nothing else and the rest of the outcome is empty.
    pub question: Option<Question>,
}

/// Bounds that keep a script from hanging the viewer.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub loop_iterations: u64,
    pub recursion: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { loop_iterations: 1_000_000, recursion: 256 }
    }
}

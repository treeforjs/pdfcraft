//! Trust Manager ▸ file attachments: which attachment types may be opened or launched.
//!
//! Acrobat keeps a list of file extensions it will and won't open from a PDF's attachments:
//! programs and scripts (`.exe`, `.bat`, `.js`, `.vbs`, …) are blocked, and a type it doesn't
//! know asks first. This is the same policy, kept on the [`Session`] so the app and the
//! automation tools agree.
//!
//! - A built-in deny list ([`BUILTIN_DENY`]) of types that run code when opened, and a built-in
//!   allow list ([`BUILTIN_ALLOW`]) of plain document and image types.
//! - The user's own allow and deny lists ([`AttachmentTrust`]) override both: user deny, then
//!   user allow, then the built-in lists. Anything else (or a name with no extension) prompts.
//!
//! Names come from the document, so they are hostile: [`check`] looks at the name the way an
//! operating system would launch it. Only the last path component counts; trailing dots and
//! spaces (which Windows drops) and invisible or direction-changing characters (which disguise
//! `invoice<U+202E>fdp.exe` as `invoiceexe.pdf`) are removed; case is folded; and on Windows the part
//! before an NTFS stream suffix (`x.exe:stream`) is what runs, so both readings are checked and
//! the stricter wins. Only the final extension decides how a file opens, so `report.pdf.exe` is
//! an `.exe` (and is flagged as disguised), while `notes.exe.pdf` is a PDF.

use serde::{Deserialize, Serialize};

use crate::Session;

/// Types that run code (or install, or point at something that does) when opened.
#[rustfmt::skip]
pub const BUILTIN_DENY: &[&str] = &[
    // Windows programs, installers and libraries.
    "exe", "com", "scr", "pif", "msi", "msp", "mst", "msix", "msixbundle", "appx", "appxbundle", "cpl", "dll", "ocx", "sys", "drv", "gadget",
    "application", "appref-ms", "xbap", "diagcab",
    // Windows scripts and script hosts.
    "bat", "cmd", "js", "jse", "vb", "vbs", "vbe", "wsf", "wsh", "wsc", "ws", "hta", "sct", "ps1", "ps1xml", "ps2", "ps2xml", "psc1", "psc2",
    "psd1", "psm1", "msh", "msh1", "msh2", "mshxml", "msh1xml", "msh2xml",
    // Windows shortcuts and settings that launch or change things.
    "lnk", "url", "scf", "inf", "reg", "msc", "chm", "hlp", "shs", "shb", "settingcontent-ms", "library-ms", "website", "jnlp",
    // Disk images, which mount and run.
    "iso", "img", "vhd", "vhdx",
    // Office documents with macros, add-ins and Access databases.
    "docm", "dotm", "xlsm", "xltm", "xlam", "xll", "pptm", "potm", "ppam", "ppsm", "sldm", "iqy", "slk", "mde", "mda", "ade", "adp", "accde",
    "mdb", "mdt", "mdw", "mdz",
    // macOS applications, installers and scripts.
    "app", "pkg", "mpkg", "dmg", "command", "tool", "workflow", "action", "scpt", "scptd", "applescript", "terminal", "osax", "prefpane",
    "kext", "dylib", "fileloc", "inetloc", "webloc",
    // Unix programs, packages and shell scripts.
    "sh", "bash", "zsh", "csh", "tcsh", "ksh", "fish", "run", "bin", "out", "elf", "so", "desktop", "deb", "rpm", "appimage", "snap",
    "flatpakref",
    // Scripting languages and runtimes that run a file by double-click.
    "jar", "class", "py", "pyc", "pyo", "pyw", "pyz", "pl", "pm", "rb", "php", "lua", "tcl", "vbscript", "wasm",
    // Mobile apps.
    "apk", "ipa", "xap",
];

/// Plain document, data and image types that open in a viewer rather than run.
#[rustfmt::skip]
pub const BUILTIN_ALLOW: &[&str] = &[
    "pdf", "fdf", "xfdf", "txt", "text", "csv", "tsv", "md", "log", "json", "xml", "png", "jpg", "jpeg", "gif", "bmp", "tif", "tiff", "webp",
];

/// Most entries in each user list (settings and tool arguments are untrusted).
pub const MAX_USER_EXTENSIONS: usize = 256;
/// Longest extension a user list may hold.
pub const MAX_EXTENSION_LEN: usize = 32;
/// Characters of an attachment name looked at (a longer one is cut).
const MAX_NAME_CHARS: usize = 1024;
/// Most extensions reported from one name.
const MAX_INNER_EXTENSIONS: usize = 16;

/// What may happen when an attachment is opened or launched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    /// Open it.
    Allow,
    /// Ask first: the type is unknown.
    Prompt,
    /// Never open or launch it.
    Block,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Prompt => "prompt",
            Decision::Block => "block",
        }
    }
}

/// Which rule gave the decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    UserDeny,
    UserAllow,
    BuiltinDeny,
    BuiltinAllow,
    Unknown,
    NoExtension,
}

/// The verdict on one attachment name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Check {
    /// The name as given (cut to a sane length).
    pub name: String,
    /// The name as it would be launched: last path component, cleaned and lower-cased.
    pub normalized: String,
    /// The extension that decides (lower case, no dot); `None` when the name has none.
    pub extension: Option<String>,
    /// Extensions before the last one (`report.pdf.exe` → `["pdf"]`).
    pub inner_extensions: Vec<String>,
    pub decision: Decision,
    pub basis: Basis,
    /// The name pretends to be another type: an allowed inner extension in front of a blocked or
    /// unknown one, or hidden direction/invisible characters.
    pub disguised: bool,
    /// One sentence saying why.
    pub reason: String,
}

/// The user's overrides of the built-in lists (Trust Manager ▸ attachments). Extensions are lower
/// case without the dot; one extension is never in both lists.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentTrust {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

/// A user-supplied extension (`.EXE`, `exe`, `*.exe`) in canonical form, or why it can't be one.
pub fn normalize_extension(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    let s = s.strip_prefix('*').unwrap_or(s);
    let s = s.trim_start_matches('.').trim_end_matches(|c: char| c == '.' || c.is_whitespace()).to_lowercase();
    if s.is_empty() {
        return Err(format!("{raw:?} is not a file extension"));
    }
    if s.chars().count() > MAX_EXTENSION_LEN {
        return Err(format!("{raw:?} is longer than {MAX_EXTENSION_LEN} characters"));
    }
    if !s.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '+' | '~' | '$')) {
        return Err(format!("{raw:?} is not a file extension (letters, digits and - _ + ~ $ only, without dots)"));
    }
    Ok(s)
}

/// Invisible and direction-changing characters that make a name read as something it isn't.
fn is_disguise_char(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

fn trim_launch(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c.is_control()).trim_end_matches(|c: char| c == '.' || c.is_whitespace() || c.is_control())
}

/// The last path component as a launcher would see it, plus whether hidden characters were found.
fn launch_name(raw: &str) -> (String, bool) {
    let cut: String = raw.chars().take(MAX_NAME_CHARS).collect();
    let hidden = cut.chars().any(is_disguise_char);
    let clean: String = cut.chars().filter(|c| !is_disguise_char(*c)).collect();
    // Trailing separators, dots and spaces first, so `x.exe/` and `x.exe. .` are still `x.exe`.
    let trimmed = clean.trim_end_matches(|c: char| matches!(c, '/' | '\\') || c == '.' || c.is_whitespace() || c.is_control());
    let last = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    (trim_launch(last).to_lowercase(), hidden)
}

/// (inner extensions, final extension) of a cleaned name.
fn extensions(name: &str) -> (Vec<String>, Option<String>) {
    let mut parts: Vec<&str> = name.split('.').collect();
    if parts.len() < 2 {
        return (Vec::new(), None);
    }
    let last = parts.pop().map(trim_launch).filter(|e| !e.is_empty()).map(str::to_string);
    let mut inner: Vec<String> = parts.iter().skip(1).map(|p| trim_launch(p)).filter(|p| !p.is_empty()).map(str::to_string).collect();
    // Keep the ones nearest the real extension.
    let extra = inner.len().saturating_sub(MAX_INNER_EXTENSIONS);
    inner.drain(..extra);
    (inner, last)
}

impl AttachmentTrust {
    /// Lists checked and canonicalized; an extension in both is refused.
    pub fn new(allow: &[String], deny: &[String]) -> Result<Self, String> {
        let canon = |list: &[String], what: &str| -> Result<Vec<String>, String> {
            if list.len() > MAX_USER_EXTENSIONS {
                return Err(format!("the {what} list can hold at most {MAX_USER_EXTENSIONS} extensions"));
            }
            let mut out: Vec<String> = Vec::new();
            for e in list {
                let e = normalize_extension(e)?;
                if !out.contains(&e) {
                    out.push(e);
                }
            }
            out.sort();
            Ok(out)
        };
        let allow = canon(allow, "allow")?;
        let deny = canon(deny, "deny")?;
        if let Some(both) = allow.iter().find(|e| deny.contains(e)) {
            return Err(format!(".{both} is in both the allow and the deny list"));
        }
        Ok(Self { allow, deny })
    }

    /// Restore saved settings leniently: bad entries are skipped, the lists are capped, and an
    /// extension in both lists stays denied.
    pub fn from_settings(v: &serde_json::Value) -> Self {
        let list = |key: &str| -> Vec<String> {
            let mut out: Vec<String> = Vec::new();
            for e in v.get(key).and_then(|l| l.as_array()).into_iter().flatten().filter_map(|e| e.as_str()) {
                if out.len() >= MAX_USER_EXTENSIONS {
                    break;
                }
                if let Ok(e) = normalize_extension(e)
                    && !out.contains(&e)
                {
                    out.push(e);
                }
            }
            out.sort();
            out
        };
        let deny = list("deny");
        let allow = list("allow").into_iter().filter(|e| !deny.contains(e)).collect();
        Self { allow, deny }
    }

    /// The settings form [`AttachmentTrust::from_settings`] reads.
    pub fn to_settings(&self) -> serde_json::Value {
        serde_json::json!({ "allow": self.allow, "deny": self.deny })
    }

    /// Whether nothing overrides the built-in lists.
    pub fn is_default(&self) -> bool {
        self.allow.is_empty() && self.deny.is_empty()
    }

    fn rule(&self, ext: &str) -> (Decision, Basis) {
        if self.deny.iter().any(|e| e == ext) {
            (Decision::Block, Basis::UserDeny)
        } else if self.allow.iter().any(|e| e == ext) {
            (Decision::Allow, Basis::UserAllow)
        } else if BUILTIN_DENY.contains(&ext) {
            (Decision::Block, Basis::BuiltinDeny)
        } else if BUILTIN_ALLOW.contains(&ext) {
            (Decision::Allow, Basis::BuiltinAllow)
        } else {
            (Decision::Prompt, Basis::Unknown)
        }
    }

    fn check_one(&self, name: &str, normalized: String, hidden: bool) -> Check {
        let (inner, ext) = extensions(&normalized);
        let (decision, basis) = match &ext {
            Some(e) => self.rule(e),
            None => (Decision::Prompt, Basis::NoExtension),
        };
        let looks_like = inner.iter().rev().find(|e| self.rule(e).0 == Decision::Allow).cloned();
        let disguised = hidden || (decision != Decision::Allow && looks_like.is_some());
        let shown = ext.as_deref().map(|e| format!(".{e}")).unwrap_or_default();
        let mut reason = match basis {
            Basis::UserDeny => format!("{shown} files are on your list of attachment types never to open"),
            Basis::UserAllow => format!("{shown} files are on your list of attachment types to open"),
            Basis::BuiltinDeny => format!("{shown} files can run programs or scripts on this computer, so they are never opened from a PDF"),
            Basis::BuiltinAllow => format!("{shown} files are documents or images that open in a viewer"),
            Basis::Unknown => format!("{shown} is not a known safe type: confirm before opening it"),
            Basis::NoExtension => "the name has no file extension, so its type is unknown: confirm before opening it".to_string(),
        };
        if decision != Decision::Allow
            && let Some(l) = &looks_like
        {
            reason.push_str(&format!(" (the name is made to look like a .{l} file)"));
        }
        if hidden {
            reason.push_str(" (the name contains invisible or direction-changing characters that can disguise its type)");
        }
        Check {
            name: name.chars().take(MAX_NAME_CHARS).collect(),
            normalized,
            extension: ext,
            inner_extensions: inner,
            decision,
            basis,
            disguised,
            reason,
        }
    }

    /// The verdict on opening or launching an attachment called `name`.
    pub fn check(&self, name: &str) -> Check {
        let (normalized, hidden) = launch_name(name);
        let full = self.check_one(name, normalized.clone(), hidden);
        // `x.exe:stream` launches `x.exe` on Windows (an NTFS alternate data stream), while
        // elsewhere the colon is part of the name: the stricter reading wins.
        let Some((before, _)) = normalized.split_once(':') else { return full };
        let stream = self.check_one(name, trim_launch(before).to_string(), hidden);
        if stream.decision > full.decision { stream } else { full }
    }
}

/// [`AttachmentTrust::check`] with no user overrides.
pub fn check(name: &str) -> Check {
    AttachmentTrust::default().check(name)
}

impl Session {
    /// The user's attachment allow/deny lists.
    pub fn attachment_trust(&self) -> &AttachmentTrust {
        &self.attachment_trust
    }

    pub fn set_attachment_trust(&mut self, trust: AttachmentTrust) {
        self.attachment_trust = trust;
    }

    /// The verdict on opening or launching an attachment called `name` under this session's policy.
    pub fn check_attachment(&self, name: &str) -> Check {
        self.attachment_trust.check(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(name: &str) -> Decision {
        check(name).decision
    }

    #[test]
    fn executables_and_scripts_are_blocked() {
        for n in ["setup.exe", "run.bat", "x.cmd", "payload.js", "macro.vbs", "a.ps1", "a.scr", "a.lnk", "a.hta", "a.jar", "a.sh", "a.app", "a.docm"]
        {
            assert_eq!(d(n), Decision::Block, "{n}");
            assert_eq!(check(n).basis, Basis::BuiltinDeny, "{n}");
        }
    }

    #[test]
    fn documents_are_allowed_and_unknown_types_prompt() {
        for n in ["report.pdf", "data.csv", "photo.JPG", "notes.txt"] {
            assert_eq!(d(n), Decision::Allow, "{n}");
        }
        assert_eq!(check("design.sketchfile").basis, Basis::Unknown);
        assert_eq!(d("design.sketchfile"), Decision::Prompt);
        assert_eq!(check("README").basis, Basis::NoExtension);
        assert_eq!(d("README"), Decision::Prompt);
        assert_eq!(d(""), Decision::Prompt);
        assert_eq!(d("..."), Decision::Prompt);
    }

    #[test]
    fn case_trailing_dots_and_spaces_are_normalized() {
        for n in ["SETUP.EXE", "Setup.Exe", "setup.exe.", "setup.exe . . ", "setup.exe\t", " setup.exe", "setup.exe\0", "setup..exe"] {
            let c = check(n);
            assert_eq!(c.decision, Decision::Block, "{n:?}");
            assert_eq!(c.extension.as_deref(), Some("exe"), "{n:?}");
        }
        assert_eq!(check("setup.exe. .").normalized, "setup.exe");
    }

    #[test]
    fn only_the_last_path_component_counts() {
        assert_eq!(d("dir.pdf/evil.exe"), Decision::Block);
        assert_eq!(d("C:\\Users\\x\\evil.bat"), Decision::Block);
        assert_eq!(d("evil.exe/"), Decision::Block);
        assert_eq!(d("dir.exe/fine.pdf"), Decision::Allow);
    }

    #[test]
    fn double_extensions_use_the_last_and_flag_disguises() {
        let c = check("invoice.pdf.exe");
        assert_eq!(c.decision, Decision::Block);
        assert_eq!(c.extension.as_deref(), Some("exe"));
        assert_eq!(c.inner_extensions, vec!["pdf"]);
        assert!(c.disguised);
        assert!(c.reason.contains("look like a .pdf"), "{}", c.reason);
        // Spaces padding the fake extension away from the real one.
        assert_eq!(d("invoice.pdf                    .exe"), Decision::Block);
        assert!(check("invoice.pdf .js").disguised);
        // The last extension decides how it opens.
        let c = check("notes.exe.pdf");
        assert_eq!(c.decision, Decision::Allow);
        assert!(!c.disguised);
        assert!(check("archive.tar.unknownext").decision == Decision::Prompt);
    }

    #[test]
    fn hidden_characters_are_seen_through() {
        // Right-to-left override: shows as "invoiceexe.pdf".
        let c = check("invoice\u{202E}fdp.exe");
        assert_eq!(c.decision, Decision::Block);
        assert!(c.disguised);
        // A zero-width space inside the extension does not hide it.
        assert_eq!(d("setup.ex\u{200B}e"), Decision::Block);
        assert!(check("report\u{200B}.pdf").disguised);
    }

    #[test]
    fn stream_suffixes_take_the_stricter_reading() {
        assert_eq!(d("evil.exe:Zone.Identifier"), Decision::Block);
        assert_eq!(d("evil.exe::$DATA"), Decision::Block);
        assert_eq!(d("a:b.exe"), Decision::Block);
        assert_eq!(d("time 10:30.pdf"), Decision::Prompt);
    }

    #[test]
    fn user_lists_override_the_built_in_ones() {
        let t = AttachmentTrust::new(&["EXE".into(), ".docx".into()], &["*.PDF".into(), "csv".into()]).unwrap();
        assert_eq!(t.allow, vec!["docx", "exe"]);
        assert_eq!(t.deny, vec!["csv", "pdf"]);
        assert_eq!(t.check("tool.exe").basis, Basis::UserAllow);
        assert_eq!(t.check("tool.exe").decision, Decision::Allow);
        assert_eq!(t.check("letter.docx").decision, Decision::Allow);
        assert_eq!(t.check("a.pdf").decision, Decision::Block);
        assert_eq!(t.check("a.pdf").basis, Basis::UserDeny);
        assert_eq!(t.check("a.bat").decision, Decision::Block);
        // A user-allowed inner extension can disguise too.
        assert!(t.check("letter.docx.vbs").disguised);
    }

    #[test]
    fn bad_user_lists_are_refused() {
        assert!(AttachmentTrust::new(&["exe".into()], &[".EXE".into()]).unwrap_err().contains("both"));
        assert!(AttachmentTrust::new(&["".into()], &[]).is_err());
        assert!(AttachmentTrust::new(&["a.b".into()], &[]).is_err());
        assert!(AttachmentTrust::new(&["a/b".into()], &[]).is_err());
        assert!(AttachmentTrust::new(&["x".repeat(MAX_EXTENSION_LEN + 1)], &[]).is_err());
        let many: Vec<String> = (0..=MAX_USER_EXTENSIONS).map(|i| format!("e{i}")).collect();
        assert!(AttachmentTrust::new(&many, &[]).is_err());
    }

    #[test]
    fn settings_round_trip_and_hostile_settings_are_tamed() {
        let t = AttachmentTrust::new(&["docx".into()], &["zip".into()]).unwrap();
        assert_eq!(AttachmentTrust::from_settings(&t.to_settings()), t);
        let v = serde_json::json!({ "allow": ["exe", 7, "", "a.b", "Zip"], "deny": ["zip", null, "x".repeat(99)] });
        let r = AttachmentTrust::from_settings(&v);
        assert_eq!(r.allow, vec!["exe"]);
        assert_eq!(r.deny, vec!["zip"]);
        let many: Vec<String> = (0..10_000).map(|i| format!("e{i}")).collect();
        assert_eq!(AttachmentTrust::from_settings(&serde_json::json!({ "deny": many })).deny.len(), MAX_USER_EXTENSIONS);
        assert!(AttachmentTrust::from_settings(&serde_json::json!("nonsense")).is_default());
    }

    #[test]
    fn hostile_names_do_not_panic() {
        let long = format!("{}.exe", "a".repeat(100_000));
        let c = check(&long);
        assert!(c.name.chars().count() <= MAX_NAME_CHARS);
        let dots = ".".repeat(50_000) + "x";
        let c = check(&dots);
        assert!(c.inner_extensions.len() <= MAX_INNER_EXTENSIONS);
        for n in ["\u{202E}", ":", "::", "\\", "/", ".exe", "ǅ.ＥＸＥ", "\u{FFFF}.\u{10FFFF}"] {
            let _ = check(n);
        }
        assert_eq!(d(".exe"), Decision::Block);
    }

    #[test]
    fn the_session_keeps_the_policy() {
        let mut s = Session::new();
        assert_eq!(s.check_attachment("a.exe").decision, Decision::Block);
        s.set_attachment_trust(AttachmentTrust::new(&["exe".into()], &[]).unwrap());
        assert_eq!(s.check_attachment("a.exe").decision, Decision::Allow);
        assert_eq!(s.attachment_trust().allow, vec!["exe"]);
    }
}

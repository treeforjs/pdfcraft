//! Preferences → Security → Trust Manager: the sites PDFs may reach (host rules with allow, block
//! or ask, and whether subdomains count) and the attachment types that may be opened or are
//! always refused, on top of the built-in lists. Both are the engine Session's policies and are
//! saved with the app's settings.

use egui::{Align, Layout};
use pdfcraft_engine::attachment_trust::AttachmentTrust;
use pdfcraft_engine::url_access::{HostAction, HostRule};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// What is being typed in the Trust Manager.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustDraft {
    pub host: String,
    pub action: HostAction,
    pub subdomains: bool,
    pub extension: String,
    /// `true`: the next extension goes on the allow list; `false`: the deny list.
    pub allow: bool,
    /// The last refusal, shown under the boxes.
    pub error: Option<String>,
}

impl Default for TrustDraft {
    fn default() -> Self {
        Self { host: String::new(), action: HostAction::Block, subdomains: true, extension: String::new(), allow: false, error: None }
    }
}

/// The longest host or extension that can be typed.
const MAX_TYPED: usize = 255;

fn action_label(a: HostAction) -> &'static str {
    match a {
        HostAction::Allow => tl!("Allow"),
        HostAction::Block => tl!("Block"),
        HostAction::Ask => tl!("Ask each time"),
    }
}

impl PdfCraftApp {
    /// Add (or replace) a host rule; `Err` says why it was refused.
    pub fn trust_add_host(&mut self, host: &str, action: HostAction, subdomains: bool) -> Result<(), String> {
        let rule = HostRule::new(host, action, subdomains)?;
        let mut policy = self.session.url_policy().clone();
        policy.set_rule(rule)?;
        self.session.set_url_policy(&policy);
        Ok(())
    }

    /// Remove the rule for `host`.
    pub fn trust_remove_host(&mut self, host: &str) {
        let mut policy = self.session.url_policy().clone();
        if matches!(policy.remove_rule(host), Ok(true)) {
            self.session.set_url_policy(&policy);
        }
    }

    /// Put `extension` on the allow or the deny list (taking it off the other one).
    pub fn trust_add_extension(&mut self, extension: &str, allow: bool) -> Result<(), String> {
        let e = pdfcraft_engine::attachment_trust::normalize_extension(extension)?;
        let now = self.session.attachment_trust().clone();
        let (mut a, mut d) = (now.allow, now.deny);
        a.retain(|x| *x != e);
        d.retain(|x| *x != e);
        if allow {
            a.push(e)
        } else {
            d.push(e)
        }
        let trust = AttachmentTrust::new(&a, &d)?;
        self.session.set_attachment_trust(trust);
        Ok(())
    }

    /// Take `extension` off both lists (the built-in lists decide again).
    pub fn trust_remove_extension(&mut self, extension: &str) {
        let now = self.session.attachment_trust().clone();
        let (mut a, mut d) = (now.allow, now.deny);
        a.retain(|x| x != extension);
        d.retain(|x| x != extension);
        if let Ok(trust) = AttachmentTrust::new(&a, &d) {
            self.session.set_attachment_trust(trust);
        }
    }
}

/// A small boxed single-line text field.
fn boxed_text(ui: &mut egui::Ui, t: &Tokens, text: &mut String, hint: &str, salt: &str, width: f32) -> egui::Response {
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, t.border))
        .corner_radius(egui::CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(6, 3))
        .show(ui, |ui| {
            ui.add(egui::TextEdit::singleline(text).hint_text(hint).desired_width(width).frame(egui::Frame::NONE).char_limit(MAX_TYPED).id_salt(salt))
        })
        .inner
}

/// The Trust Manager section, under the Security page's settings.
pub(crate) fn section(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) {
    ui.add_space(10.0);
    widgets::section_title(ui, tl!("Trust Manager"));
    ui.label(egui::RichText::new(tl!("Sites")).font(theme::medium(13.0)));
    ui.label(
        egui::RichText::new(tl!(
            "Rules for web sites that links, scripts and form submissions reach. Used when internet access follows the site rules."
        ))
        .small()
        .color(t.text_muted),
    );
    ui.add_space(4.0);
    let rules = app.session.url_policy().rules.clone();
    let mut remove_host = None;
    if rules.is_empty() {
        ui.label(egui::RichText::new(tl!("No site rules yet.")).color(t.text_faint));
    }
    egui::Grid::new("trust-hosts").num_columns(4).spacing([14.0, 4.0]).show(ui, |ui| {
        for r in &rules {
            ui.label(egui::RichText::new(&r.host).monospace());
            ui.label(action_label(r.action));
            ui.label(egui::RichText::new(if r.subdomains { tl!("and subdomains") } else { tl!("this host only") }).color(t.text_muted));
            if ui.small_button(tl!("Remove")).on_hover_text(crate::i18n::fmt(tl!("Remove the rule for {host}"), &[("host", &r.host)])).clicked() {
                remove_host = Some(r.host.clone());
            }
            ui.end_row();
        }
    });
    if let Some(h) = remove_host {
        app.trust_remove_host(&h);
    }
    let mut add_host = false;
    ui.horizontal_wrapped(|ui| {
        let d = &mut app.trust_draft;
        let r = boxed_text(ui, t, &mut d.host, "example.com", "trust-host", 200.0);
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            add_host = true;
        }
        egui::ComboBox::from_id_salt("trust-host-action").selected_text(action_label(d.action)).show_ui(ui, |ui| {
            for a in [HostAction::Allow, HostAction::Block, HostAction::Ask] {
                ui.selectable_value(&mut d.action, a, action_label(a));
            }
        });
        ui.checkbox(&mut d.subdomains, tl!("Include subdomains"));
        if widgets::pill_button(ui, tl!("Add site"), false).clicked() {
            add_host = true;
        }
    });
    if add_host {
        let d = app.trust_draft.clone();
        match app.trust_add_host(&d.host, d.action, d.subdomains) {
            Ok(()) => {
                app.trust_draft.host.clear();
                app.trust_draft.error = None;
            }
            Err(e) => app.trust_draft.error = Some(e),
        }
    }
    ui.add_space(10.0);
    ui.label(egui::RichText::new(tl!("Attachments")).font(theme::medium(13.0)));
    ui.label(
        egui::RichText::new(tl!(
            "Programs and scripts are refused and plain documents open; other types ask. Your lists override the built-in ones."
        ))
        .small()
        .color(t.text_muted),
    );
    ui.add_space(4.0);
    let trust = app.session.attachment_trust().clone();
    let mut remove_ext = None;
    for (title, list) in [(tl!("Always allow"), &trust.allow), (tl!("Never open"), &trust.deny)] {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(title).color(t.text_muted));
            if list.is_empty() {
                ui.label(egui::RichText::new("—").color(t.text_faint));
            }
            for e in list {
                if ui.small_button(format!(".{e}  ✕")).on_hover_text(crate::i18n::fmt(tl!("Take .{ext} off the list"), &[("ext", e)])).clicked() {
                    remove_ext = Some(e.clone());
                }
            }
        });
    }
    if let Some(e) = remove_ext {
        app.trust_remove_extension(&e);
    }
    let mut add_ext = false;
    ui.horizontal_wrapped(|ui| {
        let d = &mut app.trust_draft;
        let r = boxed_text(ui, t, &mut d.extension, tl!("Extension, such as zip"), "trust-extension", 160.0);
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            add_ext = true;
        }
        egui::ComboBox::from_id_salt("trust-extension-list").selected_text(if d.allow { tl!("Always allow") } else { tl!("Never open") }).show_ui(
            ui,
            |ui| {
                ui.selectable_value(&mut d.allow, true, tl!("Always allow"));
                ui.selectable_value(&mut d.allow, false, tl!("Never open"));
            },
        );
        if widgets::pill_button(ui, tl!("Add type"), false).clicked() {
            add_ext = true;
        }
    });
    if add_ext {
        let d = app.trust_draft.clone();
        match app.trust_add_extension(&d.extension, d.allow) {
            Ok(()) => {
                app.trust_draft.extension.clear();
                app.trust_draft.error = None;
            }
            Err(e) => app.trust_draft.error = Some(e),
        }
    }
    if let Some(e) = &app.trust_draft.error {
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.label(egui::RichText::new(e).color(egui::Color32::from_rgb(0xD3, 0x2F, 0x2F)));
        });
    }
}

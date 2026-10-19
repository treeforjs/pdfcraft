//! Updates (issue #28): Help ▸ Check for updates, a quiet daily check, and installing a new
//! version from inside PdfCraft, through the craft apps' shared updater (`craft-update`).
//!
//! The desktop app hands over an [`Updater`] ([`PdfCraftApp::set_updater`]), so this crate has no
//! network code; without one (the web build, tests) the command opens the releases page.
//!
//! - The automatic check waits [`AutoCheck::START_DELAY_SECS`] after the first frame, runs on a
//!   background thread at most once a day, and only ever shows a notice: never a dialog over
//!   the user's work. Settings ▸ General turns it off.
//! - A download is checked against the release's signed checksum list before it is kept, and is
//!   installed when PdfCraft quits (after unsaved documents are handled) or on Restart now.

use egui::{Align, Layout};

pub use craft_update::AutoCheck;
#[cfg(not(target_arch = "wasm32"))]
pub use craft_update::{Error, State, Updater};

use crate::{PdfCraftApp, theme, widgets};

/// Where every PdfCraft release is listed.
pub const RELEASES_PAGE: &str = "https://github.com/treeforjs/pdfcraft/releases";

/// Whether release `latest` (a tag such as `v0.2.0`) is newer than version `current` (`0.1.1`).
/// A version that doesn't parse is never newer.
pub fn is_newer(latest: &str, current: &str) -> bool {
    matches!((craft_update::Version::parse(latest), craft_update::Version::parse(current)), (Some(l), Some(c)) if l > c)
}

#[derive(Default)]
pub(crate) struct Updates {
    /// The Updates dialog is showing.
    pub(crate) open: bool,
    /// When to check by itself (saved with the settings).
    pub(crate) auto: AutoCheck,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) updater: Option<Updater>,
    /// The first frame's time, from which the automatic check's delay counts.
    started: Option<f64>,
    /// The running or last check started by itself (its outcome is quiet).
    background: bool,
    /// A check is running: its end is recorded as the last check.
    checking: bool,
    /// Restart now was chosen: the new version opens after the update installs.
    restart: bool,
}

impl PdfCraftApp {
    /// Use `updater` for checks and installs (the desktop app; see the module docs).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_updater(&mut self, updater: Updater) {
        self.updates.updater = Some(updater);
    }

    /// The updater, if the desktop app set one.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn updater(&self) -> Option<&Updater> {
        self.updates.updater.as_ref()
    }

    /// Whether PdfCraft looks for updates by itself (Settings ▸ General).
    pub fn auto_update_check(&self) -> &AutoCheck {
        &self.updates.auto
    }

    /// Help ▸ Check for updates: look now and show the outcome (or the download in progress).
    pub fn check_for_updates(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(u) = &mut self.updates.updater {
            self.updates.open = true;
            if matches!(u.state(), State::Idle | State::UpToDate | State::Failed(_)) {
                u.dismiss();
                u.check();
                self.updates.checking = true;
            }
            self.updates.background = false;
            return;
        }
        self.open_url(RELEASES_PAGE);
    }

    /// Each frame: pick up news from the updater and start the automatic check when it's due.
    /// `now` is egui's clock (seconds since start).
    pub(crate) fn poll_updates(&mut self, now: f64) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let started = *self.updates.started.get_or_insert(now);
            let Some(u) = &mut self.updates.updater else { return };
            let idle = matches!(u.state(), State::Idle | State::UpToDate | State::Failed(_));
            if idle && !self.updates.open && now - started >= AutoCheck::START_DELAY_SECS as f64 && self.updates.auto.due(AutoCheck::now()) {
                u.check();
                self.updates.checking = true;
                self.updates.background = true;
            }
            if !u.poll() {
                return;
            }
            if self.updates.checking && !matches!(u.state(), State::Checking) {
                self.updates.checking = false;
                self.updates.auto.last_checked = Some(AutoCheck::now());
                if self.updates.background && !self.updates.open {
                    match u.state() {
                        State::Available(offer) if !self.updates.auto.is_skipped(&offer.release.version) => {
                            let v = offer.release.version.to_string();
                            self.notify_fmt("PdfCraft {v} is available. Choose Help ▸ Check for updates to install it.", &[("v", &v)]);
                        }
                        // Nothing to say about a quiet check that found nothing or failed.
                        State::Available(_) | State::UpToDate | State::Failed(_) => u.dismiss(),
                        _ => {}
                    }
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = now;
    }

    /// PdfCraft is quitting (unsaved work already handled): install a downloaded update.
    pub(crate) fn install_update_on_quit(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(u) = &mut self.updates.updater {
            let restart = std::mem::take(&mut self.updates.restart);
            match u.apply(restart) {
                Ok(Some(_)) => log::info!("installing the downloaded update (restart: {restart})"),
                Ok(None) => {}
                Err(e) => log::warn!("couldn't install the downloaded update: {e}"),
            }
        }
    }

    pub(crate) fn persist_updates(&self) -> serde_json::Value {
        serde_json::to_value(&self.updates.auto).unwrap_or_default()
    }

    pub(crate) fn restore_updates(&mut self, v: &serde_json::Value) {
        if let Ok(auto) = serde_json::from_value::<AutoCheck>(v.clone()) {
            self.updates.auto = auto;
        }
    }

    /// Settings ▸ General: the automatic check on or off.
    pub(crate) fn auto_update_checkbox(&mut self, ui: &mut egui::Ui) {
        #[cfg(not(target_arch = "wasm32"))]
        ui.checkbox(&mut self.updates.auto.enabled, tl!("Check for updates automatically"));
        #[cfg(target_arch = "wasm32")]
        let _ = ui;
    }
}

/// What the dialog's buttons asked for.
#[cfg(not(target_arch = "wasm32"))]
enum Action {
    Close,
    Download,
    Skip(String),
    Cancel,
    Page(String),
    Restart,
}

/// The Updates dialog.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn dialog(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if !app.updates.open {
        return;
    }
    let Some(u) = &app.updates.updater else {
        app.updates.open = false;
        return;
    };
    let t = theme::Tokens::get(ctx);
    let current = env!("CARGO_PKG_VERSION");
    let mut action = None;
    let modal = egui::Modal::new(egui::Id::new("updates")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.horizontal(|ui| {
            ui.add(crate::icons::image("cloud", 22.0, t.accent));
            ui.label(egui::RichText::new(tl!("Check for updates")).font(theme::semibold(16.0)));
        });
        ui.add_space(8.0);
        let muted = |s: String| egui::RichText::new(s).color(t.text_muted);
        // Buttons, right to left: (label, primary, action).
        let mut buttons: Vec<(&str, bool, Action)> = Vec::new();
        match u.state() {
            State::Idle => {
                ui.label(tl!("No check has run yet."));
                buttons.push((tl!("Close"), true, Action::Close));
            }
            State::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tl!("Checking for a newer version…"));
                });
                buttons.push((tl!("Close"), false, Action::Close));
            }
            State::UpToDate => {
                ui.label(crate::i18n::fmt(tl!("PdfCraft {c} is up to date."), &[("c", current)]));
                buttons.push((tl!("Close"), true, Action::Close));
            }
            State::Available(offer) => {
                let v = offer.release.version.to_string();
                ui.label(egui::RichText::new(crate::i18n::fmt(tl!("PdfCraft {v} is available."), &[("v", &v)])).strong());
                ui.label(muted(crate::i18n::fmt(tl!("You have version {c}."), &[("c", current)])));
                notes(ui, &offer.release.notes, t.text_muted);
                match &offer.install {
                    Ok(_) => buttons.push((tl!("Download and install"), true, Action::Download)),
                    Err(why) => {
                        ui.add_space(4.0);
                        ui.label(muted(why.clone()));
                        buttons.push((tl!("Open release page"), true, Action::Page(offer.release.page.clone())));
                    }
                }
                buttons.push((tl!("Later"), false, Action::Close));
                buttons.push((tl!("Skip this version"), false, Action::Skip(v)));
            }
            State::Downloading { offer, received, total } => {
                let v = offer.release.version.to_string();
                ui.label(crate::i18n::fmt(tl!("Downloading PdfCraft {v}…"), &[("v", &v)]));
                let fraction = if *total > 0 { (*received as f64 / *total as f64).clamp(0.0, 1.0) as f32 } else { 0.0 };
                let text = format!("{} / {}", crate::panels::human_size(*received as usize), crate::panels::human_size(*total as usize));
                ui.add(egui::ProgressBar::new(fraction).text(text));
                buttons.push((tl!("Cancel"), false, Action::Cancel));
            }
            State::Ready(offer) => {
                let v = offer.release.version.to_string();
                ui.label(egui::RichText::new(crate::i18n::fmt(tl!("PdfCraft {v} is ready to install."), &[("v", &v)])).strong());
                ui.label(muted(tl!("It installs when you quit PdfCraft. Restart now asks about unsaved documents first.").to_string()));
                buttons.push((tl!("Restart now"), true, Action::Restart));
                buttons.push((tl!("Install when I quit"), false, Action::Close));
            }
            State::Failed(Error::Cancelled) => {
                ui.label(tl!("The update was cancelled."));
                buttons.push((tl!("Close"), true, Action::Close));
            }
            State::Failed(e) => {
                ui.label(crate::i18n::fmt(tl!("Couldn't update: {e}"), &[("e", &e.to_string())]));
                ui.label(muted(crate::i18n::fmt(
                    tl!("You have version {c}. All releases are listed at {page}."),
                    &[("c", current), ("page", RELEASES_PAGE)],
                )));
                buttons.push((tl!("Close"), true, Action::Close));
                buttons.push((tl!("Open release page"), false, Action::Page(RELEASES_PAGE.to_string())));
            }
        }
        ui.add_space(10.0);
        ui.label(
            egui::RichText::new(tl!(
                "Downloads come from PdfCraft's GitHub releases and are checked against the release's signed checksums before anything is installed."
            ))
            .color(t.text_muted)
            .small(),
        );
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            for (label, primary, a) in buttons {
                if widgets::pill_button(ui, label, primary).clicked() {
                    action = Some(a);
                }
            }
        });
    });
    if modal.should_close() && action.is_none() {
        action = Some(Action::Close);
    }
    let Some(action) = action else { return };
    let Some(u) = &mut app.updates.updater else { return };
    match action {
        Action::Close => app.updates.open = false,
        Action::Download => u.download(),
        Action::Cancel => u.cancel(),
        Action::Skip(v) => {
            app.updates.auto.skipped = Some(v);
            u.dismiss();
            app.updates.open = false;
        }
        Action::Page(url) => {
            app.updates.open = false;
            app.open_url(&url);
        }
        Action::Restart => {
            app.updates.open = false;
            app.updates.restart = true;
            // The usual way out: unsaved documents are asked about first.
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn dialog(_app: &mut PdfCraftApp, _ctx: &egui::Context) {}

/// The release notes, short: the first lines, in a scroll area.
#[cfg(not(target_arch = "wasm32"))]
fn notes(ui: &mut egui::Ui, notes: &str, color: egui::Color32) {
    let text: String = notes.lines().map(str::trim_end).filter(|l| !l.is_empty()).take(12).collect::<Vec<_>>().join("\n");
    if text.is_empty() {
        return;
    }
    ui.add_space(6.0);
    egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
        ui.label(egui::RichText::new(text).small().color(color));
    });
}

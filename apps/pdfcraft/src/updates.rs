//! In-app updates (issue #28): the shared craft updater, asking GitHub for PdfCraft releases.

use std::sync::Arc;

use craft_update::{AppInfo, GitHub, Updater};

/// The Ed25519 public key that signs releases' checksum lists, set at build time by the
/// release workflow (repository variable `CRAFT_UPDATE_PUBLIC_KEY`). Builds without it still
/// find new versions but never install anything themselves.
const PUBLIC_KEY: Option<&str> = option_env!("CRAFT_UPDATE_PUBLIC_KEY");

/// What the updater needs to know about this build.
pub fn app_info() -> AppInfo {
    AppInfo {
        id: "pdfcraft".into(),
        display_name: "PdfCraft".into(),
        repo: "treeforjs/pdfcraft".into(),
        current: env!("CARGO_PKG_VERSION").into(),
        public_key: PUBLIC_KEY.map(str::trim).filter(|k| !k.is_empty()).map(Into::into),
        prereleases: false,
    }
}

/// The updater for this window; `wake` repaints when there is news. Builds nothing that
/// touches the network or disk: the first check starts later, from the frame loop.
pub fn updater(wake: impl Fn() + Send + Sync + 'static) -> Updater {
    let mut u = Updater::new(app_info(), Arc::new(GitHub::new(concat!("PdfCraft/", env!("CARGO_PKG_VERSION")))));
    u.on_change(wake);
    u
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_build_is_described_for_the_updater() {
        let info = app_info();
        assert_eq!(info.releases_page(), pdfcraft_ui_egui::updates::RELEASES_PAGE);
        assert!(craft_update::Version::parse(&info.current).is_some());
        // A key set at build time must be usable, or installs would silently never happen.
        if info.public_key.is_some() {
            assert!(info.key().is_some(), "CRAFT_UPDATE_PUBLIC_KEY isn't an Ed25519 public key");
        }
    }
}

//! PrintCraft desktop app.
//!
//! Usage: `printcraft [options] [files…]`
//!
//! View options (applied after the files open; also the seed of the UI control channel):
//! `--page N  --zoom 150  --layout continuous|two-up|single  --panel comments|bookmarks|pages|fields|layers|attachments|none
//!  --theme light|dark  --mode all|read|edit|convert|sign  --tool <catalogue id>  --left open|closed
//!  --organize on  --fields on  --dialog properties|shortcuts|about  --palette <query>  --home on`

use printcraft_ui_egui::PrintCraftApp;

fn main() -> eframe::Result {
    let mut files = Vec::new();
    let mut options: Vec<(String, String)> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--version" => {
                println!("printcraft {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            flag if flag.starts_with("--") => {
                let value = args.next().unwrap_or_default();
                options.push((flag.trim_start_matches("--").to_string(), value));
            }
            _ => files.push(a),
        }
    }
    let integrated = cfg!(target_os = "macos");
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("PrintCraft")
        .with_inner_size([1440.0, 920.0])
        .with_min_inner_size([820.0, 520.0])
        .with_drag_and_drop(true);
    if integrated {
        viewport = viewport.with_fullsize_content_view(true).with_titlebar_shown(false).with_title_shown(false);
    }
    let native = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        "PrintCraft",
        native,
        Box::new(move |cc| {
            let mut app = PrintCraftApp::new();
            if let Some(json) = cc.storage.and_then(|s| s.get_string("printcraft")) {
                app.restore(&json);
            }
            app.integrated_titlebar = integrated;
            // Autosave unsaved changes; offer to recover documents a crashed session left behind.
            if let Some(dir) = printcraft_ui_egui::RecoveryStore::default_dir() {
                app.enable_recovery(printcraft_ui_egui::RecoveryStore::new(dir));
            }
            for f in files {
                app.open_path(&f);
            }
            for (k, v) in options {
                if let Err(e) = app.set_option(&k, &v) {
                    eprintln!("printcraft: --{k} {v}: {e}");
                }
            }
            Ok(Box::new(app))
        }),
    )
}

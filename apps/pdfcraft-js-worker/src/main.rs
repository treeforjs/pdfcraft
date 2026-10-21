//! Single-request WASIp1 kernel. Build explicitly with `cargo xtask js-sandbox`.
use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(target_os = "wasi")]
    match pdfcraft_js::guest_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("script kernel: {e}");
            ExitCode::FAILURE
        }
    }
    #[cfg(not(target_os = "wasi"))]
    {
        eprintln!("This kernel must be built for wasm32-wasip1: cargo xtask js-sandbox");
        ExitCode::FAILURE
    }
}

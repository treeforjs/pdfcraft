use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=PDFCRAFT_JS_SANDBOX");
    if std::env::var("CARGO_CFG_TARGET_OS")?.as_str() == "wasi" {
        return Ok(());
    }
    let path = std::env::var_os("PDFCRAFT_JS_SANDBOX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/js-sandbox/pdfcraft-js-worker.wasm"));
    println!("cargo:rerun-if-changed={}", path.display());
    let metadata = std::fs::metadata(&path).map_err(|e| {
        format!(
            "script sandbox is missing ({}): {e}. Run `rustup target add wasm32-wasip1` and `cargo xtask js-sandbox` before building PdfCraft",
            path.display()
        )
    })?;
    if !metadata.is_file() || metadata.len() > 32 * 1024 * 1024 {
        return Err("script sandbox must be a regular Wasm file no larger than 32 MiB".into());
    }
    let output = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("Cargo did not provide OUT_DIR")?).join("script-kernel.wasm");
    std::fs::copy(path, output)?;
    Ok(())
}

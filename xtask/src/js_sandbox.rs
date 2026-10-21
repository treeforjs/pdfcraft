//! Explicit, nonrecursive bootstrap of the portable scripting kernel.
use std::path::Path;
use std::process::Command;

use anyhow::{Context, bail};

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let profile = match args {
        [] => "js-sandbox",
        [flag, profile] if flag == "--profile" && matches!(profile.as_str(), "dev" | "js-sandbox") => profile,
        _ => bail!("usage: cargo xtask js-sandbox [--profile dev|js-sandbox]"),
    };
    let root = crate::gates::root();
    let target = crate::gates::target_dir().join("js-kernel-build");
    let status = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .current_dir(&root)
        .env_remove("PDFCRAFT_JS_SANDBOX")
        .args(["rustc", "--locked", "-p", "pdfcraft-js-worker", "--target", "wasm32-wasip1", "--profile", profile, "--target-dir"])
        .arg(&target)
        // This reserves guest linear memory, never a native thread stack. Recursive parser
        // work still shares the 128 MiB aggregate linear-memory cap and interpreter fuel.
        .args(["--", "-C", "link-arg=-zstack-size=16777216"])
        .status()
        .context("start the script-kernel build (install its target with `rustup target add wasm32-wasip1`)")?;
    if !status.success() {
        bail!("script-kernel build failed: {status}; no host interpreter fallback is available");
    }
    let directory = if profile == "dev" { "debug" } else { profile };
    let source = target.join("wasm32-wasip1").join(directory).join("pdfcraft-js-worker.wasm");
    let destination = root.join("target/js-sandbox/pdfcraft-js-worker.wasm");
    install(&source, &destination)?;
    println!("Script kernel ready: {}", destination.display());
    Ok(())
}

fn install(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::metadata(source).context("read generated script-kernel metadata")?;
    if !metadata.is_file() || metadata.len() > 32 * 1024 * 1024 {
        bail!("generated script kernel is not a regular Wasm file within 32 MiB");
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).context("create ignored script-kernel output directory")?;
    }
    std::fs::copy(source, destination).context("install generated script kernel")?;
    Ok(())
}

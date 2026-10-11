//! Embeds the fonts of the optional craft-fonts build input (`CRAFT_FONTS_DIR`) as `CRAFT_FONTS`.
//! The recipe is craft-fonts' `docs/integration.md`; unset, `CRAFT_FONTS` is empty. It only reads
//! the local checkout (no network). On wasm32 it embeds only the UI faces (BIZ UDPGothic Regular
//! and any `Arab` or `Telu` face), to keep the web build within hosting limits (Cloudflare Pages:
//! 25 MiB per file).

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    if let Err(e) = standard_metrics() {
        println!("cargo::error=standard font metrics: {e}");
    }
    println!("cargo::rerun-if-env-changed=CRAFT_FONTS_DIR");
    println!("cargo::rerun-if-env-changed=CRAFT_FONTS_REQUIRED");
    let mut src = String::from("pub static CRAFT_FONTS: &[CraftFont] = &[\n");
    if let Some(dir) = std::env::var_os("CRAFT_FONTS_DIR").map(PathBuf::from) {
        match craft_fonts(&dir) {
            Ok(entries) => src.push_str(&entries),
            Err(e) if std::env::var_os("CRAFT_FONTS_REQUIRED").is_some() => {
                println!("cargo::error=CRAFT_FONTS_DIR={}: {e}", dir.display());
            }
            Err(e) => println!("cargo::warning=building without craft-fonts: CRAFT_FONTS_DIR={}: {e}", dir.display()),
        }
    }
    src.push_str("];\n");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default()).join("craft_fonts.rs");
    if let Err(e) = std::fs::write(&out, src) {
        println!("cargo::error=writing {}: {e}", out.display());
    }
}

/// Reuse the already attributed, permitted non-visual standard-font data, without importing
/// the renderer into the font layer or copying a second set of vendor assets into the tree.
fn standard_metrics() -> Result<(), String> {
    let source =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default()).join("../../vendor/hayro-interpret/src/font/generated/metrics.rs");
    println!("cargo::rerun-if-changed={}", source.display());
    let input = std::fs::read_to_string(&source).map_err(|e| e.to_string())?;
    let mut output = String::new();
    let mut tables = 0;
    for table in input.split("pub(crate) static ").skip(1) {
        let (name, body) = table.split_once(':').ok_or("invalid metrics table")?;
        let mut entries = Vec::new();
        for line in body.lines() {
            let line = line.trim();
            if !line.starts_with('"') {
                continue;
            }
            let (glyph, width) = line.split_once(" => ").ok_or("invalid glyph width")?;
            let width: u16 = width.trim_end_matches(',').trim_end_matches("_f32").parse().map_err(|e| format!("{e}"))?;
            entries.push((glyph, width));
        }
        entries.sort_unstable_by_key(|(glyph, _)| *glyph);
        if entries.is_empty() {
            return Err(format!("empty metrics table {name}"));
        }
        let _ = writeln!(output, "pub(crate) static {name}: &[(&str, u16)] = &[");
        for (glyph, width) in entries {
            let _ = writeln!(output, "({glyph}, {width}),");
        }
        output.push_str("];\n");
        tables += 1;
    }
    if tables != 14 {
        return Err(format!("expected 14 metrics tables, found {tables}"));
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default()).join("standard_metrics.rs");
    std::fs::write(out, output).map_err(|e| e.to_string())
}

/// One `CraftFont { .. }` initialiser per manifest line.
fn craft_fonts(dir: &std::path::Path) -> Result<String, String> {
    let manifest = dir.join("fonts/manifest.txt");
    println!("cargo::rerun-if-changed={}", manifest.display());
    let text = std::fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let wasm = std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|a| a == "wasm32");
    let mut out = String::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let f: Vec<&str> = line.split(" | ").map(str::trim).collect();
        let [family, style, file, scripts, ..] = f.as_slice() else {
            return Err(format!("malformed manifest line: {line}"));
        };
        // Arabic interface faces are small, so the web build keeps them too.
        let arabic = scripts.split(',').any(|s| s.trim() == "Arab");
        // Telugu faces too, for the Telugu interface.
        let telugu = scripts.split(',').any(|s| s.trim() == "Telu");
        let web_face = (*family == "BIZ UDPGothic" && *style == "Regular") || arabic || telugu;
        if wasm && !web_face {
            continue;
        }
        let path = dir.join(file).canonicalize().map_err(|e| format!("{file}: {e}"))?;
        println!("cargo::rerun-if-changed={}", path.display());
        let scripts: Vec<String> = scripts.split(',').map(|s| format!("{:?}", s.trim())).collect();
        let _ = writeln!(
            out,
            "    CraftFont {{ family: {family:?}, style: {style:?}, scripts: &[{}], bytes: include_bytes!({:?}) }},",
            scripts.join(", "),
            path.display().to_string(),
        );
    }
    Ok(out)
}

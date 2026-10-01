//! printcraft-cli — headless PrintCraft.
//!
//! ```text
//! printcraft-cli info   <file.pdf> [--password PW]            document summary as JSON
//! printcraft-cli render <file.pdf> --page N [--dpi 96] --out x.pam
//! printcraft-cli text   <file.pdf> [--page N]                  extracted text (pages separated by form feeds)
//! printcraft-cli edit   <in.pdf> --out out.pdf [--rotate 1,3:90] [--delete 2,4] [--move 5:1]
//!                       [--insert-blank 1] [--title T] [--author A] [--full]
//! printcraft-cli combine <a.pdf> <b.pdf> … --out combined.pdf
//! printcraft-cli extract <in.pdf> --pages 1,3,5 --out out.pdf
//! printcraft-cli split   <in.pdf> (--every N | --before 3,7) [--out-dir DIR]
//! printcraft-cli check  <files or dirs…> [--timeout 20] [--dpi 36] [--json out.json]
//! ```
//!
//! `check` is the robustness harness: every file is opened, inspected and fully rendered in a
//! *separate child process* with a wall-clock timeout, so hangs, panics and aborts in any
//! dependency are observed and reported instead of taking the harness down.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use printcraft_render::{PageRenderer, RenderConfig, RenderRequest, RequestKind, inspect};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("info") => info(&args[1..]),
        Some("render") => render(&args[1..]),
        Some("text") => text(&args[1..]),
        Some("edit") => edit(&args[1..]),
        Some("combine") => combine(&args[1..]),
        Some("extract") => extract(&args[1..]),
        Some("split") => split(&args[1..]),
        Some("check") => check(&args[1..]),
        Some("check-one") => check_one(&args[1..]),
        Some("--version") => {
            println!("printcraft-cli {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => Err("usage: printcraft-cli <info|render|text|edit|combine|extract|split|check> …  (see source header for options)".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("printcraft-cli: {e}");
            ExitCode::FAILURE
        }
    }
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn positional(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        if a.starts_with("--") {
            skip = true;
            continue;
        }
        out.push(a.as_str());
    }
    out
}

fn read(path: &str) -> Result<Arc<Vec<u8>>, String> {
    std::fs::read(path).map(Arc::new).map_err(|e| format!("{path}: {e}"))
}

fn info(args: &[String]) -> Result<(), String> {
    let path = *positional(args).first().ok_or("info: missing file")?;
    let info = inspect(read(path)?, flag(args, "--password")).map_err(|e| e.to_string())?;
    let json = serde_json::json!({
        "file": path,
        "pdf_version": info.pdf_version,
        "file_size": info.file_size,
        "pages": info.pages.len(),
        "first_page_pt": info.pages.first().map(|p| [p.width, p.height]),
        "title": info.title, "author": info.author, "producer": info.producer, "creator": info.creator,
        "encrypted": info.encrypted, "tagged": info.tagged, "javascript": info.has_javascript,
        "bookmarks": info.outline.len(), "annotations": info.annotations.len(), "fields": info.fields.len(),
        "links": info.links.len(), "layers": info.layers.len(), "attachments": info.attachments.len(),
        "page_labels": info.pages.iter().take(8).map(|p| p.label.clone()).collect::<Vec<_>>(),
        "warnings": info.warnings,
    });
    println!("{}", serde_json::to_string_pretty(&json).unwrap_or_default());
    Ok(())
}

fn text(args: &[String]) -> Result<(), String> {
    let path = *positional(args).first().ok_or("text: missing file")?;
    let mut r = PageRenderer::new(read(path)?, RenderConfig { password: flag(args, "--password").map(Arc::from), ..Default::default() });
    let pages: Vec<usize> = match flag(args, "--page") {
        Some(p) => vec![p.parse::<usize>().map_err(|_| "bad --page")?.saturating_sub(1)],
        None => (0..r.page_count()).collect(),
    };
    for (n, p) in pages.iter().enumerate() {
        let out = r.render(RenderRequest { page: *p, kind: RequestKind::Text, tile: None, scale: 1.0, tag: 0 });
        if let Some(e) = out.error {
            eprintln!("page {}: {e}", p + 1);
            continue;
        }
        if n > 0 {
            println!("\u{c}");
        }
        println!("{}", out.text.map(|t| t.plain_text()).unwrap_or_default());
    }
    Ok(())
}

/// 1-based page list ("1,3,5") → 0-based indices.
fn page_list(s: &str) -> Result<Vec<usize>, String> {
    s.split(',').map(|p| p.trim().parse::<usize>().ok().filter(|n| *n > 0).map(|n| n - 1).ok_or(format!("bad page number {p:?}"))).collect()
}

/// Apply page and metadata edits through the engine and save (incrementally unless `--full`).
fn edit(args: &[String]) -> Result<(), String> {
    use printcraft_engine::{Edit, Session};
    let path = *positional(args).first().ok_or("edit: missing file")?;
    let out = flag(args, "--out").ok_or("edit: missing --out")?;
    let mut session = Session::new();
    let id = session.open(path, Some(path.to_string()), read(path)?, flag(args, "--password")).map_err(|e| e.to_string())?;
    let mut edits = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).map(String::as_str).unwrap_or("");
        match args[i].as_str() {
            "--rotate" => {
                let (pages, deg) = value.split_once(':').ok_or("--rotate PAGES:DEGREES")?;
                edits.push(Edit::RotatePages { pages: page_list(pages)?, degrees: deg.parse().map_err(|_| "bad degrees")? });
            }
            "--delete" => edits.push(Edit::DeletePages { pages: page_list(value)? }),
            "--move" => {
                let (pages, to) = value.split_once(':').ok_or("--move PAGES:TO")?;
                let to: usize = to.parse().map_err(|_| "bad target")?;
                edits.push(Edit::MovePages { pages: page_list(pages)?, to: to.saturating_sub(1) });
            }
            "--insert-blank" => {
                let at: usize = value.parse().map_err(|_| "bad position")?;
                edits.push(Edit::InsertBlankPage { at: at.saturating_sub(1), width: 612.0, height: 792.0 });
            }
            "--title" => edits.push(Edit::SetInfo { key: "Title".into(), value: value.into() }),
            "--author" => edits.push(Edit::SetInfo { key: "Author".into(), value: value.into() }),
            _ => {}
        }
        i += 1;
    }
    for e in edits {
        session.apply(id, e).map_err(|e| e.to_string())?;
    }
    let bytes = if args.iter().any(|a| a == "--full") { session.save_full_bytes(id) } else { session.save_bytes(id) }.map_err(|e| e.to_string())?;
    std::fs::write(out, bytes.as_slice()).map_err(|e| format!("{out}: {e}"))
}

fn file_stem(path: &str) -> String {
    Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

fn combine(args: &[String]) -> Result<(), String> {
    let out = flag(args, "--out").ok_or("combine: missing --out")?;
    let inputs = positional(args);
    if inputs.len() < 2 {
        return Err("combine: give at least two input files".into());
    }
    let sources = inputs.iter().map(|p| Ok((file_stem(p), read(p)?))).collect::<Result<Vec<_>, String>>()?;
    let bytes = printcraft_engine::Session::new().combine(&sources).map_err(|e| e.to_string())?;
    std::fs::write(out, bytes.as_slice()).map_err(|e| format!("{out}: {e}"))
}

fn extract(args: &[String]) -> Result<(), String> {
    let path = *positional(args).first().ok_or("extract: missing file")?;
    let out = flag(args, "--out").ok_or("extract: missing --out")?;
    let pages = page_list(flag(args, "--pages").ok_or("extract: missing --pages")?)?;
    let mut session = printcraft_engine::Session::new();
    let id = session.open(path, None, read(path)?, flag(args, "--password")).map_err(|e| e.to_string())?;
    let bytes = session.extract(id, &pages).map_err(|e| e.to_string())?;
    std::fs::write(out, bytes.as_slice()).map_err(|e| format!("{out}: {e}"))
}

fn split(args: &[String]) -> Result<(), String> {
    use printcraft_engine::SplitBy;
    let path = *positional(args).first().ok_or("split: missing file")?;
    let by = match (flag(args, "--every"), flag(args, "--before")) {
        (Some(n), None) => SplitBy::PageCount(n.parse().map_err(|_| "bad --every")?),
        (None, Some(list)) => SplitBy::Before(page_list(list)?),
        _ => return Err("split: give either --every N or --before PAGES".into()),
    };
    let dir = PathBuf::from(flag(args, "--out-dir").unwrap_or("."));
    let mut session = printcraft_engine::Session::new();
    let id = session.open(path, None, read(path)?, flag(args, "--password")).map_err(|e| e.to_string())?;
    let stem = file_stem(path);
    for (a, b, bytes) in session.split(id, &by).map_err(|e| e.to_string())? {
        let name = dir.join(if a == b { format!("{stem}-p{a}.pdf") } else { format!("{stem}-p{a}-{b}.pdf") });
        std::fs::write(&name, bytes.as_slice()).map_err(|e| format!("{}: {e}", name.display()))?;
        println!("{}", name.display());
    }
    Ok(())
}

fn render(args: &[String]) -> Result<(), String> {
    let path = *positional(args).first().ok_or("render: missing file")?;
    let page: usize = flag(args, "--page").unwrap_or("1").parse().map_err(|_| "bad --page")?;
    let dpi: f32 = flag(args, "--dpi").unwrap_or("96").parse().map_err(|_| "bad --dpi")?;
    let out = flag(args, "--out").ok_or("render: missing --out (.pam)")?;
    let mut r = PageRenderer::new(read(path)?, RenderConfig { password: flag(args, "--password").map(Arc::from), ..Default::default() });
    let p = r.render(RenderRequest { page: page.saturating_sub(1), kind: RequestKind::Pixels, tile: None, scale: dpi / 72.0, tag: 0 });
    if let Some(e) = p.error {
        return Err(e);
    }
    // PAM (netpbm RGB_ALPHA) keeps this tool dependency-free; convert with any image tool.
    let mut f = std::fs::File::create(out).map_err(|e| e.to_string())?;
    write!(f, "P7\nWIDTH {}\nHEIGHT {}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n", p.width, p.height).map_err(|e| e.to_string())?;
    f.write_all(&p.rgba).map_err(|e| e.to_string())?;
    eprintln!("rendered page {page} at {dpi} dpi: {}×{} px in {} ms", p.width, p.height, p.millis);
    Ok(())
}

/// Child-process body for `check`: prints one JSON line.
fn check_one(args: &[String]) -> Result<(), String> {
    let path = *positional(args).first().ok_or("check-one: missing file")?;
    let dpi: f32 = flag(args, "--dpi").unwrap_or("36").parse().map_err(|_| "bad --dpi")?;
    let start = Instant::now();
    let bytes = read(path)?;
    let (status, pages, failed, detail, warnings) = match inspect(bytes.clone(), None) {
        Err(e) => (format!("open-error:{}", short(&e.to_string())), 0, 0, e.to_string(), 0),
        Ok(info) => {
            let mut r = PageRenderer::new(bytes, RenderConfig::default());
            let mut failed = Vec::new();
            for i in 0..info.pages.len().min(500) {
                let p = r.render(RenderRequest { page: i, kind: RequestKind::Pixels, tile: None, scale: dpi / 72.0, tag: 0 });
                if let Some(e) = r.render(RenderRequest { page: i, kind: RequestKind::Text, tile: None, scale: 1.0, tag: 0 }).error {
                    failed.push(format!("p{} text: {e}", i + 1));
                }
                if let Some(e) = p.error {
                    failed.push(format!("p{}: {e}", i + 1));
                }
            }
            let status = if failed.is_empty() { "ok".to_string() } else { "page-errors".to_string() };
            (status, info.pages.len(), failed.len(), failed.join(" | "), info.warnings.len())
        }
    };
    let line = serde_json::json!({ "file": path, "status": status, "pages": pages, "failed_pages": failed, "warnings": warnings,
        "ms": start.elapsed().as_millis() as u64, "detail": detail.chars().take(400).collect::<String>() });
    println!("{line}");
    Ok(())
}

fn short(s: &str) -> String {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).take(4).collect::<Vec<_>>().join("-").to_lowercase()
}

fn collect(paths: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                stack.extend(rd.flatten().map(|e| e.path()));
            }
        } else if p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
            out.push(p);
        }
    }
    out.sort();
    out
}

fn check(args: &[String]) -> Result<(), String> {
    let files = collect(&positional(args));
    if files.is_empty() {
        return Err("check: no PDF files found".into());
    }
    let timeout = Duration::from_secs(flag(args, "--timeout").unwrap_or("20").parse().map_err(|_| "bad --timeout")?);
    let dpi = flag(args, "--dpi").unwrap_or("36").to_string();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut results = Vec::new();
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for (n, f) in files.iter().enumerate() {
        let r = run_child(&exe, f, &dpi, timeout);
        let status = r["status"].as_str().unwrap_or("?").to_string();
        *counts.entry(status.split(':').next().unwrap_or("?").to_string()).or_default() += 1;
        if status != "ok" {
            eprintln!(
                "[{}/{}] {status:<28} {}  {}",
                n + 1,
                files.len(),
                f.display(),
                r["detail"].as_str().unwrap_or("").chars().take(160).collect::<String>()
            );
        }
        results.push(r);
    }
    eprintln!("\nchecked {} files: {:?}", files.len(), counts);
    if let Some(out) = flag(args, "--json") {
        std::fs::write(out, serde_json::to_string_pretty(&results).unwrap_or_default()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn run_child(exe: &Path, file: &Path, dpi: &str, timeout: Duration) -> serde_json::Value {
    let file_s = file.to_string_lossy().to_string();
    let child = Command::new(exe).args(["check-one", &file_s, "--dpi", dpi]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return serde_json::json!({ "file": file_s, "status": "harness-error", "detail": e.to_string() }),
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut so) = child.stdout.take() {
                    use std::io::Read;
                    let _ = so.read_to_string(&mut out);
                }
                return match serde_json::from_str::<serde_json::Value>(out.trim()) {
                    Ok(v) => v,
                    Err(_) => serde_json::json!({ "file": file_s, "status": "crash", "detail": format!("child exited with {status}") }),
                };
            }
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return serde_json::json!({ "file": file_s, "status": "timeout", "detail": format!("exceeded {}s", timeout.as_secs()) });
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return serde_json::json!({ "file": file_s, "status": "harness-error", "detail": e.to_string() }),
        }
    }
}

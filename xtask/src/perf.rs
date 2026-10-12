//! `cargo xtask perf`: measured performance and memory budgets on synthetic documents
//! (parity `misc.performance-budgets`, `view.large-document-performance`; docs/performance.md).
//!
//! 1. Generates original, deterministic synthetic PDFs (no corpus files, no assets) under
//!    `<target>/perf/`: a 500-page and a 5 000-page text document, an image-heavy document and a
//!    single page with 100 000 vector shapes.
//! 2. Builds two probes in the chosen profile (dev by default; `--release` for release):
//!    `pdfcraft-automation`'s `perf_probe` example (the tool table behind `pdfcraft-cli run` and
//!    MCP: open, first/last page render, all-text extraction, find, incremental and full save) and
//!    `pdfcraft-ui-egui`'s `perf_ui` example (the real shell under egui_kittest: open, first frame,
//!    time to a settled first page, scroll frame times, navigation latency).
//! 3. Runs each probe `--warmup` + `--runs` times per document, each in a fresh child process on a
//!    fresh copy of the file, so peak RSS (`VmHWM`, read by the child itself) belongs to that
//!    run alone. Warm-up runs are discarded; the report keeps the median, min, max and spread.
//! 4. Compares the medians with the budgets in `xtask/baselines/perf.json` for the profile and
//!    fails on any median over budget, unless `--report-only`. Budgets are deliberately generous
//!    (several times the measured median on a shared machine): they catch gross regressions, not
//!    small drifts.
//!
//! The report goes to `<target>/perf/perf-report.json`. `pdftotext`/`pdftoppm` timings on the
//! same files are recorded as an informational reference when Poppler is installed.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use serde_json::{Value, json};

use crate::gates::{root, target_dir};

const USAGE: &str = "cargo xtask perf [--runs N (3)] [--warmup N (1)] [--release] [--report-only] [--only a,b] [--no-ui] [--no-oracle] [--keep]";

/// One synthetic document and how to drive it.
pub struct Scenario {
    pub name: &'static str,
    pub about: &'static str,
    pub pages: usize,
    /// A phrase that occurs once, on the last page (so `find` scans the whole document).
    pub phrase: &'static str,
    pub build: fn() -> Vec<u8>,
}

pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "text-500",
        about: "500 Letter pages of 40 Helvetica lines each, one Square comment per page (the M3 500-page document)",
        pages: 500,
        phrase: PHRASE,
        build: || text_doc(500),
    },
    Scenario {
        name: "text-5000",
        about: "5 000 text pages of the same kind: how open, extraction, search and saves scale",
        pages: 5000,
        phrase: PHRASE,
        build: || text_doc(5000),
    },
    Scenario {
        name: "images-24",
        about: "24 pages, each a full-page 1275×1650 RGB image (150 dpi Letter, Flate; ~151 MB of decoded pixels)",
        pages: 24,
        phrase: PHRASE,
        build: || image_doc(24, 1275, 1650),
    },
    Scenario {
        name: "vector-1",
        about: "one page with 100 000 filled polygons and 10 000 stroked curves (Flate content stream)",
        pages: 1,
        phrase: PHRASE,
        build: || vector_doc(100_000),
    },
];

/// Occurs exactly once per generated document, on its last page.
const PHRASE: &str = "quartz zebra final marker";

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let opts = Options::parse(args)?;
    let profile = if opts.release { "release" } else { "dev" };
    let work = target_dir().join("perf");
    std::fs::create_dir_all(&work).with_context(|| format!("create {}", work.display()))?;

    let scenarios: Vec<&Scenario> = SCENARIOS.iter().filter(|s| opts.only.as_ref().is_none_or(|o| o.iter().any(|n| n == s.name))).collect();
    if scenarios.is_empty() {
        bail!("--only matched no scenario (have: {})", SCENARIOS.iter().map(|s| s.name).collect::<Vec<_>>().join(", "));
    }

    let engine_probe = build_example("pdfcraft-automation", "perf_probe", opts.release)?;
    let ui_probe = if opts.ui { Some(build_example("pdfcraft-ui-egui", "perf_ui", opts.release)?) } else { None };

    let budgets = load_budgets()?;
    let profile_budgets = budgets.get("profiles").and_then(|p| p.get(profile)).cloned().unwrap_or(Value::Null);
    let machine = machine_info(profile);
    eprintln!(
        "perf: profile {profile}, {} measured + {} warm-up runs per probe; {}",
        opts.runs,
        opts.warmup,
        machine["summary"].as_str().unwrap_or("")
    );

    let mut results = serde_json::Map::new();
    let mut failures = Vec::new();
    for s in scenarios {
        let t = Instant::now();
        let bytes = (s.build)();
        let file = work.join(format!("{}.pdf", s.name));
        std::fs::write(&file, &bytes).with_context(|| format!("write {}", file.display()))?;
        eprintln!(
            "perf: {} — {} ({} pages, {:.1} MB, generated in {:.1} s)",
            s.name,
            s.about,
            s.pages,
            bytes.len() as f64 / 1e6,
            t.elapsed().as_secs_f64()
        );
        let load_before = load_average();

        let mut samples: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        let mut errors = Vec::new();
        let scratch = work.join(format!("{}.run.pdf", s.name));
        for i in 0..opts.warmup + opts.runs {
            let measured = i >= opts.warmup;
            std::fs::copy(&file, &scratch).with_context(|| format!("copy to {}", scratch.display()))?;
            let mut cmd = Command::new(&engine_probe);
            cmd.arg(&scratch).arg(s.phrase);
            match run_probe(cmd, Duration::from_secs(900)) {
                Ok(v) if measured => add_samples(&mut samples, "engine", &v),
                Ok(_) => {}
                Err(e) => errors.push(format!("engine run {}: {e:#}", i + 1)),
            }
            let _ = std::fs::remove_file(&scratch);
            let _ = std::fs::remove_file(work.join(format!("{}.run.pdf.full.pdf", s.name)));
            if let Some(ui) = &ui_probe {
                let mut cmd = Command::new(ui);
                cmd.arg(&file).arg(s.pages.to_string());
                match run_probe(cmd, Duration::from_secs(900)) {
                    Ok(v) if measured => {
                        if v["all_settled"] != json!(true) {
                            errors.push(format!("ui run {}: the view did not settle within 120 s", i + 1));
                        }
                        if v["first_rasters"].as_u64().unwrap_or(0) == 0 {
                            errors.push(format!("ui run {}: settled with no page raster on screen", i + 1));
                        }
                        add_samples(&mut samples, "ui", &v)
                    }
                    Ok(_) => {}
                    Err(e) => errors.push(format!("ui run {}: {e:#}", i + 1)),
                }
            }
            if opts.oracle && measured {
                oracle(&file, &work, &mut samples);
            }
            eprint!(".");
            let _ = std::io::stderr().flush();
        }
        eprintln!();

        let budget = profile_budgets.get(s.name).cloned().unwrap_or(Value::Null);
        let mut metrics = serde_json::Map::new();
        for (key, values) in &samples {
            let st = stats(values);
            let limit = budget.get(key).and_then(Value::as_f64);
            let status = match limit {
                Some(b) if st.median > b => {
                    failures.push(format!("{} {key}: median {:.1} > budget {b}", s.name, st.median));
                    "over"
                }
                Some(_) => "ok",
                None => "-",
            };
            metrics.insert(key.clone(), json!({ "median": st.median, "min": st.min, "max": st.max, "spread_pct": st.spread_pct, "n": values.len(), "budget": limit, "status": status }));
        }
        if let Some(b) = budget.as_object() {
            for key in b.keys().filter(|k| !k.starts_with('_') && !samples.contains_key(*k)) {
                if !(key.starts_with("ui.") && !opts.ui || key.starts_with("oracle.")) {
                    errors.push(format!("budgeted metric {key} was not measured"));
                }
            }
        }
        failures.extend(errors.iter().map(|e| format!("{}: {e}", s.name)));
        print_table(s.name, &metrics);
        results.insert(
            s.name.into(),
            json!({
                "about": s.about,
                "pages": s.pages,
                "file_bytes": bytes.len(),
                "sha256": sha256_hex(&bytes),
                "load_average_before": load_before,
                "load_average_after": load_average(),
                "errors": errors,
                "metrics": metrics,
            }),
        );
        if !opts.keep {
            let _ = std::fs::remove_file(&file);
        }
    }

    let report = json!({
        "schema": 1,
        "profile": profile,
        "runs": opts.runs,
        "warmup": opts.warmup,
        "machine": machine,
        "scenarios": results,
        "failures": failures,
    });
    let out = work.join("perf-report.json");
    std::fs::write(&out, serde_json::to_vec_pretty(&report)?).with_context(|| format!("write {}", out.display()))?;
    eprintln!("perf: report written to {}", out.display());
    if failures.is_empty() {
        eprintln!("perf: every measured median is within its {profile} budget");
        Ok(())
    } else if opts.report_only {
        eprintln!("perf: {} budget problem(s), not failing (--report-only):\n  {}", failures.len(), failures.join("\n  "));
        Ok(())
    } else {
        bail!("{} budget problem(s):\n  {}", failures.len(), failures.join("\n  "))
    }
}

struct Options {
    runs: usize,
    warmup: usize,
    release: bool,
    report_only: bool,
    only: Option<Vec<String>>,
    ui: bool,
    oracle: bool,
    /// Keep the generated documents in `<target>/perf/` for manual probing.
    keep: bool,
}

impl Options {
    fn parse(args: &[String]) -> anyhow::Result<Self> {
        let mut o = Options { runs: 3, warmup: 1, release: false, report_only: false, only: None, ui: true, oracle: true, keep: false };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut value = |name: &str| it.next().cloned().ok_or_else(|| anyhow!("{name} needs a value; usage: {USAGE}"));
            match a.as_str() {
                "--runs" => o.runs = value("--runs")?.parse().context("--runs")?,
                "--warmup" => o.warmup = value("--warmup")?.parse().context("--warmup")?,
                "--only" => o.only = Some(value("--only")?.split(',').map(str::to_string).collect()),
                "--release" => o.release = true,
                "--report-only" => o.report_only = true,
                "--no-ui" => o.ui = false,
                "--no-oracle" => o.oracle = false,
                "--keep" => o.keep = true,
                other => bail!("unknown option {other:?}; usage: {USAGE}"),
            }
        }
        if o.runs == 0 {
            bail!("--runs must be at least 1");
        }
        Ok(o)
    }
}

/// `cargo build` one example and return its executable.
fn build_example(package: &str, example: &str, release: bool) -> anyhow::Result<PathBuf> {
    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.current_dir(root()).args(["build", "-p", package, "--example", example, "--message-format=json-render-diagnostics"]);
    if release {
        cmd.arg("--release");
    }
    eprintln!("$ cargo build -p {package} --example {example}{}", if release { " --release" } else { "" });
    let out = cmd.stderr(Stdio::inherit()).output().with_context(|| format!("build {example}"))?;
    if !out.status.success() {
        bail!("building {package} example {example} failed ({})", out.status);
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == example)
        .find_map(|m| m["executable"].as_str().map(PathBuf::from))
        .ok_or_else(|| anyhow!("cargo did not report an executable for example {example}"))
}

/// Run a probe and parse the JSON object it prints last.
fn run_probe(mut cmd: Command, timeout: Duration) -> anyhow::Result<Value> {
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().context("spawn probe")?;
    let mut stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
    let mut stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;
    let read_out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut s);
        s
    });
    let read_err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr, &mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let out = read_out.join().unwrap_or_default();
    let err = read_err.join().unwrap_or_default();
    if !status.success() {
        bail!("exited with {status}: {}", err.lines().rev().take(5).collect::<Vec<_>>().join(" | "));
    }
    let line = out.lines().rev().find(|l| l.starts_with('{')).ok_or_else(|| anyhow!("no JSON output"))?;
    serde_json::from_str(line).context("probe output")
}

/// Flatten a probe report into `prefix.metric` samples: every number whose key ends in `_ms`,
/// `peak_rss_kib` (converted to MiB), `over_16_7ms`, a phase's `ms` (as `<phase>_ms`), plus the median and worst of number arrays.
pub fn add_samples(samples: &mut BTreeMap<String, Vec<f64>>, prefix: &str, v: &Value) {
    let Some(obj) = v.as_object() else { return };
    for (k, val) in obj {
        let key = format!("{prefix}.{k}");
        match val {
            Value::Number(n) => {
                let Some(x) = n.as_f64() else { continue };
                if k == "ms" {
                    samples.entry(format!("{prefix}_ms")).or_default().push(x);
                } else if let Some(base) = key.strip_suffix("peak_rss_kib") {
                    samples.entry(format!("{base}peak_rss_mib")).or_default().push(x / 1024.0);
                } else if k.ends_with("_ms") || k == "over_16_7ms" {
                    samples.entry(key).or_default().push(x);
                }
            }
            Value::Array(items) if k.ends_with("_ms") => {
                let xs: Vec<f64> = items.iter().filter_map(Value::as_f64).collect();
                if !xs.is_empty() {
                    let st = stats(&xs);
                    let base = key.trim_end_matches("_ms");
                    samples.entry(format!("{base}.median_ms")).or_default().push(st.median);
                    samples.entry(format!("{base}.max_ms")).or_default().push(st.max);
                }
            }
            Value::Object(_) if k == "phases" => add_samples(samples, prefix, val),
            Value::Object(_) if k != "raster_memory" => add_samples(samples, &key, val),
            _ => {}
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Stats {
    pub median: f64,
    pub min: f64,
    pub max: f64,
    /// (max − min) / median, in percent: how noisy this machine was for the metric.
    pub spread_pct: f64,
}

pub fn stats(values: &[f64]) -> Stats {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        return Stats { median: 0.0, min: 0.0, max: 0.0, spread_pct: 0.0 };
    }
    let median = if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 };
    let (min, max) = (v[0], v[n - 1]);
    let spread_pct = if median > 0.0 { (max - min) / median * 100.0 } else { 0.0 };
    Stats { median, min, max, spread_pct }
}

fn print_table(name: &str, metrics: &serde_json::Map<String, Value>) {
    println!("\n{name}");
    println!("  {:<44} {:>10} {:>10} {:>10} {:>8} {:>10}  status", "metric", "median", "min", "max", "spread", "budget");
    for (k, m) in metrics {
        let f = |key: &str| m[key].as_f64().unwrap_or(0.0);
        let budget = m["budget"].as_f64().map_or_else(|| "-".to_string(), |b| format!("{b}"));
        println!(
            "  {k:<44} {:>10.1} {:>10.1} {:>10.1} {:>7.0}% {budget:>10}  {}",
            f("median"),
            f("min"),
            f("max"),
            f("spread_pct"),
            m["status"].as_str().unwrap_or("")
        );
    }
}

fn load_budgets() -> anyhow::Result<Value> {
    let path = root().join("xtask/baselines/perf.json");
    let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

/// Poppler's timings on the same file, for reference only (never budgeted).
fn oracle(file: &Path, work: &Path, samples: &mut BTreeMap<String, Vec<f64>>) {
    let time = |cmd: &mut Command| -> Option<f64> {
        let t = Instant::now();
        let ok = cmd.stdout(Stdio::null()).stderr(Stdio::null()).status().ok()?.success();
        ok.then(|| t.elapsed().as_secs_f64() * 1000.0)
    };
    if let Some(ms) = time(Command::new("pdftotext").arg("-q").arg(file).arg("-")) {
        samples.entry("oracle.pdftotext_all_ms".into()).or_default().push(ms);
    }
    let prefix = work.join("oracle");
    if let Some(ms) = time(Command::new("pdftoppm").args(["-q", "-r", "96", "-f", "1", "-l", "1", "-png"]).arg(file).arg(&prefix)) {
        samples.entry("oracle.pdftoppm_first_page_ms".into()).or_default().push(ms);
    }
    if let Ok(dir) = std::fs::read_dir(work) {
        for e in dir.flatten() {
            if e.file_name().to_string_lossy().starts_with("oracle") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn load_average() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg").ok()?.split_whitespace().next()?.parse().ok()
}

fn machine_info(profile: &str) -> Value {
    let cores = std::thread::available_parallelism().map_or(0, |n| n.get());
    let read = |p: &str| std::fs::read_to_string(p).ok().map(|s| s.trim().to_string());
    let cpu = read("/proc/cpuinfo")
        .and_then(|s| s.lines().find(|l| l.starts_with("model name")).and_then(|l| l.split(':').nth(1)).map(|m| m.trim().to_string()));
    let mem_kib = read("/proc/meminfo").and_then(|s| s.lines().next().and_then(|l| l.split_whitespace().nth(1)).and_then(|n| n.parse::<u64>().ok()));
    let git = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(root())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let unix_time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    json!({
        "summary": format!("{cores} cores, {} GiB, load {:?}, {}", mem_kib.unwrap_or(0) / (1024 * 1024), load_average(), cpu.clone().unwrap_or_default()),
        "os": std::env::consts::OS,
        "kernel": read("/proc/sys/kernel/osrelease"),
        "cpu": cpu,
        "cores": cores,
        "mem_kib": mem_kib,
        "git": git,
        "profile": profile,
        "unix_time": unix_time,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

// ---- Synthetic documents ------------------------------------------------------------------------

/// A minimal PDF writer: objects are numbered from 1 in push order.
#[derive(Default)]
struct Pdf {
    objects: Vec<Vec<u8>>,
}

impl Pdf {
    fn push(&mut self, body: impl Into<Vec<u8>>) -> usize {
        self.objects.push(body.into());
        self.objects.len()
    }

    /// Reserve an object number to fill later (forward references).
    fn reserve(&mut self) -> usize {
        self.push(Vec::new())
    }

    fn set(&mut self, id: usize, body: impl Into<Vec<u8>>) {
        if let Some(slot) = self.objects.get_mut(id - 1) {
            *slot = body.into();
        }
    }

    fn stream(dict: &str, data: &[u8], flate: bool) -> Vec<u8> {
        let data = if flate { deflate(data) } else { data.to_vec() };
        let filter = if flate { " /Filter /FlateDecode" } else { "" };
        let mut out = format!("<< {dict} /Length {}{filter} >>\nstream\n", data.len()).into_bytes();
        out.extend_from_slice(&data);
        out.extend_from_slice(b"\nendstream");
        out
    }

    fn finish(self, root: usize) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = Vec::with_capacity(self.objects.len());
        for (i, o) in self.objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(o);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", self.objects.len() + 1).as_bytes());
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root {root} 0 R >>\nstartxref\n{xref}\n%%EOF\n", self.objects.len() + 1).as_bytes());
        out
    }
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    // Writing to a Vec cannot fail.
    let _ = e.write_all(data);
    e.finish().unwrap_or_default()
}

/// Catalog + page tree around `pages` page objects built by `page(pdf, index, parent)`.
fn document(pages: usize, mut page: impl FnMut(&mut Pdf, usize, usize) -> usize) -> Vec<u8> {
    let mut pdf = Pdf::default();
    let catalog = pdf.reserve();
    let tree = pdf.reserve();
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", page(&mut pdf, i, tree))).collect();
    pdf.set(catalog, format!("<< /Type /Catalog /Pages {tree} 0 R >>"));
    pdf.set(tree, format!("<< /Type /Pages /Kids [{}] /Count {pages} /MediaBox [0 0 612 792] >>", kids.join(" ")));
    pdf.finish(catalog)
}

fn last_page_marker(i: usize, pages: usize) -> String {
    if i + 1 == pages { format!("BT /F1 12 Tf 72 40 Td ({PHRASE}) Tj ET\n") } else { String::new() }
}

/// `n` Letter pages of 40 Helvetica lines with one Square comment each (the same shape as the
/// UI test `ui-egui/tests/perf.rs`), uncompressed content as many simple producers write it.
pub fn text_doc(n: usize) -> Vec<u8> {
    let mut font = 0;
    document(n, |pdf, i, parent| {
        if font == 0 {
            font = pdf.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
        }
        let mut body: String =
            (0..40).map(|l| format!("BT /F1 10 Tf 72 {} Td (Page {} line {l} with some words to lay out) Tj ET\n", 720 - l * 15, i + 1)).collect();
        body.push_str(&last_page_marker(i, n));
        let contents = pdf.push(Pdf::stream("", body.as_bytes(), false));
        let annot = pdf.push("<< /Type /Annot /Subtype /Square /Rect [72 72 144 144] /C [1 0 0] /T (Ada) /Contents (Check) >>");
        pdf.push(format!(
            "<< /Type /Page /Parent {parent} 0 R /Contents {contents} 0 R /Resources << /Font << /F1 {font} 0 R >> >> /Annots [{annot} 0 R] >>"
        ))
    })
}

/// A deterministic pseudo-random sequence (64-bit LCG, Knuth's MMIX constants).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
}

/// `n` pages, each one full-page `w`×`h` RGB image: smooth gradients with a little noise, so it
/// compresses like a photo-ish scan rather than to nothing.
pub fn image_doc(n: usize, w: usize, h: usize) -> Vec<u8> {
    let mut font = 0;
    let mut rng = Lcg(0x5eed);
    document(n, |pdf, i, parent| {
        if font == 0 {
            font = pdf.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        }
        let mut px = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let noise = (rng.next() & 0x0f) as usize;
                px.push(((x * 255 / w.max(1) + i * 37 + noise) & 0xff) as u8);
                px.push(((y * 255 / h.max(1) + i * 11 + noise) & 0xff) as u8);
                px.push((((x + y) * 128 / (w + h).max(1) + 64 + noise) & 0xff) as u8);
            }
        }
        let image = pdf.push(Pdf::stream(
            &format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8"),
            &px,
            true,
        ));
        let body = format!("q 612 0 0 792 0 0 cm /Im1 Do Q\nBT /F1 12 Tf 72 760 Td (Image page {}) Tj ET\n{}", i + 1, last_page_marker(i, n));
        let contents = pdf.push(Pdf::stream("", body.as_bytes(), false));
        pdf.push(format!("<< /Type /Page /Parent {parent} 0 R /Contents {contents} 0 R /Resources << /Font << /F1 {font} 0 R >> /XObject << /Im1 {image} 0 R >> >> >>"))
    })
}

/// One page with `shapes` small filled polygons in varying colours, plus a stroked Bézier curve
/// after every tenth.
pub fn vector_doc(shapes: usize) -> Vec<u8> {
    let mut rng = Lcg(0xc0ffee);
    let mut body = String::with_capacity(shapes * 64);
    for s in 0..shapes {
        let r = |rng: &mut Lcg, m: u32| rng.next() % m;
        let (x, y) = (r(&mut rng, 560) + 20, r(&mut rng, 740) + 20);
        let col = |v: u32| f64::from(v % 1000) / 1000.0;
        body.push_str(&format!("{:.3} {:.3} {:.3} rg ", col(rng.next()), col(rng.next()), col(rng.next())));
        let corners = 3 + s % 4;
        for c in 0..corners {
            let (dx, dy) = (r(&mut rng, 24), r(&mut rng, 24));
            body.push_str(&format!("{} {} {} ", x + dx, y + dy, if c == 0 { "m" } else { "l" }));
        }
        body.push_str("h f\n");
        if s % 10 == 9 {
            let p: Vec<u32> = (0..6).map(|_| r(&mut rng, 560) + 20).collect();
            body.push_str(&format!("0.{} w 0 0 0.5 RG {x} {y} m {} {} {} {} {} {} c S\n", s % 9 + 1, p[0], p[1], p[2], p[3], p[4], p[5]));
        }
    }
    body.push_str(&last_page_marker(0, 1));
    document(1, |pdf, _, parent| {
        let font = pdf.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        let contents = pdf.push(Pdf::stream("", body.as_bytes(), true));
        pdf.push(format!("<< /Type /Page /Parent {parent} 0 R /Contents {contents} 0 R /Resources << /Font << /F1 {font} 0 R >> >> >>"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(bytes: &[u8]) -> lopdf::Document {
        lopdf::Document::load_mem(bytes).expect("generated PDF loads")
    }

    #[test]
    fn generated_documents_are_valid_deterministic_and_carry_the_phrase_once() {
        for (bytes, pages) in [(text_doc(7), 7), (image_doc(2, 40, 30), 2), (vector_doc(50), 1)] {
            let doc = load(&bytes);
            assert_eq!(doc.get_pages().len(), pages);
            let last = *doc.get_pages().keys().max().unwrap();
            assert!(doc.extract_text(&[last]).unwrap().contains(PHRASE), "phrase on the last page");
            if pages > 1 {
                assert!(!doc.extract_text(&[1]).unwrap().contains(PHRASE), "phrase only on the last page");
            }
        }
        assert_eq!(sha256_hex(&text_doc(5)), sha256_hex(&text_doc(5)));
        assert_eq!(sha256_hex(&image_doc(2, 40, 30)), sha256_hex(&image_doc(2, 40, 30)));
        assert_eq!(sha256_hex(&vector_doc(50)), sha256_hex(&vector_doc(50)));
    }

    #[test]
    fn stats_use_the_median_and_report_the_spread() {
        assert_eq!(stats(&[5.0, 1.0, 3.0]), Stats { median: 3.0, min: 1.0, max: 5.0, spread_pct: 4.0 / 3.0 * 100.0 });
        assert_eq!(stats(&[4.0, 2.0]).median, 3.0);
        assert_eq!(stats(&[]).median, 0.0);
        assert_eq!(stats(&[f64::NAN, 2.0]).median, 2.0);
    }

    #[test]
    fn probe_reports_flatten_into_named_samples() {
        let engine = json!({ "probe": "engine", "pages": 3, "phases": { "open": { "ms": 12.5, "peak_rss_kib": 2048 } }, "total_ms": 20.0, "peak_rss_kib": 4096 });
        let ui = json!({ "open_ms": 3.0, "smooth_scroll": { "median_ms": 1.5, "frames": 300, "over_16_7ms": 2 }, "navigate_ms": [1.0, 9.0, 3.0], "raster_memory": { "page_bytes": 10 }, "peak_rss_kib": 1024 });
        let mut s = BTreeMap::new();
        add_samples(&mut s, "engine", &engine);
        add_samples(&mut s, "ui", &ui);
        let keys: Vec<&str> = s.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "engine.open.peak_rss_mib",
                "engine.open_ms",
                "engine.peak_rss_mib",
                "engine.total_ms",
                "ui.navigate.max_ms",
                "ui.navigate.median_ms",
                "ui.open_ms",
                "ui.peak_rss_mib",
                "ui.smooth_scroll.median_ms",
                "ui.smooth_scroll.over_16_7ms",
            ]
        );
        assert_eq!(s["engine.peak_rss_mib"], [4.0]);
        assert_eq!(s["ui.navigate.median_ms"], [3.0]);
        assert_eq!(s["ui.navigate.max_ms"], [9.0]);
    }

    #[test]
    fn budgets_cover_every_scenario_with_positive_limits() {
        let b = load_budgets().unwrap();
        let dev = &b["profiles"]["dev"];
        for s in SCENARIOS {
            let m = dev[s.name].as_object().unwrap_or_else(|| panic!("dev budgets for {}", s.name));
            assert!(m.keys().any(|k| k.starts_with("engine.")) && m.keys().any(|k| k.ends_with("peak_rss_mib")), "{}", s.name);
            for (k, v) in m.iter().filter(|(k, _)| !k.starts_with('_')) {
                assert!(v.as_f64().is_some_and(|x| x > 0.0), "{} {k}", s.name);
                assert!(!k.starts_with("oracle."), "the Poppler reference is never budgeted");
            }
        }
    }
}

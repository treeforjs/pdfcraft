//! `cargo xtask fidelity`: measure PdfCraft's rendering and text extraction against Poppler.
//!
//! Synthetic fixtures (`fixtures.rs`, generated from code; nothing third-party) are rendered by
//! `pdfcraft-cli render` (the same `PageRenderer` pixel path that the `page_render` automation
//! tool uses) and by Poppler's `pdftoppm`, run only as an external oracle process (never linked,
//! never copied). Text comes from `pdfcraft-cli text` and `pdftotext`. Per page we report the
//! mean and maximum channel difference and the share of pixels differing by more than a
//! tolerance; per fixture, word-level F1 against Poppler and against the fixture's own ground
//! truth. Results go to `<target>/fidelity/report.json` and `summary.md`, with side-by-side
//! comparison images (PdfCraft | Poppler | differences in red) under `renders/`.
//!
//! `xtask/baselines/fidelity.json` holds the last accepted numbers (metrics only; no PDFs or
//! images). A page that differs noticeably more than its baseline, or text F1 that drops, fails
//! the command. `--update-baseline` rewrites it. Poppler is the reference here, not Acrobat:
//! where Poppler itself deviates from the spec the ground-truth columns say so for text, and the
//! comparison images let a human judge pixels. Missing Poppler skips the command with a message.
//! See `docs/fidelity.md`.

pub mod fixtures;
pub mod ttf;

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::gates::{root, target_dir};

const USAGE: &str = "cargo xtask fidelity [--dpi 96] [--tolerance 32] [--only NAME] [--out DIR] [--cli PATH] [--release] [--update-baseline]";

/// Largest raster accepted from either renderer (pixels), so a broken header cannot ask for an
/// unbounded allocation.
const MAX_PIXELS: usize = 64 << 20;

struct Options {
    dpi: f64,
    tolerance: u8,
    only: Option<String>,
    out: PathBuf,
    cli: Option<PathBuf>,
    release: bool,
    update: bool,
}

fn options(args: &[String]) -> Result<Options> {
    let mut o = Options { dpi: 96.0, tolerance: 32, only: None, out: target_dir().join("fidelity"), cli: None, release: false, update: false };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().with_context(|| format!("{a} needs a value\nusage: {USAGE}"));
        match a.as_str() {
            "--dpi" => o.dpi = value()?.parse().ok().filter(|d| (18.0..=300.0).contains(d)).context("--dpi must be 18–300")?,
            "--tolerance" => o.tolerance = value()?.parse().context("--tolerance must be 0–255")?,
            "--only" => o.only = Some(value()?.clone()),
            "--out" => o.out = PathBuf::from(value()?),
            "--cli" => o.cli = Some(PathBuf::from(value()?)),
            "--release" => o.release = true,
            "--update-baseline" => o.update = true,
            "-h" | "--help" => bail!("usage: {USAGE}"),
            other => bail!("unknown option {other}\nusage: {USAGE}"),
        }
    }
    if o.update && o.only.is_some() {
        bail!("--update-baseline records every fixture; drop --only");
    }
    Ok(o)
}

// ---------------------------------------------------------------------------------------------
// Report types

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PageResult {
    pub page: usize,
    pub ours: [u32; 2],
    pub oracle: [u32; 2],
    /// Mean absolute channel difference, 0–255, over the union of both rasters.
    pub mean_delta: f64,
    pub max_delta: u8,
    /// Pixels whose largest channel difference exceeds the tolerance, % of all pixels.
    pub diff_pct: f64,
    /// The same, as % of the pixels that are not white in either raster ("ink").
    pub ink_diff_pct: f64,
    pub compare: Option<String>,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TextResult {
    pub ours_words: usize,
    pub oracle_words: usize,
    pub truth_words: usize,
    /// Bag-of-words F1 between PdfCraft and pdftotext.
    pub f1: f64,
    /// Order-aware F1 (longest common word subsequence) between PdfCraft and pdftotext.
    pub order_f1: f64,
    pub truth_f1_ours: f64,
    pub truth_f1_oracle: f64,
    pub truth_order_f1_ours: f64,
    pub missing_vs_truth: Vec<String>,
    pub extra_vs_truth: Vec<String>,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FixtureResult {
    pub name: String,
    pub about: String,
    pub pages: Vec<PageResult>,
    pub text: TextResult,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Report {
    pub dpi: f64,
    pub tolerance: u8,
    pub poppler: String,
    pub fixtures: Vec<FixtureResult>,
    pub regressions: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct PageBase {
    pub diff_pct: f64,
    pub mean_delta: f64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct TextBase {
    pub f1: f64,
    pub truth_f1: f64,
}

/// `xtask/baselines/fidelity.json`: numbers only.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
pub struct Baseline {
    pub dpi: f64,
    pub tolerance: u8,
    pub poppler: String,
    pub pages: BTreeMap<String, PageBase>,
    pub text: BTreeMap<String, TextBase>,
}

// ---------------------------------------------------------------------------------------------
// Entry point

pub fn run(args: &[String]) -> Result<()> {
    let o = options(args)?;
    let Some(poppler) = poppler_version() else {
        println!(
            "fidelity: SKIPPED, Poppler (pdftoppm, pdftotext) is not installed; it is the external oracle (apt install poppler-utils / brew install poppler)"
        );
        return Ok(());
    };
    let cli = match &o.cli {
        Some(p) => p.clone(),
        None => {
            let mut build = vec!["build", "-p", "pdfcraft-cli"];
            if o.release {
                build.push("--release");
            }
            crate::gates::run_args(&build)?;
            target_dir().join(if o.release { "release" } else { "debug" }).join(format!("pdfcraft-cli{}", std::env::consts::EXE_SUFFIX))
        }
    };
    let fixtures_dir = o.out.join("fixtures");
    let renders = o.out.join("renders");
    for d in [&fixtures_dir, &renders] {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    let all = fixtures::all(&root().join("assets/fonts"))?;
    let selected: Vec<_> = all.iter().filter(|f| o.only.as_deref().is_none_or(|n| f.name.contains(n))).collect();
    if selected.is_empty() {
        bail!("no fixture matches --only {:?}", o.only);
    }
    println!("fidelity: {} fixtures at {} dpi, tolerance {}, oracle Poppler {poppler}", selected.len(), o.dpi, o.tolerance);
    let mut results = Vec::new();
    for f in selected {
        let pdf = fixtures_dir.join(format!("{}.pdf", f.name));
        std::fs::write(&pdf, &f.bytes).with_context(|| format!("writing {}", pdf.display()))?;
        let mut pages = Vec::new();
        for p in 1..=f.pages() {
            let r = measure_page(&cli, &pdf, p, &renders, f.name, &o);
            let r = r.unwrap_or_else(|e| PageResult { page: p, error: Some(format!("{e:#}")), ..Default::default() });
            println!(
                "  {:<22} p{p}: diff {:>6.2}%  ink {:>6.2}%  mean Δ {:>5.2}  max Δ {:>3}{}",
                f.name,
                r.diff_pct,
                r.ink_diff_pct,
                r.mean_delta,
                r.max_delta,
                r.error.as_deref().map(|e| format!("  ERROR {e}")).unwrap_or_default()
            );
            pages.push(r);
        }
        let text = measure_text(&cli, &pdf, f);
        println!(
            "  {:<22} text: F1 vs Poppler {:.3} (order {:.3}); vs truth: PdfCraft {:.3}, Poppler {:.3}{}",
            f.name,
            text.f1,
            text.order_f1,
            text.truth_f1_ours,
            text.truth_f1_oracle,
            text.error.as_deref().map(|e| format!("  ERROR {e}")).unwrap_or_default()
        );
        results.push(FixtureResult { name: f.name.into(), about: f.about.into(), pages, text });
    }

    let baseline_path = root().join("xtask/baselines/fidelity.json");
    let current = baseline_of(&results, &o, &poppler);
    let mut notes = Vec::new();
    let regressions = if o.update {
        std::fs::write(&baseline_path, serde_json::to_string_pretty(&current)? + "\n")?;
        notes.push(format!("baseline updated: {} pages, {} fixtures", current.pages.len(), current.text.len()));
        Vec::new()
    } else {
        match std::fs::read_to_string(&baseline_path) {
            Ok(s) => {
                let base: Baseline = serde_json::from_str(&s).context("xtask/baselines/fidelity.json")?;
                if base.dpi != o.dpi || base.tolerance != o.tolerance {
                    bail!("the baseline was recorded at {} dpi, tolerance {}; run with those values or --update-baseline", base.dpi, base.tolerance);
                }
                if base.poppler != poppler {
                    notes.push(format!("baseline was recorded with Poppler {}, this is {poppler}: small shifts may be the oracle's", base.poppler));
                }
                compare_baseline(&base, &results, o.only.is_none(), &mut notes)
            }
            Err(_) => {
                notes.push("no baseline yet: run with --update-baseline to record one".into());
                Vec::new()
            }
        }
    };
    let report = Report { dpi: o.dpi, tolerance: o.tolerance, poppler, fixtures: results, regressions, notes };
    std::fs::write(o.out.join("report.json"), serde_json::to_string_pretty(&report)? + "\n")?;
    let md = summary(&report);
    std::fs::write(o.out.join("summary.md"), &md)?;
    println!("\n{md}");
    println!("report: {}\nsummary: {}", o.out.join("report.json").display(), o.out.join("summary.md").display());
    if !report.regressions.is_empty() {
        bail!("{} fidelity regression(s) against xtask/baselines/fidelity.json", report.regressions.len());
    }
    Ok(())
}

fn poppler_version() -> Option<String> {
    let out = Command::new("pdftoppm").arg("-v").output().ok()?;
    Command::new("pdftotext").arg("-v").output().ok()?;
    let text = String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
    let version = text.lines().next()?.rsplit(' ').next()?.trim().to_string();
    Some(version)
}

// ---------------------------------------------------------------------------------------------
// Processes

struct Exec {
    ok: bool,
    stdout: Vec<u8>,
    stderr: String,
}

/// Runs `cmd` to completion or kills it after `timeout`.
fn exec(cmd: &mut Command, timeout: Duration) -> Result<Exec> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().context("spawn")?;
    let mut so = child.stdout.take().context("stdout")?;
    let mut se = child.stderr.take().context("stderr")?;
    let out = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = so.read_to_end(&mut v);
        v
    });
    let err = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = se.read_to_end(&mut v);
        v
    });
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after {} s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = String::from_utf8_lossy(&err.join().unwrap_or_default()).trim().to_string();
    Ok(Exec { ok: status.success(), stdout, stderr })
}

const TIMEOUT: Duration = Duration::from_secs(60);

fn measure_page(cli: &Path, pdf: &Path, page: usize, renders: &Path, name: &str, o: &Options) -> Result<PageResult> {
    let stem = format!("{name}-p{page}");
    let pam = renders.join(format!("{stem}-pdfcraft.pam"));
    let r =
        exec(Command::new(cli).arg("render").arg(pdf).args(["--page", &page.to_string(), "--dpi", &o.dpi.to_string(), "--out"]).arg(&pam), TIMEOUT)
            .context("pdfcraft-cli render")?;
    if !r.ok {
        bail!("pdfcraft-cli render: {}", r.stderr);
    }
    let ours = parse_pam(&std::fs::read(&pam)?).context("PdfCraft raster")?;
    let prefix = renders.join(format!("{stem}-poppler"));
    let p = exec(
        Command::new("pdftoppm")
            .args(["-cropbox", "-r", &o.dpi.to_string(), "-f", &page.to_string(), "-l", &page.to_string(), "-singlefile"])
            .arg(pdf)
            .arg(&prefix),
        TIMEOUT,
    )
    .context("pdftoppm")?;
    if !p.ok {
        bail!("pdftoppm: {}", p.stderr);
    }
    let ppm = prefix.with_extension("ppm");
    let theirs = parse_ppm(&std::fs::read(&ppm)?).context("Poppler raster")?;
    let (m, image) = compare(&ours, &theirs, o.tolerance);
    let png = renders.join(format!("{stem}-compare.png"));
    std::fs::write(&png, encode_png(&image)?)?;
    let _ = std::fs::remove_file(&pam);
    let _ = std::fs::remove_file(&ppm);
    Ok(PageResult {
        page,
        ours: [ours.w, ours.h],
        oracle: [theirs.w, theirs.h],
        compare: Some(format!("renders/{stem}-compare.png")),
        error: None,
        ..m
    })
}

fn measure_text(cli: &Path, pdf: &Path, f: &fixtures::Fixture) -> TextResult {
    let ours = exec(Command::new(cli).arg("text").arg(pdf), TIMEOUT);
    let theirs = exec(Command::new("pdftotext").args(["-q", "-cropbox"]).arg(pdf).arg("-"), TIMEOUT);
    let mut errors = Vec::new();
    let mut get = |r: Result<Exec>, who: &str| match r {
        Ok(e) if e.ok => String::from_utf8_lossy(&e.stdout).into_owned(),
        Ok(e) => {
            errors.push(format!("{who}: {}", e.stderr));
            String::new()
        }
        Err(e) => {
            errors.push(format!("{who}: {e:#}"));
            String::new()
        }
    };
    let (ours, theirs) = (words(&get(ours, "pdfcraft-cli text")), words(&get(theirs, "pdftotext")));
    let truth = words(&f.truth.join("\n"));
    let (bag_o, bag_t, bag_truth) = (bag(&ours), bag(&theirs), bag(&truth));
    TextResult {
        ours_words: ours.len(),
        oracle_words: theirs.len(),
        truth_words: truth.len(),
        f1: f1(&bag_o, &bag_t),
        order_f1: order_f1(&ours, &theirs),
        truth_f1_ours: f1(&bag_o, &bag_truth),
        truth_f1_oracle: f1(&bag_t, &bag_truth),
        truth_order_f1_ours: order_f1(&ours, &truth),
        missing_vs_truth: difference(&bag_truth, &bag_o),
        extra_vs_truth: difference(&bag_o, &bag_truth),
        error: (!errors.is_empty()).then(|| errors.join("; ")),
    }
}

// ---------------------------------------------------------------------------------------------
// Text metrics

pub fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

fn bag(w: &[String]) -> BTreeMap<&str, usize> {
    let mut m = BTreeMap::new();
    for x in w {
        *m.entry(x.as_str()).or_insert(0) += 1;
    }
    m
}

fn prf(common: usize, a: usize, b: usize) -> f64 {
    if a == 0 && b == 0 {
        return 1.0;
    }
    if a == 0 || b == 0 || common == 0 {
        return 0.0;
    }
    let (p, r) = (common as f64 / a as f64, common as f64 / b as f64);
    2.0 * p * r / (p + r)
}

pub fn f1(a: &BTreeMap<&str, usize>, b: &BTreeMap<&str, usize>) -> f64 {
    let common: usize = a.iter().map(|(w, n)| (*n).min(b.get(w).copied().unwrap_or(0))).sum();
    prf(common, a.values().sum(), b.values().sum())
}

/// F1 over the longest common subsequence of words: penalises reading-order differences.
/// Quadratic, so inputs beyond 4 million cells fall back to the bag F1.
pub fn order_f1(a: &[String], b: &[String]) -> f64 {
    if a.len().saturating_mul(b.len()) > 4_000_000 {
        return f1(&bag(a), &bag(b));
    }
    let mut prev = vec![0usize; b.len() + 1];
    let mut cur = vec![0usize; b.len() + 1];
    for x in a {
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = if x == y { prev[j] + 1 } else { prev[j + 1].max(cur[j]) };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prf(prev[b.len()], a.len(), b.len())
}

/// Words of `a` not covered by `b` (with multiplicity), at most 20.
fn difference(a: &BTreeMap<&str, usize>, b: &BTreeMap<&str, usize>) -> Vec<String> {
    let mut out = Vec::new();
    for (w, n) in a {
        let missing = n.saturating_sub(b.get(w).copied().unwrap_or(0));
        out.extend(std::iter::repeat_n((*w).to_string(), missing));
    }
    out.truncate(20);
    out
}

// ---------------------------------------------------------------------------------------------
// Rasters

#[derive(Debug, PartialEq)]
pub struct Rgb {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

impl Rgb {
    fn get(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        if x >= self.w || y >= self.h {
            return None;
        }
        let i = (y as usize * self.w as usize + x as usize) * 3;
        let p = self.px.get(i..i + 3)?;
        Some([p[0], p[1], p[2]])
    }
}

fn checked_len(w: u32, h: u32, depth: usize) -> Result<usize> {
    let n = (w as usize).checked_mul(h as usize).filter(|n| *n > 0 && *n <= MAX_PIXELS).context("raster size out of range")?;
    n.checked_mul(depth).context("raster size overflow")
}

/// Netpbm PAM `RGB_ALPHA` with premultiplied alpha (what `pdfcraft-cli render --out x.pam`
/// writes), composited over white.
pub fn parse_pam(d: &[u8]) -> Result<Rgb> {
    let end = d.windows(7).position(|w| w == b"ENDHDR\n").context("PAM: no ENDHDR")?;
    let header = std::str::from_utf8(d.get(..end).context("PAM header")?)?;
    let mut fields = BTreeMap::new();
    for line in header.lines().skip(1) {
        if let Some((k, v)) = line.split_once(' ') {
            fields.insert(k, v.trim());
        }
    }
    let num = |k: &str| -> Result<u32> { fields.get(k).context(format!("PAM: no {k}"))?.parse().context(format!("PAM: bad {k}")) };
    let (w, h) = (num("WIDTH")?, num("HEIGHT")?);
    if num("DEPTH")? != 4 || num("MAXVAL")? != 255 {
        bail!("PAM: expected DEPTH 4, MAXVAL 255");
    }
    let data = d.get(end + 7..).context("PAM data")?;
    let len = checked_len(w, h, 4)?;
    let data = data.get(..len).context("PAM: truncated data")?;
    let px = data.as_chunks::<4>().0.iter().flat_map(|p| [0, 1, 2].map(|c| p[c].saturating_add(255 - p[3]))).collect();
    Ok(Rgb { w, h, px })
}

/// Binary PPM (P6, maxval 255), as `pdftoppm` writes.
pub fn parse_ppm(d: &[u8]) -> Result<Rgb> {
    let mut pos = 0usize;
    let mut token = || -> Result<String> {
        loop {
            match d.get(pos) {
                Some(b'#') => {
                    while d.get(pos).is_some_and(|c| *c != b'\n') {
                        pos += 1;
                    }
                }
                Some(c) if c.is_ascii_whitespace() => pos += 1,
                Some(_) => break,
                None => bail!("PPM: truncated header"),
            }
        }
        let start = pos;
        while d.get(pos).is_some_and(|c| !c.is_ascii_whitespace()) {
            pos += 1;
        }
        Ok(String::from_utf8_lossy(d.get(start..pos).unwrap_or_default()).into_owned())
    };
    if token()? != "P6" {
        bail!("PPM: not P6");
    }
    let w: u32 = token()?.parse().context("PPM width")?;
    let h: u32 = token()?.parse().context("PPM height")?;
    if token()? != "255" {
        bail!("PPM: maxval must be 255");
    }
    let len = checked_len(w, h, 3)?;
    let px = d.get(pos + 1..).and_then(|rest| rest.get(..len)).context("PPM: truncated data")?.to_vec();
    Ok(Rgb { w, h, px })
}

/// Pixel metrics over the union of both rasters (a pixel only one covers differs by 255) and
/// a side-by-side image: PdfCraft | Poppler | differences (red above the tolerance, ours faded).
pub fn compare(a: &Rgb, b: &Rgb, tolerance: u8) -> (PageResult, Rgb) {
    let (w, h) = (a.w.max(b.w), a.h.max(b.h));
    let gap = 6u32;
    let cw = a.w + gap + b.w + gap + w;
    let mut out = Rgb { w: cw, h, px: vec![128; cw as usize * h as usize * 3] };
    let mut put = |x: u32, y: u32, c: [u8; 3]| {
        let i = (y as usize * cw as usize + x as usize) * 3;
        if let Some(p) = out.px.get_mut(i..i + 3) {
            p.copy_from_slice(&c);
        }
    };
    let (mut sum, mut max, mut over, mut ink, mut ink_over) = (0u64, 0u8, 0u64, 0u64, 0u64);
    for y in 0..h {
        for x in 0..w {
            let (pa, pb) = (a.get(x, y), b.get(x, y));
            if let Some(p) = pa {
                put(x, y, p);
            }
            if let Some(p) = pb {
                put(a.w + gap + x, y, p);
            }
            let (delta, mean) = match (pa, pb) {
                (Some(p), Some(q)) => {
                    let d = [0, 1, 2].map(|c| p[c].abs_diff(q[c]));
                    (d.into_iter().max().unwrap_or(0), d.iter().map(|v| u64::from(*v)).sum::<u64>() / 3)
                }
                _ => (255, 255),
            };
            let inked = [pa, pb].iter().any(|p| p.is_none_or(|p| p.iter().any(|c| *c < 250)));
            sum += mean;
            max = max.max(delta);
            ink += u64::from(inked);
            let differs = delta > tolerance;
            over += u64::from(differs);
            ink_over += u64::from(differs && inked);
            let shade = pa.map_or(255, |p| {
                let lum = (u32::from(p[0]) * 3 + u32::from(p[1]) * 6 + u32::from(p[2])) / 10;
                (255 - (255 - lum) / 4) as u8
            });
            put(a.w + gap + b.w + gap + x, y, if differs { [255, 0, 0] } else { [shade; 3] });
        }
    }
    let n = (u64::from(w) * u64::from(h)).max(1) as f64;
    let r = PageResult {
        mean_delta: round(sum as f64 / n),
        max_delta: max,
        diff_pct: round(100.0 * over as f64 / n),
        ink_diff_pct: round(if ink == 0 { 0.0 } else { 100.0 * ink_over as f64 / ink as f64 }),
        ..Default::default()
    };
    (r, out)
}

fn round(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn encode_png(img: &Rgb) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut enc = png::Encoder::new(&mut bytes, img.w, img.h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header()?;
    writer.write_image_data(&img.px)?;
    writer.finish()?;
    Ok(bytes)
}

// ---------------------------------------------------------------------------------------------
// Baseline

fn baseline_of(results: &[FixtureResult], o: &Options, poppler: &str) -> Baseline {
    let mut b = Baseline { dpi: o.dpi, tolerance: o.tolerance, poppler: poppler.into(), ..Default::default() };
    for f in results {
        for p in f.pages.iter().filter(|p| p.error.is_none()) {
            b.pages.insert(format!("{}#{}", f.name, p.page), PageBase { diff_pct: p.diff_pct, mean_delta: p.mean_delta });
        }
        if f.text.error.is_none() {
            b.text.insert(f.name.clone(), TextBase { f1: round(f.text.f1), truth_f1: round(f.text.truth_f1_ours) });
        }
    }
    b
}

/// Allowed worsening before a page counts as a regression: half a percentage point of the page
/// (anti-aliasing noise) or a tenth of the recorded value, whichever is larger.
fn diff_slack(base: f64) -> f64 {
    (base * 0.10).max(0.5)
}

fn delta_slack(base: f64) -> f64 {
    (base * 0.15).max(0.5)
}

const TEXT_SLACK: f64 = 0.02;

pub fn compare_baseline(base: &Baseline, results: &[FixtureResult], complete: bool, notes: &mut Vec<String>) -> Vec<String> {
    let mut regressions = Vec::new();
    let mut seen = Vec::new();
    for f in results {
        seen.push(f.name.as_str());
        for p in &f.pages {
            let key = format!("{}#{}", f.name, p.page);
            let Some(b) = base.pages.get(&key) else {
                notes.push(format!("{key}: new page, not in the baseline"));
                continue;
            };
            if let Some(e) = &p.error {
                regressions.push(format!("{key}: failed ({e}); the baseline rendered it"));
                continue;
            }
            if p.diff_pct > b.diff_pct + diff_slack(b.diff_pct) {
                regressions.push(format!("{key}: {:.2}% of pixels differ, baseline {:.2}%", p.diff_pct, b.diff_pct));
            } else if p.diff_pct + diff_slack(b.diff_pct) < b.diff_pct {
                notes.push(format!("{key}: improved to {:.2}% from {:.2}% (consider --update-baseline)", p.diff_pct, b.diff_pct));
            }
            if p.mean_delta > b.mean_delta + delta_slack(b.mean_delta) {
                regressions.push(format!("{key}: mean Δ {:.2}, baseline {:.2}", p.mean_delta, b.mean_delta));
            }
        }
        match base.text.get(&f.name) {
            None => notes.push(format!("{}: text not in the baseline", f.name)),
            Some(_) if f.text.error.is_some() => {
                regressions.push(format!("{}: text extraction failed: {}", f.name, f.text.error.as_deref().unwrap_or("")))
            }
            Some(b) => {
                if f.text.f1 < b.f1 - TEXT_SLACK {
                    regressions.push(format!("{}: text F1 vs Poppler {:.3}, baseline {:.3}", f.name, f.text.f1, b.f1));
                }
                if f.text.truth_f1_ours < b.truth_f1 - TEXT_SLACK {
                    regressions.push(format!("{}: text F1 vs ground truth {:.3}, baseline {:.3}", f.name, f.text.truth_f1_ours, b.truth_f1));
                }
            }
        }
    }
    if complete {
        for name in base.text.keys().filter(|n| !seen.contains(&n.as_str())) {
            notes.push(format!("{name}: in the baseline but no longer generated"));
        }
    }
    regressions
}

// ---------------------------------------------------------------------------------------------
// Summary

fn summary(r: &Report) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "# PdfCraft rendering and text fidelity vs Poppler {}\n", r.poppler);
    let _ = writeln!(
        s,
        "{} dpi; a pixel differs when a channel is off by more than {}. *Ink* is the share of non-white pixels that differ. Text F1 is word-level; *truth* is the fixture's own text.\n",
        r.dpi, r.tolerance
    );
    s.push_str("| Fixture | Pages | Worst diff % | Worst ink diff % | Worst mean Δ | Text F1 vs Poppler | Order F1 | Truth F1 PdfCraft | Truth F1 Poppler |\n");
    s.push_str("|---|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    let fold = |f: &FixtureResult, g: fn(&PageResult) -> f64| f.pages.iter().map(g).fold(0.0, f64::max);
    for f in &r.fixtures {
        let errors = f.pages.iter().filter(|p| p.error.is_some()).count();
        let _ = writeln!(
            s,
            "| {}{} | {} | {:.2} | {:.2} | {:.2} | {:.3} | {:.3} | {:.3} | {:.3} |",
            f.name,
            if errors > 0 { format!(" ({errors} failed)") } else { String::new() },
            f.pages.len(),
            fold(f, |p| p.diff_pct),
            fold(f, |p| p.ink_diff_pct),
            fold(f, |p| p.mean_delta),
            f.text.f1,
            f.text.order_f1,
            f.text.truth_f1_ours,
            f.text.truth_f1_oracle
        );
    }
    let mut pages: Vec<(&FixtureResult, &PageResult)> = r.fixtures.iter().flat_map(|f| f.pages.iter().map(move |p| (f, p))).collect();
    pages.sort_by(|x, y| y.1.ink_diff_pct.total_cmp(&x.1.ink_diff_pct));
    s.push_str("\n## Worst pages (by ink diff)\n\n");
    for (f, p) in pages.iter().take(8) {
        let _ = writeln!(
            s,
            "- {} p{}: {:.2}% of ink differs ({:.2}% of page, mean Δ {:.2}, size {}×{} vs {}×{}) — `{}`",
            f.name,
            p.page,
            p.ink_diff_pct,
            p.diff_pct,
            p.mean_delta,
            p.ours[0],
            p.ours[1],
            p.oracle[0],
            p.oracle[1],
            p.compare.as_deref().or(p.error.as_deref()).unwrap_or("")
        );
    }
    let text_issues: Vec<&FixtureResult> =
        r.fixtures.iter().filter(|f| !f.text.missing_vs_truth.is_empty() || !f.text.extra_vs_truth.is_empty()).collect();
    if !text_issues.is_empty() {
        s.push_str("\n## PdfCraft text vs ground truth\n\n");
        for f in text_issues {
            let _ = writeln!(s, "- {}: missing {:?}; extra {:?}", f.name, f.text.missing_vs_truth, f.text.extra_vs_truth);
        }
    }
    if !r.regressions.is_empty() {
        s.push_str("\n## Regressions\n\n");
        for x in &r.regressions {
            let _ = writeln!(s, "- {x}");
        }
    }
    if !r.notes.is_empty() {
        s.push_str("\n## Notes\n\n");
        for x in &r.notes {
            let _ = writeln!(s, "- {x}");
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32, c: [u8; 3]) -> Rgb {
        Rgb { w, h, px: c.repeat((w * h) as usize) }
    }

    #[test]
    fn identical_rasters_have_no_difference() {
        let (m, side) = compare(&img(4, 3, [10, 20, 30]), &img(4, 3, [10, 20, 30]), 0);
        assert_eq!((m.mean_delta, m.max_delta, m.diff_pct, m.ink_diff_pct), (0.0, 0, 0.0, 0.0));
        assert_eq!((side.w, side.h), (4 + 6 + 4 + 6 + 4, 3));
    }

    #[test]
    fn metrics_count_tolerance_ink_and_size_mismatch() {
        let a = img(2, 2, [255, 255, 255]);
        let mut b = img(2, 2, [255, 255, 255]);
        b.px[0..3].copy_from_slice(&[0, 255, 255]); // one pixel off by 255 in one channel
        b.px[3..6].copy_from_slice(&[250, 255, 255]); // one within tolerance
        let (m, _) = compare(&a, &b, 32);
        assert_eq!(m.max_delta, 255);
        assert_eq!(m.diff_pct, 25.0);
        assert_eq!(m.ink_diff_pct, 100.0, "the only inked pixel differs");
        assert_eq!(m.mean_delta, round((85.0 + 1.0) / 4.0));
        // A raster one column wider: the extra column differs entirely.
        let (m, _) = compare(&img(3, 2, [255, 255, 255]), &a, 32);
        assert!((m.diff_pct - 100.0 / 3.0).abs() < 0.01, "{}", m.diff_pct);
    }

    #[test]
    fn pam_is_composited_over_white_and_ppm_parses_comments() {
        let mut pam = b"P7\nWIDTH 2\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n".to_vec();
        pam.extend_from_slice(&[0, 0, 0, 0, 100, 0, 0, 128]); // transparent, half red premultiplied
        let r = parse_pam(&pam).expect("pam");
        assert_eq!(r.px, vec![255, 255, 255, 227, 127, 127]);
        let mut ppm = b"P6\n# made by a test\n2 1\n255\n".to_vec();
        ppm.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(parse_ppm(&ppm).expect("ppm"), Rgb { w: 2, h: 1, px: vec![1, 2, 3, 4, 5, 6] });
        assert!(parse_ppm(b"P6\n2 1\n255\n\x01").is_err(), "truncated");
        assert!(parse_ppm(b"P6\n99999 99999\n255\n").is_err(), "oversized");
        assert!(parse_pam(b"P7\nWIDTH 1\nHEIGHT 1\nDEPTH 3\nMAXVAL 255\nENDHDR\n\0\0\0").is_err(), "wrong depth");
    }

    #[test]
    fn word_f1_and_order_f1() {
        let a = words("The quick, brown fox!");
        let b = words("brown fox the quick");
        assert_eq!(f1(&bag(&a), &bag(&b)), 1.0);
        assert!((order_f1(&a, &b) - 0.5).abs() < 1e-9, "LCS of 2 in 4 and 4");
        assert_eq!(order_f1(&a, &a), 1.0);
        assert_eq!(f1(&bag(&[]), &bag(&[])), 1.0, "both empty agree");
        assert_eq!(f1(&bag(&a), &bag(&[])), 0.0);
        assert_eq!(difference(&bag(&words("a a b")), &bag(&words("a"))), vec!["a", "b"]);
    }

    #[test]
    fn baseline_flags_worse_pages_and_text_but_not_noise() {
        let base = Baseline {
            dpi: 96.0,
            tolerance: 32,
            poppler: "x".into(),
            pages: [("f#1".to_string(), PageBase { diff_pct: 2.0, mean_delta: 1.0 })].into(),
            text: [("f".to_string(), TextBase { f1: 0.95, truth_f1: 1.0 })].into(),
        };
        let result = |diff: f64, f1: f64| FixtureResult {
            name: "f".into(),
            about: String::new(),
            pages: vec![PageResult { page: 1, diff_pct: diff, mean_delta: 1.2, ..Default::default() }],
            text: TextResult { f1, truth_f1_ours: 1.0, ..Default::default() },
        };
        let mut notes = Vec::new();
        assert!(compare_baseline(&base, &[result(2.4, 0.94)], true, &mut notes).is_empty(), "within slack");
        let r = compare_baseline(&base, &[result(2.6, 0.90)], true, &mut notes);
        assert_eq!(r.len(), 2, "{r:?}");
        let mut failed = result(0.0, 0.95);
        failed.pages[0].error = Some("boom".into());
        assert_eq!(compare_baseline(&base, &[failed], true, &mut notes).len(), 1);
    }
}

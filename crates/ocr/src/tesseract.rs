//! An optional engine that runs an installed `tesseract` executable.
//!
//! Tesseract is never linked or shipped: when the user has installed it, PdfCraft finds the
//! program (a configured path, `$PDFCRAFT_TESSERACT`, or `tesseract` on `PATH`), writes the page
//! image to its standard input as a binary PGM/PPM, and reads its TSV word list from standard
//! output. Each run has a time limit and an output cap, and the output is parsed as hostile
//! input ([`parse_tsv`]). Without the program the engine reports itself unavailable. The web
//! build cannot start programs, so there it is always unavailable.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use crate::engines::{EngineInfo, KNOWN_LANGUAGES, OcrEngine};
use crate::{Line, OcrError, Word};

/// The engine id.
pub const ID: &str = "tesseract";

/// The most TSV output read from one recognition.
pub const MAX_OUTPUT: usize = 32 * 1024 * 1024;
/// The most words accepted from one page (as [`crate::validate_words`] allows).
pub const MAX_WORDS: usize = 200_000;
/// The longest word accepted, in UTF-8 bytes.
pub const MAX_WORD_BYTES: usize = 16_384;
/// The most recognized text accepted from one page.
pub const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;

/// How to find and run Tesseract.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// The program to run; `None` looks for `tesseract` on `PATH`.
    pub path: Option<PathBuf>,
    /// Time limit for recognizing one image.
    pub timeout: Duration,
    /// Time limit for asking the version or the installed languages.
    pub probe_timeout: Duration,
    /// The most bytes read from its output.
    pub max_output: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config { path: None, timeout: Duration::from_secs(120), probe_timeout: Duration::from_secs(10), max_output: MAX_OUTPUT }
    }
}

/// What asking the program found.
#[derive(Clone, Debug)]
struct Probe {
    program: PathBuf,
    version: Option<String>,
    /// Tesseract's trained-data names (`eng`, `deu`, …).
    languages: Vec<String>,
}

/// The Tesseract engine.
#[derive(Debug)]
pub struct Tesseract {
    config: Config,
    probe: Mutex<Option<Probe>>,
}

impl Tesseract {
    pub fn new(config: Config) -> Tesseract {
        Tesseract { config, probe: Mutex::new(None) }
    }

    /// The default configuration, with the program from `$PDFCRAFT_TESSERACT` when it is set.
    pub fn from_env() -> Tesseract {
        let path = std::env::var_os("PDFCRAFT_TESSERACT").filter(|p| !p.is_empty()).map(PathBuf::from);
        Tesseract::new(Config { path, ..Config::default() })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The program that would run: the configured path when it is a file, else `tesseract` on
    /// `PATH`. `None` when there is none (and always on the web).
    pub fn locate(&self) -> Option<PathBuf> {
        if cfg!(target_arch = "wasm32") {
            return None;
        }
        match &self.config.path {
            Some(p) => p.is_file().then(|| p.clone()),
            None => find_on_path(),
        }
    }

    fn unavailable(&self) -> String {
        if cfg!(target_arch = "wasm32") {
            return "Tesseract can't run in the web version".into();
        }
        match &self.config.path {
            Some(p) => format!("Tesseract is not installed at {}", p.display()),
            None => "Tesseract is not installed (no tesseract program on PATH; install it, or set PDFCRAFT_TESSERACT to the program)".into(),
        }
    }

    /// Ask the program for its version and languages (cached for the same program).
    fn probe(&self, fresh: bool) -> Result<Probe, OcrError> {
        let program = self.locate().ok_or_else(|| OcrError::Unavailable(self.unavailable()))?;
        let mut slot = self.probe.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(p) = slot.as_ref().filter(|p| !fresh && p.program == program) {
            return Ok(p.clone());
        }
        let cap = 1024 * 1024;
        let version = run(&program, &["--version"], None, self.config.probe_timeout, cap)
            .map_err(|e| OcrError::Unavailable(format!("{} did not run: {e}", program.display())))?;
        let langs = run(&program, &["--list-langs"], None, self.config.probe_timeout, cap)
            .map_err(|e| OcrError::Unavailable(format!("{} did not list its languages: {e}", program.display())))?;
        let probe = Probe {
            program,
            version: parse_version(&version.stdout).or_else(|| parse_version(&version.stderr)),
            // Older versions print the list on standard error.
            languages: parse_languages(if langs.stdout.is_empty() { &langs.stderr } else { &langs.stdout }),
        };
        *slot = Some(probe.clone());
        Ok(probe)
    }

    fn trained_data(&self, language: &str) -> Result<(PathBuf, &'static str), OcrError> {
        let code = KNOWN_LANGUAGES.iter().find(|l| l.0 == language).map(|l| l.1).ok_or_else(|| {
            OcrError::Language(format!("unsupported recognition language {language:?}; see the engine's languages for what it reads"))
        })?;
        let probe = self.probe(false)?;
        if !probe.languages.iter().any(|l| l == code) {
            return Err(OcrError::Language(format!(
                "Tesseract has no {code} language data installed for {language:?}; install it (e.g. the tesseract-ocr-{code} package)"
            )));
        }
        Ok((probe.program, code))
    }
}

impl OcrEngine for Tesseract {
    fn id(&self) -> &str {
        ID
    }

    fn name(&self) -> &str {
        "Tesseract (installed program)"
    }

    fn info(&self) -> EngineInfo {
        let (available, version, languages, detail) = match self.probe(true) {
            Ok(p) => {
                let langs = KNOWN_LANGUAGES.iter().filter(|l| p.languages.iter().any(|t| t == l.1)).map(|l| l.0.to_owned()).collect();
                (true, p.version, langs, format!("runs {}", p.program.display()))
            }
            Err(e) => (false, None, Vec::new(), e.to_string()),
        };
        EngineInfo { id: ID.into(), name: self.name().into(), available, version, languages, external: true, detail }
    }

    fn prepare(&self, language: &str) -> Result<(), OcrError> {
        self.trained_data(language).map(|_| ())
    }

    fn recognize(&self, pixels: &[u8], width: u32, height: u32, language: &str) -> Result<Vec<Line>, OcrError> {
        let (program, code) = self.trained_data(language)?;
        let image = pnm(pixels, width, height)?;
        let out = run(&program, &["stdin", "stdout", "-l", code, "--psm", "3", "tsv"], Some(image), self.config.timeout, self.config.max_output)
            .map_err(|e| OcrError::Recognize(format!("Tesseract: {e}")))?;
        parse_tsv(&out.stdout, width, height)
    }
}

/// `tesseract` (`tesseract.exe` on Windows) in a `PATH` directory.
fn find_on_path() -> Option<PathBuf> {
    let name = if cfg!(windows) { "tesseract.exe" } else { "tesseract" };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).take(256).map(|d| d.join(name)).find(|p| p.is_file())
}

/// "tesseract 5.3.0" (or "tesseract v4.1.1…") from `--version`.
fn parse_version(out: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(out.get(..out.len().min(64 * 1024))?);
    let line = text.lines().map(str::trim).find(|l| l.to_ascii_lowercase().starts_with("tesseract "))?;
    let v = line.split_whitespace().nth(1)?.trim_start_matches('v');
    let v: String = v.chars().take(64).filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).collect();
    (!v.is_empty()).then_some(v)
}

/// The trained-data names `--list-langs` prints, one per line after its heading.
fn parse_languages(out: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(out.get(..out.len().min(1024 * 1024)).unwrap_or_default());
    let mut langs: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| (1..=64).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
        .take(1000)
        .map(str::to_owned)
        .collect();
    langs.sort();
    langs.dedup();
    langs
}

/// The image as a binary PGM (gray) or PPM (RGB; RGBA is composited over white first).
fn pnm(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>, OcrError> {
    let rgb = crate::recognition_pixels(pixels, width, height)?;
    let magic = if crate::validate_image(&rgb, width, height)? == 1 { "P5" } else { "P6" };
    let mut out = format!("{magic}\n{width} {height}\n255\n").into_bytes();
    out.try_reserve_exact(rgb.len()).map_err(|_| OcrError::InvalidImage("not enough memory for the recognition image".into()))?;
    out.extend_from_slice(&rgb);
    Ok(out)
}

/// Common ligatures Tesseract may print, spelled out (the searchable-text font has no glyph
/// for them).
fn spell_ligatures(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\u{FB00}' => out.push_str("ff"),
            '\u{FB01}' => out.push_str("fi"),
            '\u{FB02}' => out.push_str("fl"),
            '\u{FB03}' => out.push_str("ffi"),
            '\u{FB04}' => out.push_str("ffl"),
            '\u{FB05}' | '\u{FB06}' => out.push_str("st"),
            c => out.push(c),
        }
    }
    out
}

/// Parse Tesseract's TSV output (`level page_num block_num par_num line_num word_num left top
/// width height conf text`) into lines of words in image pixels. Only word rows (level 5) are
/// used; a row that is malformed, out of the image, empty or too long is left out, and so is a
/// word with characters the searchable-text font can't encode (after spelling out ligatures).
/// Confidence (0–100, -1 for none) becomes 0–1. Output larger than [`MAX_OUTPUT`], or with more
/// than [`MAX_WORDS`] words or [`MAX_TEXT_BYTES`] of text, is refused.
pub fn parse_tsv(out: &[u8], width: u32, height: u32) -> Result<Vec<Line>, OcrError> {
    if out.len() > MAX_OUTPUT {
        return Err(OcrError::Recognize(format!("Tesseract's output is larger than {MAX_OUTPUT} bytes")));
    }
    let (w, h) = (i64::from(width), i64::from(height));
    let text = String::from_utf8_lossy(out);
    let mut lines: Vec<Line> = Vec::new();
    let mut key = None;
    let (mut words, mut bytes) = (0usize, 0usize);
    for row in text.lines() {
        let fields: Vec<&str> = row.splitn(12, '\t').collect();
        let [level, page, block, par, line, _word, left, top, bw, bh, conf, text] = fields[..] else { continue };
        if level.trim() != "5" {
            continue;
        }
        let int = |s: &str| s.trim().parse::<i64>().ok().filter(|v| (-(1 << 24)..=1 << 24).contains(v));
        let (Some(page), Some(block), Some(par), Some(line), Some(l), Some(t), Some(bw), Some(bh)) =
            (int(page), int(block), int(par), int(line), int(left), int(top), int(bw), int(bh))
        else {
            continue;
        };
        // The box, clipped to the image; one wholly outside it, or empty, is left out.
        let (r, b) = (l.saturating_add(bw).min(w), t.saturating_add(bh).min(h));
        let (l, t) = (l.max(0), t.max(0));
        if bw <= 0 || bh <= 0 || l >= r || t >= b {
            continue;
        }
        let word = spell_ligatures(text.trim_end_matches(['\r', '\n']).trim());
        if word.is_empty() || word.len() > MAX_WORD_BYTES || crate::validate_text(&word).is_err() {
            continue;
        }
        let confidence = conf.trim().parse::<f32>().ok().filter(|c| c.is_finite() && *c >= 0.0).map(|c| (c / 100.0).clamp(0.0, 1.0));
        words = words.saturating_add(1);
        bytes = bytes.saturating_add(word.len());
        if words > MAX_WORDS || bytes > MAX_TEXT_BYTES {
            return Err(OcrError::Recognize(format!("Tesseract read more than {MAX_WORDS} words or {MAX_TEXT_BYTES} bytes of text on one page")));
        }
        let this = (page, block, par, line);
        if key != Some(this) || lines.is_empty() {
            lines.push(Line::default());
            key = Some(this);
        }
        if let Some(current) = lines.last_mut() {
            current.words.push(Word { text: word, rect: [l as f32, t as f32, r as f32, b as f32], confidence });
        }
    }
    Ok(lines)
}

/// What a finished program wrote.
#[derive(Debug)]
struct Output {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error("could not start it: {0}")]
    Start(String),
    #[error("it took longer than {0:.1} seconds and was stopped")]
    Timeout(f64),
    #[error("its output was larger than {0} bytes")]
    TooLarge(usize),
    #[error("it failed ({0}): {1}")]
    Failed(String, String),
    #[error("{0}")]
    Io(String),
}

/// Run `program` with `args`, feeding it `stdin`, for at most `timeout`, keeping at most
/// `max_output` bytes of standard output (more stops it) and 64 KiB of standard error.
#[cfg(not(target_arch = "wasm32"))]
fn run(program: &Path, args: &[&str], stdin: Option<Vec<u8>>, timeout: Duration, max_output: usize) -> Result<Output, RunError> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Instant;

    const MAX_STDERR: usize = 64 * 1024;
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| RunError::Start(e.to_string()))?;
    let start = Instant::now();
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A program that exits without reading all of it closes the pipe: not an error here.
        let spawned = std::thread::Builder::new().name("pdfcraft-ocr-stdin".into()).spawn(move || {
            let _ = pipe.write_all(&bytes);
        });
        if let Err(e) = spawned {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RunError::Io(format!("could not feed it the image: {e}")));
        }
    }
    let overflow = Arc::new(AtomicBool::new(false));
    let reader = |pipe: Option<Box<dyn Read + Send>>, cap: usize, flag: Option<Arc<AtomicBool>>| {
        let (tx, rx) = mpsc::channel();
        let spawned = pipe.map(|mut pipe| {
            std::thread::Builder::new().name("pdfcraft-ocr-output".into()).spawn(move || {
                let mut kept = Vec::new();
                let read = (&mut pipe).take(cap as u64 + 1).read_to_end(&mut kept);
                if kept.len() > cap {
                    kept.truncate(cap);
                    match &flag {
                        // Too much: the caller stops the program.
                        Some(f) => f.store(true, Ordering::SeqCst),
                        // Keep draining so the program never blocks on a full pipe.
                        None => {
                            let _ = std::io::copy(&mut pipe, &mut std::io::sink());
                        }
                    }
                }
                let _ = tx.send(read.map(|_| kept).map_err(|e| e.to_string()));
            })
        });
        (spawned, rx)
    };
    let (out_thread, out_rx) = reader(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>), max_output, Some(overflow.clone()));
    let (err_thread, err_rx) = reader(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>), MAX_STDERR, None);
    if !matches!(out_thread, Some(Ok(_))) || !matches!(err_thread, Some(Ok(_))) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RunError::Io("could not read its output".into()));
    }
    let status = loop {
        if overflow.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RunError::TooLarge(max_output));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Io(e.to_string()));
            }
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RunError::Timeout(timeout.as_secs_f64()));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // The pipes close when the program exits; a child it started could hold them open, so the
    // wait for the rest of the output is bounded too.
    let remaining = |start: Instant| timeout.saturating_sub(start.elapsed()).max(Duration::from_millis(100));
    let stdout = match out_rx.recv_timeout(remaining(start)) {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => return Err(RunError::Io(format!("reading its output: {e}"))),
        Err(_) => return Err(RunError::Io("its output did not end".into())),
    };
    if overflow.load(Ordering::SeqCst) {
        return Err(RunError::TooLarge(max_output));
    }
    let stderr = err_rx.recv_timeout(remaining(start)).ok().and_then(Result::ok).unwrap_or_default();
    if !status.success() {
        let why: String = String::from_utf8_lossy(&stderr).trim().chars().take(500).collect();
        return Err(RunError::Failed(status.to_string(), why));
    }
    Ok(Output { stdout, stderr })
}

#[cfg(target_arch = "wasm32")]
fn run(_: &Path, _: &[&str], _: Option<Vec<u8>>, _: Duration, _: usize) -> Result<Output, RunError> {
    Err(RunError::Start("programs can't be started in the web version".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TSV: &str = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext
1\t1\t0\t0\t0\t0\t0\t0\t200\t100\t-1\t
2\t1\t1\t0\t0\t0\t10\t10\t180\t60\t-1\t
4\t1\t1\t1\t1\t0\t10\t10\t180\t20\t-1\t
5\t1\t1\t1\t1\t1\t10\t10\t50\t20\t96.5\tHello
5\t1\t1\t1\t1\t2\t70\t10\t60\t20\t91\tWorld
5\t1\t1\t1\t2\t1\t10\t50\t40\t20\t-1\t\u{FB01}ne
5\t1\t1\t1\t2\t2\t60\t50\t40\t20\t130\tcafé\r
";

    #[test]
    fn tsv_words_become_lines_with_boxes_and_confidence() {
        let lines = parse_tsv(TSV.as_bytes(), 200, 100).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text(), "Hello World");
        assert_eq!(lines[0].words[0].rect, [10.0, 10.0, 60.0, 30.0]);
        assert_eq!(lines[0].words[0].confidence, Some(0.965));
        assert_eq!(lines[1].text(), "fine café", "ligatures are spelled out, CR trimmed");
        assert_eq!(lines[1].words[0].confidence, None, "-1 means no confidence");
        assert_eq!(lines[1].words[1].confidence, Some(1.0), "clamped");
    }

    #[test]
    fn malformed_rows_are_left_out() {
        let rows = [
            "5\t1\t1\t1\t1\t1\t10\t10\t50\t20\t96",                     // too few fields
            "5\t1\t1\t1\t1\t1\tx\t10\t50\t20\t96\tbad",                 // not a number
            "5\t1\t1\t1\t1\t1\t10\t10\t0\t20\t96\tempty",               // empty box
            "5\t1\t1\t1\t1\t1\t10\t10\t-5\t20\t96\tnegative",           // negative size
            "5\t1\t1\t1\t1\t1\t500\t10\t50\t20\t96\toutside",           // right of the image
            "5\t1\t1\t1\t1\t1\t99999999999999999999\t0\t5\t5\t9\thuge", // overflows
            "5\t1\t1\t1\t1\t1\t9223372036854775807\t0\t5\t5\t9\tmax",   // out of range
            "5\t1\t1\t1\t1\t1\t10\t10\t50\t20\tNaN\t   ",               // blank text
            "5\t1\t1\t1\t1\t1\t10\t10\t50\t20\t50\t漢字",               // not encodable
            "5\t1\t1\t1\t1\t1\t10\t10\t50\t20\t50\tbell\u{7}",          // control character
            "\t\t\t\t\t\t\t\t\t\t\t",
            "",
        ];
        let mut out = rows.join("\n").into_bytes();
        out.extend_from_slice(b"\n5\t1\t1\t1\t1\t1\t-10\t-10\t50\t20\tinf\tclipped\n");
        out.extend_from_slice(&[0xff, 0xfe, b'\n']);
        let lines = parse_tsv(&out, 200, 100).unwrap();
        let words: Vec<&Word> = lines.iter().flat_map(|l| &l.words).collect();
        assert_eq!(words.len(), 1, "{words:?}");
        assert_eq!((words[0].text.as_str(), words[0].rect, words[0].confidence), ("clipped", [0.0, 0.0, 40.0, 10.0], None));
        assert!(parse_tsv(b"", 1, 1).unwrap().is_empty());
        assert!(parse_tsv(&[0u8; 4096], 1, 1).unwrap().is_empty());
    }

    #[test]
    fn huge_output_is_refused_or_bounded() {
        assert!(parse_tsv(&vec![b'5'; MAX_OUTPUT + 1], 10, 10).is_err());
        let row = "5\t1\t1\t1\t1\t1\t0\t0\t1\t1\t90\tw\n";
        let many = row.repeat(MAX_WORDS + 1);
        assert!(parse_tsv(many.as_bytes(), 10, 10).is_err(), "too many words");
        let long = format!("5\t1\t1\t1\t1\t1\t0\t0\t1\t1\t90\t{}\n", "a".repeat(MAX_WORD_BYTES + 1));
        assert!(parse_tsv(long.as_bytes(), 10, 10).unwrap().is_empty(), "an overlong word is left out");
        let text = format!("5\t1\t1\t1\t1\t1\t0\t0\t1\t1\t90\t{}\n", "a".repeat(MAX_WORD_BYTES)).repeat(MAX_TEXT_BYTES / MAX_WORD_BYTES + 1);
        assert!(parse_tsv(text.as_bytes(), 10, 10).is_err(), "too much text");
    }

    #[test]
    fn version_and_languages_are_parsed_defensively() {
        assert_eq!(parse_version(b"tesseract 5.3.0\n leptonica-1.82.0\n").as_deref(), Some("5.3.0"));
        assert_eq!(parse_version(b"tesseract v4.1.1-rc2-25-g9707\n").as_deref(), Some("4.1.1-rc2-25-g9707"));
        assert_eq!(parse_version(b"something else\n"), None);
        assert_eq!(parse_version(b"tesseract \n"), None);
        let langs = parse_languages(b"List of available languages in \"/usr/share/tessdata/\" (3):\neng\nosd\ndeu\neng\n../../x\n");
        assert_eq!(langs, ["deu", "eng", "osd"]);
    }

    #[test]
    fn images_are_written_as_pnm() {
        assert_eq!(pnm(&[0, 255], 2, 1).unwrap(), b"P5\n2 1\n255\n\x00\xff");
        assert_eq!(pnm(&[1, 2, 3, 255], 1, 1).unwrap(), b"P6\n1 1\n255\n\x01\x02\x03");
        assert!(pnm(&[1, 2], 1, 1).is_err());
    }

    #[test]
    fn a_missing_program_is_unavailable() {
        let t = Tesseract::new(Config { path: Some("/nonexistent/pdfcraft/tesseract".into()), ..Config::default() });
        assert_eq!(t.locate(), None);
        let info = t.info();
        assert!(!info.available && info.languages.is_empty() && info.external, "{info:?}");
        assert!(info.detail.contains("not installed"), "{}", info.detail);
        assert!(matches!(t.prepare("en"), Err(OcrError::Unavailable(_))));
        assert!(matches!(t.recognize(&[0; 4], 2, 2, "en"), Err(OcrError::Unavailable(_))));
    }

    /// A stand-in program with Tesseract's command line, so the process handling runs without
    /// Tesseract installed.
    #[cfg(unix)]
    mod process {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn script(name: &str, recognize: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!("pdfcraft-tesseract-{}-{name}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("tesseract");
            let body = format!(
                "#!/bin/sh\ncase \"$1\" in\n--version) echo 'tesseract 5.9.9'; echo ' leptonica-1.0';;\n--list-langs) echo 'List of available languages in \"/x/\" (2):'; echo eng; echo osd;;\n*) {recognize};;\nesac\n"
            );
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        fn engine(path: PathBuf, timeout: Duration, max_output: usize) -> Tesseract {
            Tesseract::new(Config { path: Some(path), timeout, max_output, ..Config::default() })
        }

        #[test]
        fn a_stand_in_program_is_probed_fed_and_read() {
            // Reads the whole image from standard input, checks its header, prints one word.
            let read = "img=$(wc -c); [ \"$2\" = stdout ] && [ \"$4\" = eng ] && [ \"$img\" -gt 0 ] || exit 3; printf 'level\\tpage_num\\tblock_num\\tpar_num\\tline_num\\tword_num\\tleft\\ttop\\twidth\\theight\\tconf\\ttext\\n5\\t1\\t1\\t1\\t1\\t1\\t2\\t3\\t10\\t5\\t88\\tFake\\n'";
            let t = engine(script("ok", read), Duration::from_secs(20), MAX_OUTPUT);
            let info = t.info();
            assert!(info.available, "{info:?}");
            assert_eq!((info.version.as_deref(), info.languages.clone()), (Some("5.9.9"), vec!["en".to_owned()]));
            t.prepare("en").unwrap();
            assert!(matches!(t.prepare("de"), Err(OcrError::Language(_))), "no deu data installed");
            assert!(matches!(t.prepare("xx"), Err(OcrError::Language(_))));
            let lines = t.recognize(&vec![255; 40 * 20 * 4], 40, 20, "en").unwrap();
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0].words[0], Word { text: "Fake".into(), rect: [2.0, 3.0, 12.0, 8.0], confidence: Some(0.88) });
        }

        #[test]
        fn a_failing_program_reports_its_error() {
            let t = engine(script("fail", "echo 'Error opening data file' >&2; exit 1"), Duration::from_secs(20), MAX_OUTPUT);
            let e = t.recognize(&[0; 4], 2, 2, "en").unwrap_err().to_string();
            assert!(e.contains("Error opening data file"), "{e}");
        }

        #[test]
        fn a_hanging_program_is_stopped() {
            let t = engine(script("hang", "exec sleep 30"), Duration::from_millis(300), MAX_OUTPUT);
            let start = std::time::Instant::now();
            let e = t.recognize(&[0; 4], 2, 2, "en").unwrap_err().to_string();
            assert!(e.contains("longer than"), "{e}");
            assert!(start.elapsed() < Duration::from_secs(10));
        }

        #[test]
        fn endless_output_is_capped() {
            let t = engine(script("flood", "exec yes 5"), Duration::from_secs(20), 64 * 1024);
            let e = t.recognize(&[0; 4], 2, 2, "en").unwrap_err().to_string();
            assert!(e.contains("larger than"), "{e}");
        }

        #[test]
        fn hostile_output_does_not_panic() {
            let t = engine(script("junk", "head -c 200000 /dev/urandom"), Duration::from_secs(20), MAX_OUTPUT);
            assert!(t.recognize(&[0; 4], 2, 2, "en").is_ok());
        }
    }

    /// With Tesseract installed (it is not needed in CI), a real recognition.
    #[test]
    fn reads_rendered_text_when_installed() {
        let t = Tesseract::from_env();
        let info = t.info();
        if !info.available || !info.languages.iter().any(|l| l == "en") {
            eprintln!("skipped: Tesseract with English data is not installed ({})", info.detail);
            return;
        }
        eprintln!("running {} {:?}", info.detail, info.version);
        let (w, h, px) = crate::tests::test_image::hello();
        let lines = t.recognize(&px, w, h, "en").unwrap();
        let text: Vec<String> = lines.iter().map(Line::text).collect();
        assert!(text.iter().any(|t| t.contains("HELL") && t.contains("WOR")), "{text:?}");
        assert!(lines.iter().flat_map(|l| &l.words).all(|w| w.confidence.is_some()));
    }
}

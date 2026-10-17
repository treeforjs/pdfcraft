//! Evidence for the native vector path (M2): writes contributor-original synthetic fixtures,
//! compares the native device with the bootstrap renderer and with an external oracle's PPM
//! (Poppler's `pdftoppm`, run by the caller as a separate process), and times native against
//! forced-bootstrap renders. A developer tool, not part of the product.
//!
//! ```sh
//! cargo run -p pdfcraft-render --example vector_oracle -- fixtures DIR
//! for f in DIR/*.pdf; do pdftoppm -r 144 -singlefile "$f" "${f%.pdf}-poppler"; done
//! cargo run -p pdfcraft-render --example vector_oracle -- compare DIR 2
//! cargo run --release -p pdfcraft-render --example vector_oracle -- bench DIR 15
//! ```

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest, Tile};

type Error = Box<dyn std::error::Error>;

fn main() -> Result<(), Error> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("fixtures") => fixtures(Path::new(&args[2])),
        Some("compare") => compare(Path::new(&args[2]), args[3].parse()?),
        Some("bench") => bench(Path::new(&args[2]), args[3].parse()?),
        Some("one") => one(Path::new(&args[2]), &args[3]),
        Some("survey") => survey(Path::new(&args[2]), args[3].parse()?, args.get(4).map(|s| s.parse()).transpose()?.unwrap_or(400)),
        _ => {
            Err("usage: vector_oracle fixtures DIR | compare DIR SCALE | bench DIR REPS | one PDF native|bootstrap | survey PDF SCALE [MAX_PAGES]"
                .into())
        }
    }
}

// ---------------------------------------------------------------- fixtures

/// A one-page PDF. `extra` objects are numbered from 5.
fn pdf(media: &str, page_extra: &str, content: &[u8], extra: &[Vec<u8>]) -> Vec<u8> {
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!("<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [{media}] >>").into_bytes(),
        format!("<< /Type /Page /Parent 2 0 R /Contents 4 0 R {page_extra} >>").into_bytes(),
        stream("", content),
    ];
    objects.extend(extra.iter().cloned());
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        bytes.extend_from_slice(object);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    bytes
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< /Length {} {dict} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// A full circle of radius `r` around (`x`, `y`) from four cubic arcs.
fn circle(x: f64, y: f64, r: f64) -> String {
    let k = 0.552_284_75 * r;
    format!(
        "{} {y} m {} {} {} {} {x} {} c {} {} {} {} {} {y} c {} {} {} {} {x} {} c {} {} {} {} {} {y} c h ",
        x + r,
        x + r,
        y + k,
        x + k,
        y + r,
        y + r,
        x - k,
        y + r,
        x - r,
        y + k,
        x - r,
        x - r,
        y - k,
        x - k,
        y - r,
        y - r,
        x + k,
        y - r,
        x + r,
        y - k,
        x + r
    )
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn fixture_set() -> Vec<(&'static str, Vec<u8>)> {
    let media = "0 0 200 160";
    let mut out = Vec::new();
    let mut add = |name: &'static str, page_extra: &str, content: String| out.push((name, pdf(media, page_extra, content.as_bytes(), &[])));

    add(
        "fill-winding",
        "",
        format!(
            "1 0 0 rg 20 20 m 50 140 l 80 20 l 5 95 l 95 95 l h f \
             0 0 1 rg 115 20 m 145 140 l 175 20 l 100 95 l 190 95 l h f* \
             0 0.6 0 rg {} 130 120 30 20 re f* \
             0.5 g {} f",
            circle(150.0, 130.0, 22.0),
            circle(40.0, 140.0, 12.5)
        ),
    );
    add(
        "stroke-anisotropic",
        "",
        "q 3 0 0 0.6 10 30 cm 0 0 1 RG 4 w 1 j 1 J 5 0 m 25 100 l 45 0 l S Q \
         q 0.5 0 0 2.5 100 20 cm 0.8 0 0 RG 6 w 0 j 0 J 0 0 m 60 30 120 -10 180 50 c S Q \
         q 2 0 0 0.25 20 130 cm 0 0.5 0 RG 8 w 2 J 0 0 m 70 0 l S Q"
            .into(),
    );
    add(
        "stroke-sheared",
        "",
        "q 1 0 0.9 1 0 0 cm 8 w 0 j 10 M 0 0.5 0 RG 10 10 m 40 110 l 70 10 l S Q \
         q 0.7071 0.7071 -0.7071 0.7071 150 30 cm 5 w 2 j 0 0 1 RG 0 0 30 50 re S Q \
         q 1 0.4 -0.3 1 100 110 cm 3 w 1 j 1 0 0 RG 0 0 m 20 30 40 -10 60 20 c 80 0 l S Q"
            .into(),
    );
    // Not conforming PDF (cm between path construction and painting): the native compiler
    // keeps the coordinates captured at construction and strokes with the painting CTM.
    add(
        "ctm-after-path",
        "",
        "0 0 1 RG 10 20 m 90 20 l 90 60 l q 4 0 0 0.5 0 0 cm 2 w S Q \
         1 0 0 RG 110 20 m 190 60 l 2 0 0 2 0 0 cm 3 w S"
            .into(),
    );
    add(
        "caps-joins",
        "",
        "10 w 0 J 20 140 m 60 140 l S 1 J 20 115 m 60 115 l S 2 J 20 90 m 60 90 l S \
         0 J 8 w 0 j 90 120 m 110 145 l 130 120 l S 1 j 140 120 m 160 145 l 180 120 l S \
         2 j 90 80 m 110 105 l 130 80 l S \
         6 w 0 j 10 M 20 20 m 60 35 l 20 50 l S 1.5 M 80 20 m 120 35 l 80 50 l S \
         0 0 1 RG 4 M 140 20 m 170 45 l 185 20 l S"
            .into(),
    );
    add(
        "dashes",
        "",
        format!(
            "4 w [12 6] 0 d 10 150 m 190 150 l S [12 6] 9 d 10 138 m 190 138 l S \
             [8] 3 d 10 126 m 190 126 l S 1 J [0 8] 0 d 10 114 m 190 114 l S 0 J \
             [20 5 5 5] 0 d 20 20 60 70 re S [6 3] 2 d 0 0 1 RG {} S \
             2 w [3 2 1 2] 1 d 100 100 m 130 60 160 120 190 70 c S",
            circle(140.0, 50.0, 30.0)
        ),
    );
    add(
        "clips-nested",
        "",
        format!(
            "q 20 20 160 120 re W n q {} 80 60 40 40 re W* n 1 0 0 rg 0 0 200 160 re f Q \
             0 0 1 rg 0 0 200 40 re f q {} W n 0 0.6 0 rg 120 0 80 160 re f Q Q \
             0.5 g 0 0 20 20 re f",
            circle(100.0, 80.0, 50.0),
            circle(170.0, 30.0, 25.0)
        ),
    );
    add(
        "rotated-cropped",
        "/CropBox [20 10 180 150] /Rotate 90",
        "0 0 1 rg 30 20 20 100 re f 30 100 70 20 re f 30 60 50 20 re f 1 0 0 RG 3 w 120 30 m 170 140 l S".into(),
    );
    add(
        "hairlines",
        "",
        "0 w 10 150 m 190 150 l S 10 140 m 190 130 l S 20 10 m 30 120 l S \
         0.1 w 10 110 m 190 100 l S 0.5 w 10 90 m 190 80 l S \
         [4 4] 0 d 0 w 10 60 m 190 60 l S 0.3 w 10 40 m 190 20 l S"
            .into(),
    );
    add(
        "colors",
        "",
        (0..5)
            .flat_map(|i| (0..4).map(move |j| (i, j)))
            .map(|(i, j)| {
                let v = f64::from(i) * 0.25;
                let (x, y) = (10.3 + f64::from(i) * 37.1, 10.7 + f64::from(j) * 36.4);
                match j {
                    0 => format!("{v} g {x} {y} 30.5 30.5 re f "),
                    1 => format!("{v} 0 {} rg {x} {y} 30.5 30.5 re f ", 1.0 - v),
                    2 => format!("0 {v} 0.5 rg {x} {y} 30.5 30.5 re f "),
                    _ => format!("{} {v} {v} rg {x} {y} 30.5 30.5 re f ", 1.0 - v),
                }
            })
            .collect(),
    );
    // A mixed vector page for timing (and a broader fidelity sample).
    let mut rng = Lcg(7);
    let mut stress = String::from("0.5 w ");
    for i in 0..3000 {
        let (x, y) = (rng.next() * 190.0, rng.next() * 150.0);
        if i % 2 == 0 {
            let _ = write!(
                stress,
                "{:.3} {:.3} {:.3} rg {x:.2} {y:.2} {:.2} {:.2} re f ",
                rng.next(),
                rng.next(),
                rng.next(),
                rng.next() * 10.0,
                rng.next() * 10.0
            );
        } else {
            let _ = write!(
                stress,
                "{:.3} {:.3} {:.3} RG {x:.2} {y:.2} m {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c S ",
                rng.next(),
                rng.next(),
                rng.next(),
                x + rng.next() * 20.0 - 10.0,
                y + rng.next() * 20.0 - 10.0,
                x + rng.next() * 20.0 - 10.0,
                y + rng.next() * 20.0 - 10.0,
                x + rng.next() * 20.0 - 10.0,
                y + rng.next() * 20.0 - 10.0
            );
        }
    }
    add("vector-stress", "", stress);
    out
}

/// Pages that use the bootstrap renderer, for timing the native attempt's overhead.
fn fallback_set() -> Vec<(&'static str, Vec<u8>)> {
    let media = "0 0 612 792";
    let font = "/Resources << /Font << /F1 5 0 R >> >>";
    let helvetica = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec();
    let mut text = String::from("BT /F1 10 Tf 12 TL 40 760 Td ");
    for i in 0..60 {
        let _ = write!(text, "(Line {i}: the quick brown fox jumps over the lazy dog, again and again.) ' ");
    }
    text.push_str("ET");
    let mut big = String::from("BT /F1 4 Tf 5 TL 10 785 Td ");
    while big.len() < 900_000 {
        big.push_str("(abcdefghijklmnopqrstuvwxyz0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ abcdefghijklmnopqrstuvwxyz) ' ");
    }
    big.push_str("ET");
    // A large vector page that turns out to need the bootstrap renderer only at its end.
    let mut late = String::from("0.3 w ");
    let mut rng = Lcg(11);
    while late.len() < 900_000 {
        let _ = write!(late, "{:.2} {:.2} m {:.2} {:.2} l S ", rng.next() * 600.0, rng.next() * 780.0, rng.next() * 600.0, rng.next() * 780.0);
    }
    late.push_str("BT /F1 12 Tf 40 40 Td (late text) Tj ET");
    let (w, h) = (800usize, 800usize);
    let pixels: Vec<u8> = (0..w * h).flat_map(|i| [(i % w * 255 / w) as u8, (i / w * 255 / h) as u8, 128]).collect();
    let image = stream(
        &format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode"),
        &zlib(&pixels),
    );
    let flate = |content: &str| -> Vec<u8> {
        let data = zlib(content.as_bytes());
        let mut out = format!("<< /Length {} /Filter /FlateDecode >>\nstream\n", data.len()).into_bytes();
        out.extend_from_slice(&data);
        out.extend_from_slice(b"\nendstream");
        out
    };
    // Flate-compressed variants replace object 4 by pointing /Contents at object 6.
    let compressed = |content: &str, resources: &str, extra: Vec<u8>| -> Vec<u8> {
        pdf(media, &format!("{resources} /Contents 6 0 R"), b"", &[extra, flate(content)])
    };
    vec![
        ("fb-text-page", pdf(media, font, text.as_bytes(), std::slice::from_ref(&helvetica))),
        ("fb-text-900k-flate", compressed(&big, font, helvetica.clone())),
        ("fb-late-text-900k-flate", compressed(&late, font, helvetica.clone())),
        (
            "fb-image-page",
            pdf(media, "/Resources << /XObject << /Im1 5 0 R >> >>", b"q 500 0 0 500 56 146 cm /Im1 Do Q 0 0 1 rg 56 100 500 30 re f", &[image]),
        ),
    ]
}

fn fixtures(dir: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(dir)?;
    for (name, bytes) in fixture_set().into_iter().chain(fallback_set()) {
        std::fs::write(dir.join(format!("{name}.pdf")), bytes)?;
    }
    // The shrinking-pen hazard, for timing the bootstrap renderer separately (see `one`).
    let mut pen = String::from("0.000001 0 0 0.000001 0 0 cm 1000000000 w 1 j 1 J 10000000 10000000 m ");
    for i in 0..400 {
        let _ = write!(pen, "{} {} l ", 10_000_000 + (i % 2) * 70_000_000, 10_000_000 + i * 150_000);
    }
    pen.push('S');
    std::fs::write(dir.join("hazard-shrinking-pen.pdf.bin"), pdf("0 0 200 160", "", pen.as_bytes(), &[]))?;
    Ok(())
}

// ---------------------------------------------------------------- rendering

struct Image {
    w: usize,
    h: usize,
    /// RGB8 (the renders are on opaque white, so premultiplied RGBA is plain RGB).
    rgb: Vec<u8>,
}

fn render(bytes: &Arc<Vec<u8>>, scale: f32, bootstrap: bool, tile: Option<Tile>) -> Result<(Image, bool), Error> {
    let mut renderer = PageRenderer::new(bytes.clone(), RenderConfig { native_vector: !bootstrap, ..Default::default() });
    let page = renderer.render(RenderRequest { scale, tile, ..Default::default() });
    if let Some(e) = page.error {
        return Err(e.into());
    }
    let rgb = page.rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    Ok((Image { w: page.width as usize, h: page.height as usize, rgb }, renderer.stats().native_vector_rasterizations > 0))
}

fn read_ppm(path: &Path) -> Result<Image, Error> {
    let data = std::fs::read(path)?;
    let mut fields = Vec::new();
    let mut at = 0;
    while fields.len() < 4 {
        while data.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if data.get(at) == Some(&b'#') {
            while data.get(at).is_some_and(|b| *b != b'\n') {
                at += 1;
            }
            continue;
        }
        let start = at;
        while data.get(at).is_some_and(|b| !b.is_ascii_whitespace()) {
            at += 1;
        }
        fields.push(String::from_utf8_lossy(&data[start..at]).into_owned());
    }
    if fields[0] != "P6" || fields[3] != "255" {
        return Err(format!("{}: not an 8-bit P6 PPM", path.display()).into());
    }
    let (w, h): (usize, usize) = (fields[1].parse()?, fields[2].parse()?);
    let rgb = data.get(at + 1..at + 1 + w * h * 3).ok_or("short PPM")?.to_vec();
    Ok(Image { w, h, rgb })
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { 0xedb8_8320 ^ (crc >> 1) } else { crc >> 1 };
        }
    }
    !crc
}

fn write_png(path: &Path, image: &Image) -> Result<(), Error> {
    let mut raw = Vec::with_capacity(image.h * (image.w * 3 + 1));
    for row in image.rgb.chunks_exact(image.w * 3) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], body: &[u8]| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut tagged = kind.to_vec();
        tagged.extend_from_slice(body);
        out.extend_from_slice(&tagged);
        out.extend_from_slice(&crc32(&tagged).to_be_bytes());
    };
    let mut header = (image.w as u32).to_be_bytes().to_vec();
    header.extend_from_slice(&(image.h as u32).to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &zlib(&raw));
    chunk(b"IEND", &[]);
    std::fs::write(path, out)?;
    Ok(())
}

// ---------------------------------------------------------------- comparison

struct Metrics {
    max: u8,
    mean: f64,
    /// Pixels where any channel differs at all.
    differing: f64,
    /// Pixels where a channel differs by more than 64.
    large: f64,
    /// Like `large`, but a pixel matches if any pixel of the other image within one pixel
    /// (3 × 3) is within 64: tolerates edges placed up to one pixel apart by antialiasing.
    shifted_large: f64,
}

fn delta(a: &Image, b: &Image, i: usize, j: usize) -> u8 {
    (0..3).map(|c| a.rgb[i * 3 + c].abs_diff(b.rgb[j * 3 + c])).max().unwrap_or(0)
}

fn shifted(a: &Image, b: &Image, w: usize, h: usize, x: usize, y: usize) -> u8 {
    let mut best = u8::MAX;
    for dy in -1i64..=1 {
        for dx in -1i64..=1 {
            let (bx, by) = (x as i64 + dx, y as i64 + dy);
            if bx >= 0 && by >= 0 && (bx as usize) < w && (by as usize) < h {
                best = best.min(delta(a, b, y * a.w + x, by as usize * b.w + bx as usize));
            }
        }
    }
    best
}

fn metrics(a: &Image, b: &Image) -> (Metrics, Image) {
    let (w, h) = (a.w.min(b.w), a.h.min(b.h));
    let (mut max, mut sum, mut differing, mut large, mut shifted_large) = (0u8, 0u64, 0usize, 0usize, 0usize);
    let mut diff = Image { w, h, rgb: vec![255; w * h * 3] };
    for y in 0..h {
        for x in 0..w {
            let d = delta(a, b, y * a.w + x, y * b.w + x);
            max = max.max(d);
            sum += (0..3).map(|c| u64::from(a.rgb[(y * a.w + x) * 3 + c].abs_diff(b.rgb[(y * b.w + x) * 3 + c]))).sum::<u64>();
            let o = (y * w + x) * 3;
            // Context: the first image, faded; differences in red, darker for larger ones.
            let lum = a.rgb[(y * a.w + x) * 3..(y * a.w + x) * 3 + 3].iter().map(|v| u32::from(*v)).sum::<u32>() / 3;
            let faded = (192 + lum / 4) as u8;
            diff.rgb[o..o + 3].copy_from_slice(&[faded; 3]);
            if d > 0 {
                differing += 1;
            }
            if d > 64 {
                large += 1;
                let s = shifted(a, b, w, h, x, y).max(shifted(b, a, w, h, x, y));
                if s > 64 {
                    shifted_large += 1;
                    diff.rgb[o..o + 3].copy_from_slice(&[255, 0, 0]);
                } else {
                    diff.rgb[o..o + 3].copy_from_slice(&[255, 160, 0]);
                }
            } else if d > 16 {
                diff.rgb[o..o + 3].copy_from_slice(&[255, 220, 120]);
            }
        }
    }
    let n = (w * h).max(1) as f64;
    (
        Metrics {
            max,
            mean: sum as f64 / (n * 3.0),
            differing: differing as f64 * 100.0 / n,
            large: large as f64 * 100.0 / n,
            shifted_large: shifted_large as f64 * 100.0 / n,
        },
        diff,
    )
}

fn compare(dir: &Path, scale: f32) -> Result<(), Error> {
    let mut report = String::from("fixture\tnative_used\tsize_native\tsize_poppler\tpair\tmax\tmean\tpct_differing\tpct_gt64\tpct_gt64_shift1\n");
    for (name, _) in fixture_set() {
        let bytes = Arc::new(std::fs::read(dir.join(format!("{name}.pdf")))?);
        let (native, used) = render(&bytes, scale, false, None)?;
        let (bootstrap, _) = render(&bytes, scale, true, None)?;
        let poppler = read_ppm(&dir.join(format!("{name}-poppler.ppm")))?;
        write_png(&dir.join(format!("{name}-native.png")), &native)?;
        write_png(&dir.join(format!("{name}-bootstrap.png")), &bootstrap)?;
        write_png(&dir.join(format!("{name}-poppler.png")), &poppler)?;
        // Tiles stitched together must equal the whole native render exactly.
        let mut stitched = Image { w: native.w, h: native.h, rgb: vec![0; native.rgb.len()] };
        let (tw, th) = (native.w.div_ceil(3) as u32, native.h.div_ceil(2) as u32);
        for ty in 0..2u32 {
            for tx in 0..3u32 {
                let tile = Tile { x: tx * tw, y: ty * th, w: tw.min(native.w as u32 - tx * tw), h: th.min(native.h as u32 - ty * th) };
                let (part, _) = render(&bytes, scale, false, Some(tile))?;
                for y in 0..part.h {
                    let row = (tile.y as usize + y) * native.w + tile.x as usize;
                    stitched.rgb[row * 3..(row + part.w) * 3].copy_from_slice(&part.rgb[y * part.w * 3..(y + 1) * part.w * 3]);
                }
            }
        }
        for (pair, a, b) in [
            ("native-poppler", &native, &poppler),
            ("bootstrap-poppler", &bootstrap, &poppler),
            ("native-bootstrap", &native, &bootstrap),
            ("native-tiles", &native, &stitched),
        ] {
            let (m, diff) = metrics(a, b);
            if pair != "native-tiles" {
                write_png(&dir.join(format!("{name}-diff-{pair}.png")), &diff)?;
            }
            let _ = writeln!(
                report,
                "{name}\t{used}\t{}x{}\t{}x{}\t{pair}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}",
                native.w, native.h, poppler.w, poppler.h, m.max, m.mean, m.differing, m.large, m.shifted_large
            );
        }
    }
    print!("{report}");
    std::fs::write(dir.join("compare.tsv"), report)?;
    Ok(())
}

// ---------------------------------------------------------------- timing

fn percentile(values: &mut [f64], p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * p).round() as usize]
}

fn bench(dir: &Path, reps: usize) -> Result<(), Error> {
    let mut report = String::from("fixture\tnative_used\tmode\tcold_median_ms\tcold_p10_ms\tcold_p90_ms\twarm_median_ms\n");
    let names: Vec<_> = fixture_set()
        .into_iter()
        .chain(fallback_set())
        .map(|(n, _)| n)
        .filter(|n| n.starts_with("fb-") || *n == "vector-stress" || *n == "dashes")
        .collect();
    for name in names {
        let bytes = Arc::new(std::fs::read(dir.join(format!("{name}.pdf")))?);
        let mut times: [(Vec<f64>, Vec<f64>); 2] = Default::default();
        let mut used = false;
        for _ in 0..reps {
            // Interleaved so that load from other processes affects both modes alike.
            for (mode, bootstrap) in [false, true].into_iter().enumerate() {
                let mut renderer = PageRenderer::new(bytes.clone(), RenderConfig { native_vector: !bootstrap, ..Default::default() });
                let start = Instant::now();
                let page = renderer.render(RenderRequest { scale: 2.0, ..Default::default() });
                let cold = start.elapsed().as_secs_f64() * 1000.0;
                let start = Instant::now();
                let again = renderer.render(RenderRequest { scale: 2.0, ..Default::default() });
                let warm = start.elapsed().as_secs_f64() * 1000.0;
                if page.error.is_some() || again.error.is_some() {
                    return Err(format!("{name}: {:?}", page.error.or(again.error)).into());
                }
                if !bootstrap {
                    used = renderer.stats().native_vector_rasterizations > 0;
                }
                times[mode].0.push(cold);
                times[mode].1.push(warm);
            }
        }
        for (mode, (cold, warm)) in times.iter_mut().enumerate() {
            let label = if mode == 0 { "native-enabled" } else { "forced-bootstrap" };
            let _ = writeln!(
                report,
                "{name}\t{used}\t{label}\t{:.2}\t{:.2}\t{:.2}\t{:.2}",
                percentile(cold, 0.5),
                percentile(cold, 0.1),
                percentile(cold, 0.9),
                percentile(warm, 0.5)
            );
        }
    }
    print!("{report}");
    std::fs::write(dir.join(format!("bench-{}.tsv", if cfg!(debug_assertions) { "debug" } else { "release" })), report)?;
    Ok(())
}

// ---------------------------------------------------------------- real-world survey

/// Collects the native path's fallback reasons (its `log::debug!` records).
struct ReasonLog;

static REASONS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

impl log::Log for ReasonLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("pdfcraft_render")
    }
    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let message = record.args().to_string();
        if let Some((_, reason)) = message.split_once("uses the bootstrap renderer: ") {
            let stage = if message.contains("raster uses") { "raster" } else { "compile" };
            REASONS.lock().unwrap().push(format!("{stage}: {reason}"));
        } else if message.contains("native vector rendering crashed") {
            REASONS.lock().unwrap().push(format!("crash: {message}"));
        }
    }
    fn flush(&self) {}
}

fn take_reasons() -> String {
    std::mem::take(&mut *REASONS.lock().unwrap()).join("; ")
}

/// One real-world PDF, page by page (at most `max_pages`), as TSV on standard output:
/// a 1×1-pixel tile through the opted-in renderer decides whether the native path compiles
/// the page (cheap even for pages that fall back). Pages it accepts are then rendered whole
/// at `scale` both natively and through the bootstrap renderer and compared; the first three
/// pages that fall back are also timed both ways (the cost of the failed native attempt).
/// The caller runs one process per file under a time limit.
fn survey(path: &Path, scale: f32, max_pages: usize) -> Result<(), Error> {
    let _ = log::set_logger(&ReasonLog);
    log::set_max_level(log::LevelFilter::Debug);
    let bytes = Arc::new(std::fs::read(path)?);
    let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut probe = PageRenderer::new(bytes.clone(), RenderConfig { native_vector: true, ..Default::default() });
    let pages = probe.page_count();
    let mut out = std::io::stdout().lock();
    if pages == 0 {
        writeln!(out, "{file}\t0\topen-failed\t\t\t\t\t\t\t\t")?;
        return Ok(());
    }
    let mut timed_fallbacks = 0;
    for page in 0..pages.min(max_pages) {
        let _ = take_reasons();
        let before = probe.stats().native_vector_rasterizations;
        let tile = probe.render(RenderRequest { page, scale, tile: Some(Tile { x: 0, y: 0, w: 1, h: 1 }), ..Default::default() });
        let accepted = probe.stats().native_vector_rasterizations > before;
        let reason = take_reasons();
        if let Some(e) = tile.error {
            writeln!(out, "{file}\t{}\terror\t{}\t\t\t\t\t\t\t", page + 1, e.replace(['\t', '\n'], " "))?;
            continue;
        }
        if !accepted && timed_fallbacks >= 3 {
            writeln!(out, "{file}\t{}\tfallback\t{reason}\t\t\t\t\t\t\t", page + 1)?;
            continue;
        }
        // Whole renders with fresh renderers, so both pay their own first-render costs.
        let time = |native: bool| -> Result<(Image, bool, f64, Option<String>), Error> {
            let mut r = PageRenderer::new(bytes.clone(), RenderConfig { native_vector: native, ..Default::default() });
            let start = Instant::now();
            let p = r.render(RenderRequest { page, scale, ..Default::default() });
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            let rgb = p.rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
            Ok((Image { w: p.width as usize, h: p.height as usize, rgb }, r.stats().native_vector_rasterizations > 0, ms, p.error))
        };
        let (native, used, native_ms, native_error) = time(true)?;
        let reason = if reason.is_empty() { take_reasons() } else { reason };
        let (bootstrap, _, bootstrap_ms, bootstrap_error) = time(false)?;
        let status = match (accepted, used) {
            (true, true) => "native",
            (true, false) => "native-tile-only",
            (false, true) => "native-whole-only",
            (false, false) => {
                timed_fallbacks += 1;
                "fallback-timed"
            }
        };
        if native_error.is_some() || bootstrap_error.is_some() {
            writeln!(out, "{file}\t{}\t{status}-error\t{reason} native={native_error:?} bootstrap={bootstrap_error:?}\t\t\t\t\t\t\t", page + 1)?;
            continue;
        }
        let (m, _) = metrics(&native, &bootstrap);
        writeln!(
            out,
            "{file}\t{}\t{status}\t{reason}\t{native_ms:.2}\t{bootstrap_ms:.2}\t{}x{}\t{}\t{:.4}\t{:.4}\t{:.4}",
            page + 1,
            native.w,
            native.h,
            m.max,
            m.differing,
            m.large,
            m.shifted_large
        )?;
    }
    Ok(())
}

fn one(path: &Path, mode: &str) -> Result<(), Error> {
    let bytes = Arc::new(std::fs::read(path)?);
    let start = Instant::now();
    let (_, used) = render(&bytes, 2.0, mode == "bootstrap", None)?;
    println!("{}\t{mode}\tnative_used={used}\t{:.1} ms", path.display(), start.elapsed().as_secs_f64() * 1000.0);
    let _ = PathBuf::new();
    Ok(())
}

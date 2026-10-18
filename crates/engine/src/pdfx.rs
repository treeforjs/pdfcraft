//! Preflight ▸ Convert to PDF/X: every fix PDF/X-1a, PDF/X-3 or PDF/X-4 needs, as one undoable
//! edit ([`crate::Edit::ConvertPdfX`]), then the PDF/X verification again.
//!
//! The steps (see `pdfcraft_preflight::pdfx_convert` for the document-level ones): forbidden
//! elements removed and standard fonts embedded ([`pdfcraft_preflight::pdfx_prepare`]),
//! printing annotations and form fields flattened ([`pdfcraft_edit::flatten`]), transparency
//! flattened for PDF/X-1a and PDF/X-3 ([`pdfcraft_preflight::flatten_transparency`]), colour
//! converted to the output intent ([`pdfcraft_edit::convert_colors`], through the output profile
//! when it has conversion tables, written as device colour), and the output intent, page boxes,
//! identification and metadata ([`pdfcraft_preflight::pdfx_finish`]).
//!
//! [`Session::pdfx_convert`] runs the steps on a copy first. When the copy doesn't conform it
//! changes nothing and says why (unless `allow_partial`); when no step changes anything the
//! document is left alone (no undo step), so converting twice is harmless.

use pdfcraft_preflight::{XConvertOptions, XFinding, XOptions, XPart, XReport, XSeverity, verify_pdfx};

use crate::{ColorConversion, ColorDestination, DocId, Edit, EditError, Session, guard};

/// What [`Session::pdfx_convert`] did.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfxConversion {
    pub part: XPart,
    /// The verification before and after (after: of the converted document, or of the trial
    /// copy when the conversion was refused).
    pub before: XReport,
    pub after: XReport,
    /// What the conversion changed (empty: nothing needed changing).
    pub fixed: Vec<String>,
    /// What a step left as it was, and why (images Convert Colors can't decode, an output
    /// profile without conversion tables, …).
    pub notes: Vec<String>,
    /// The document was changed (or already conformed); false when refused.
    pub converted: bool,
}

impl PdfxConversion {
    /// The errors left after the conversion: what can't be fixed automatically.
    pub fn unfixable(&self) -> Vec<&XFinding> {
        self.after.findings.iter().filter(|f| f.severity == XSeverity::Error).collect()
    }
}

/// Run every step on `doc`; returns (fixed, notes).
pub(crate) fn run(doc: &mut pdfcraft_cos::Document, opts: &XConvertOptions, date: &str) -> Result<(Vec<String>, Vec<String>), EditError> {
    opts.validate(doc).map_err(EditError::Invalid)?;
    let mut notes = Vec::new();
    // Read the intent before the steps change the catalog.
    let profile = opts.intent_profile(doc);
    let sources = opts.colour_plan(doc);
    let mut fixed = pdfcraft_preflight::pdfx_prepare(doc, opts).map_err(EditError::Invalid)?;
    let all: Vec<usize> = (0..pdfcraft_model::pages(doc).len()).collect();
    let flattened = pdfcraft_edit::flatten(doc, &all, true, true)?;
    if flattened > 0 {
        fixed.push(format!("Flattened {flattened} printing annotations and form fields into the page content"));
    }
    if !opts.part.allows_transparency() {
        let pages = pdfcraft_preflight::transparent_pages(doc, &[]);
        if !pages.is_empty() {
            let r = pdfcraft_preflight::flatten_transparency(doc, &pages, &opts.flatten).map_err(EditError::Invalid)?;
            fixed.push(format!("Flattened transparency on {} pages", r.pages.len()));
        }
    }
    if !sources.is_empty() && !all.is_empty() {
        let mut spaces = pdfcraft_edit::SourceSpaces::NONE;
        for s in &sources {
            spaces.set(s);
        }
        let mut settings = ColorConversion { sources: spaces, ..ColorConversion::default() };
        let space = opts.intent_space(doc);
        settings.destination = match &profile {
            Some(p) => match pdfcraft_edit::convert_colors::Profile::parse(p, 1) {
                Ok(parsed) if parsed.exact() => {
                    settings.device_output = true;
                    ColorDestination::Icc(p.clone())
                }
                Ok(_) | Err(_) => {
                    notes.push(
                        "The output profile has no conversion tables PdfCraft can use: colours were converted with the built-in approximation".into(),
                    );
                    device(space)
                }
            },
            None => {
                notes.push("No output profile: colours were converted with the built-in approximation of the printing condition".into());
                device(space)
            }
        };
        let r = pdfcraft_edit::convert_colors(doc, &all, &settings)?;
        if r.changed() {
            let what = match &settings.destination {
                ColorDestination::DeviceGray => "DeviceGray",
                ColorDestination::DeviceRgb => "DeviceRGB",
                _ if &space == b"GRAY" => "DeviceGray",
                _ => "DeviceCMYK",
            };
            fixed.push(format!(
                "Converted {} colours, {} images, {} shadings and {} forms from {} to {what} on {} pages",
                r.colors,
                r.images + r.inline_images,
                r.shadings,
                r.forms + r.patterns,
                sources.join(", "),
                r.pages.len()
            ));
        } else {
            // Nothing to convert: notes about the approximation don't apply.
            notes.clear();
        }
        notes.extend(r.skipped.iter().map(|s| format!("Colour conversion left {s} as they were")));
    }
    fixed.extend(pdfcraft_preflight::pdfx_finish(doc, opts, date).map_err(EditError::Invalid)?);
    Ok((fixed, notes))
}

fn device(space: [u8; 4]) -> ColorDestination {
    match &space {
        b"GRAY" => ColorDestination::DeviceGray,
        b"RGB " => ColorDestination::DeviceRgb,
        _ => ColorDestination::DeviceCmyk,
    }
}

fn verify(doc: &pdfcraft_cos::Document, part: XPart) -> XReport {
    verify_pdfx(doc, &XOptions { part: Some(part), max_findings: 100 })
}

impl Session {
    /// Preflight ▸ Convert to PDF/X: make the document conform to `options.part` as one undo
    /// step. Bad options are an error; a conversion that wouldn't reach conformance changes
    /// nothing and returns `converted: false` with the remaining errors in `after` (unless
    /// `options.allow_partial`).
    pub fn pdfx_convert(&mut self, id: DocId, options: XConvertOptions) -> Result<PdfxConversion, EditError> {
        let date = self.now().map(pdfcraft_cos::pdf_date).unwrap_or_else(|| "D:19700101000000Z".into());
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let editor = doc.editor.as_ref().ok_or_else(|| EditError::ReadOnly(doc.read_only_reason.clone().unwrap_or_default()))?;
        options.validate(&editor.cos).map_err(EditError::Invalid)?;
        let part = options.part;
        let before = guard(|| verify(&editor.cos, part)).map_err(EditError::Invalid)?;
        let mut probe = editor.cos.clone();
        let (fixed, notes) = guard(|| run(&mut probe, &options, &date))
            .unwrap_or_else(|m| Err(EditError::Invalid(format!("Convert to PDF/X failed unexpectedly ({m}); the document was not changed"))))?;
        let after = guard(|| verify(&probe, part)).map_err(EditError::Invalid)?;
        drop(probe);
        if !after.conforms() && !options.allow_partial {
            return Ok(PdfxConversion { part, before, after, fixed, notes, converted: false });
        }
        if fixed.is_empty() {
            return Ok(PdfxConversion { part, before, after, fixed, notes, converted: true });
        }
        self.apply(id, Edit::ConvertPdfX { options: Box::new(options) })?;
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let after = doc.editor.as_ref().map(|e| verify(&e.cos, part)).unwrap_or(after);
        Ok(PdfxConversion { part, before, after, fixed, notes, converted: true })
    }
}

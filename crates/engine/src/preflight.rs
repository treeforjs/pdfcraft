//! Revision-bound structural preflight and explicit, undoable fixes.

use crate::{DocId, Edit, EditError, Session};
pub use pdfcraft_preflight::analysis::{Finding, Fixup, Location, Profile, Report, Rule, Settings, Severity};
pub use pdfcraft_preflight::colour_warnings::{
    Colour as WarningColour, Finding as ColourFinding, Kind as ColourWarningKind, Options as ColourWarningOptions, Report as ColourWarnings,
};
use pdfcraft_preflight::coverage::{BuiltIn, Conversion};
pub use pdfcraft_preflight::coverage::{Options as CoverageOptions, PageCoverage, Region as CoverageRegion};
pub use pdfcraft_preflight::flatten::{MAX_RESOLUTION as RASTER_MAX_RESOLUTION, MIN_RESOLUTION as RASTER_MIN_RESOLUTION};
pub use pdfcraft_preflight::separations::{InkInfo, InkList, InkUse, OutputIntent, Raster as Separations, Sample as InkSample};

#[derive(Clone, Debug)]
pub struct ResultSet {
    pub document: DocId,
    pub revision: u64,
    pub report: Report,
}

impl crate::Document {
    pub fn preflight(&self, settings: &Settings) -> Result<ResultSet, EditError> {
        let editor = self.editor.as_ref().ok_or_else(|| EditError::ReadOnly(self.read_only_reason.clone().unwrap_or_default()))?;
        let report = crate::guard(|| pdfcraft_preflight::analysis::analyze(&editor.cos, settings))
            .map_err(|e| EditError::Invalid(format!("Preflight failed: {e}")))?
            .map_err(EditError::Invalid)?;
        Ok(ResultSet { document: self.id, revision: self.generation, report })
    }

    /// Total area coverage (Output Preview): the most ink each of `pages` (0-based) puts down
    /// and where it exceeds `options.limit`. RGB and Lab colours convert through
    /// [`Self::press_conversion`]. Read-only; see [`pdfcraft_preflight::coverage`] for what is
    /// exact, what is converted and what is not measured.
    pub fn ink_coverage(&self, pages: &[usize], options: &CoverageOptions) -> Result<Vec<PageCoverage>, EditError> {
        options.validate().map_err(EditError::Invalid)?;
        let editor = self.editor.as_ref().ok_or_else(|| EditError::ReadOnly(self.read_only_reason.clone().unwrap_or_default()))?;
        let conversion = press_conversion(&editor.cos);
        pages
            .iter()
            .map(|&page| {
                crate::guard(|| pdfcraft_preflight::coverage::page_coverage_with(&editor.cos, page, options, conversion.as_ref()))
                    .map_err(|e| EditError::Invalid(format!("Ink coverage failed on page {}: {e}", page.saturating_add(1))))?
                    .map_err(EditError::Invalid)
            })
            .collect()
    }

    /// How this document's RGB and Lab colours become process inks: its output intent's
    /// CMYK profile (relative colorimetric) when it has a usable one, else the built-in
    /// conversion, whose description says why.
    pub fn press_conversion(&self) -> Result<Box<dyn Conversion>, EditError> {
        let editor = self.editor.as_ref().ok_or_else(|| EditError::ReadOnly(self.read_only_reason.clone().unwrap_or_default()))?;
        Ok(press_conversion(&editor.cos))
    }

    /// Output Preview's separations list: the process and spot inks of `pages` (0-based; all
    /// when empty) and the pages each is painted and declared on. Read-only.
    pub fn separation_inks(&self, pages: &[usize]) -> Result<InkList, EditError> {
        let editor = self.editor.as_ref().ok_or_else(|| EditError::ReadOnly(self.read_only_reason.clone().unwrap_or_default()))?;
        let conversion = press_conversion(&editor.cos);
        crate::guard(|| pdfcraft_preflight::separations::list_inks(&editor.cos, pages, conversion.as_ref()))
            .map_err(|e| EditError::Invalid(format!("Listing the separations failed: {e}")))?
            .map_err(EditError::Invalid)
    }

    /// 0-based `page` separated into one plate per ink at `dpi` (lowered on large pages or with
    /// many inks), simulating overprint when `overprint` is set. Read-only.
    pub fn separations(&self, page: usize, dpi: f64, overprint: bool) -> Result<Separations, EditError> {
        let editor = self.editor.as_ref().ok_or_else(|| EditError::ReadOnly(self.read_only_reason.clone().unwrap_or_default()))?;
        let conversion = press_conversion(&editor.cos);
        crate::guard(|| pdfcraft_preflight::separations::rasterize(&editor.cos, page, dpi, overprint, conversion.as_ref()))
            .map_err(|e| EditError::Invalid(format!("Separating page {} failed: {e}", page.saturating_add(1))))?
            .map_err(EditError::Invalid)
    }

    /// Output Preview's overprint, rich-black and registration-colour warnings for
    /// `options.pages` (0-based; all when empty). Read-only; see
    /// [`pdfcraft_preflight::colour_warnings`] for what is classified and what isn't.
    pub fn colour_warnings(&self, options: &ColourWarningOptions) -> Result<ColourWarnings, EditError> {
        options.validate().map_err(EditError::Invalid)?;
        let editor = self.editor.as_ref().ok_or_else(|| EditError::ReadOnly(self.read_only_reason.clone().unwrap_or_default()))?;
        crate::guard(|| pdfcraft_preflight::colour_warnings::check(&editor.cos, options))
            .map_err(|e| EditError::Invalid(format!("The colour warning check failed: {e}")))?
            .map_err(EditError::Invalid)
    }
}

/// RGB and Lab to CMYK through an output intent's ICC profile.
struct ProfileConversion {
    profile: pdfcraft_edit::convert_colors::Profile,
    label: String,
}

impl Conversion for ProfileConversion {
    fn describe(&self) -> String {
        self.label.clone()
    }
    fn uses_profile(&self) -> bool {
        true
    }
    fn rgb(&self, rgb: [f64; 3]) -> [f64; 4] {
        self.profile.cmyk_from_srgb(rgb).unwrap_or_else(|| BuiltIn.rgb(rgb))
    }
    fn lab(&self, lab: [f64; 3]) -> [f64; 4] {
        self.profile.cmyk_from_lab(lab).unwrap_or_else(|| BuiltIn.lab(lab))
    }
}

/// The built-in conversion, saying why the output intent (if any) isn't used.
struct Fallback(String);

impl Conversion for Fallback {
    fn describe(&self) -> String {
        format!("{} ({})", pdfcraft_preflight::coverage::BUILT_IN, self.0)
    }
    fn rgb(&self, rgb: [f64; 3]) -> [f64; 4] {
        BuiltIn.rgb(rgb)
    }
    fn lab(&self, lab: [f64; 3]) -> [f64; 4] {
        BuiltIn.lab(lab)
    }
}

fn press_conversion(doc: &pdfcraft_cos::Document) -> Box<dyn Conversion> {
    use pdfcraft_edit::convert_colors::{Family, Profile};
    let Some(intent) = pdfcraft_preflight::separations::output_intent(doc) else {
        return Box::new(Fallback("the document has no output intent".into()));
    };
    let name = match &intent.condition {
        Some(c) => format!("{} output intent {c:?}", intent.subtype),
        None => format!("{} output intent", intent.subtype),
    };
    let Some(bytes) = intent.profile else {
        let why = intent.problem.unwrap_or_else(|| "it has no DestOutputProfile".into());
        return Box::new(Fallback(format!("the {name} is not used: {why}")));
    };
    match Profile::parse(&bytes, 1) {
        Ok(p) if p.family == Family::Cmyk && p.exact() => {
            let described = if p.description.is_empty() { String::new() } else { format!(" ({})", p.description) };
            Box::new(ProfileConversion { label: format!("the {name}'s CMYK profile{described}, relative colorimetric"), profile: p })
        }
        Ok(p) if p.family != Family::Cmyk => Box::new(Fallback(format!("the {name}'s profile is not a CMYK profile"))),
        Ok(_) => Box::new(Fallback(format!("the {name}'s profile has no BToA tables PdfCraft can use"))),
        Err(e) => Box::new(Fallback(format!("the {name}'s profile could not be read: {e}"))),
    }
}

impl Session {
    /// Apply exactly the findings selected from this document version. No selection, an
    /// invalid selection, changed document, or denied permissions leaves history untouched.
    pub fn preflight_fix(&mut self, result: &ResultSet, findings: &[usize]) -> Result<ResultSet, EditError> {
        let doc = self.get(result.document).ok_or(EditError::NoDocument)?;
        if doc.generation != result.revision {
            return Err(EditError::Invalid("Preflight results are stale; analyze the document again".into()));
        }
        let fixes = result.report.fixes(findings).map_err(EditError::Invalid)?;
        self.apply(result.document, Edit::PreflightFix { fixes })?;
        self.get(result.document).ok_or(EditError::NoDocument)?.preflight(&result.report.settings)
    }
}

#[cfg(test)]
mod tests;

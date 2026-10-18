//! Revision-bound structural preflight and explicit, undoable fixes.

use crate::{DocId, Edit, EditError, Session};
pub use pdfcraft_preflight::analysis::{Finding, Fixup, Location, Profile, Report, Rule, Settings, Severity};
pub use pdfcraft_preflight::colour_warnings::{
    Colour as WarningColour, Finding as ColourFinding, Kind as ColourWarningKind, Options as ColourWarningOptions, Report as ColourWarnings,
};
use std::collections::{BTreeMap, BTreeSet};

use pdfcraft_preflight::coverage::{BuiltIn, Conversion, TintFunction};
pub use pdfcraft_preflight::coverage::{Options as CoverageOptions, PageCoverage, Region as CoverageRegion};
pub use pdfcraft_preflight::flatten::{MAX_RESOLUTION as RASTER_MAX_RESOLUTION, MIN_RESOLUTION as RASTER_MIN_RESOLUTION};
use pdfcraft_preflight::ink_manager::Managed;
pub use pdfcraft_preflight::ink_manager::{InkManager, Route as InkRoute};
pub use pdfcraft_preflight::inspector::{
    AnnotationInfo, FormInfo, ImageInfo, Inspected, Inspection, ObjectColour, ObjectKind, Overprint as ObjectOverprint, ShadingInfo, TextInfo,
    Transparency,
};
pub use pdfcraft_preflight::overprint::{
    DEFAULT_MAX_OBJECTS as OVERPRINT_DEFAULT_MAX_OBJECTS, DEFAULT_OBJECTS_DPI as OVERPRINT_DEFAULT_DPI, MAX_OBJECTS as OVERPRINT_MAX_OBJECTS,
    Mode as OverprintPreview, OverprintObject, OverprintObjects, Simulation as OverprintSimulation,
};
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
        let base = self.press(&editor.cos);
        let conversion = Managed { conversion: base.as_ref(), inks: &self.ink_manager };
        pages
            .iter()
            .map(|&page| {
                crate::guard(|| pdfcraft_preflight::coverage::page_coverage_with(&editor.cos, page, options, &conversion))
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
        Ok(self.press(&editor.cos))
    }

    /// RGB and Lab to process inks under the colour settings, displayed (overprint preview)
    /// through the renderer's resolved CMYK conversion.
    fn press(&self, cos: &pdfcraft_cos::Document) -> Box<dyn Conversion> {
        Box::new(Displayed { inner: press_conversion(cos, &self.color_management), display: self.color.render.clone() })
    }

    /// Output Preview's separations list: the process and spot inks of `pages` (0-based; all
    /// when empty) and the pages each is painted and declared on, with the session's
    /// [`Self::ink_manager`] settings applied (aliased and converted inks print elsewhere).
    /// Read-only.
    pub fn separation_inks(&self, pages: &[usize]) -> Result<InkList, EditError> {
        self.list_inks(pages, true)
    }

    /// The document's own inks on `pages` (0-based; all when empty), ignoring the Ink Manager:
    /// what the Ink Manager lists. Read-only.
    pub fn document_inks(&self, pages: &[usize]) -> Result<InkList, EditError> {
        self.list_inks(pages, false)
    }

    fn list_inks(&self, pages: &[usize], managed: bool) -> Result<InkList, EditError> {
        let editor = self.editor.as_ref().ok_or_else(|| EditError::ReadOnly(self.read_only_reason.clone().unwrap_or_default()))?;
        let base = self.press(&editor.cos);
        let none = InkManager::default();
        let conversion = Managed { conversion: base.as_ref(), inks: if managed { &self.ink_manager } else { &none } };
        crate::guard(|| pdfcraft_preflight::separations::list_inks(&editor.cos, pages, &conversion))
            .map_err(|e| EditError::Invalid(format!("Listing the separations failed: {e}")))?
            .map_err(EditError::Invalid)
    }

    /// The session's Ink Manager settings for this document (not saved with it).
    pub fn ink_manager(&self) -> &InkManager {
        &self.ink_manager
    }

    /// 0-based `page` separated into one plate per ink at `dpi` (lowered on large pages or with
    /// many inks), simulating overprint when `overprint` is set, with the session's Ink Manager
    /// settings applied. Read-only.
    pub fn separations(&self, page: usize, dpi: f64, overprint: bool) -> Result<Separations, EditError> {
        let editor = self.editor.as_ref().ok_or_else(|| EditError::ReadOnly(self.read_only_reason.clone().unwrap_or_default()))?;
        let base = self.press(&editor.cos);
        let conversion = Managed { conversion: base.as_ref(), inks: &self.ink_manager };
        crate::guard(|| pdfcraft_preflight::separations::rasterize(&editor.cos, page, dpi, overprint, &conversion))
            .map_err(|e| EditError::Invalid(format!("Separating page {} failed: {e}", page.saturating_add(1))))?
            .map_err(EditError::Invalid)
    }

    /// Whether overprint preview `mode` simulates overprint on this document's pages (for
    /// Only For PDF/X Files: whether it is a PDF/X file).
    pub fn overprint_preview_applies(&self, mode: OverprintPreview) -> Result<bool, EditError> {
        let editor = self.editor.as_ref().ok_or_else(|| EditError::ReadOnly(self.read_only_reason.clone().unwrap_or_default()))?;
        Ok(crate::guard(|| mode.applies(&editor.cos)).unwrap_or(false))
    }

    /// Whether this document is a PDF/X file (it declares a PDF/X version or has a `GTS_PDFX`
    /// output intent).
    pub fn is_pdfx(&self) -> Result<bool, EditError> {
        self.overprint_preview_applies(OverprintPreview::PdfX)
    }

    /// Overprint Preview: simulate overprint on a normal render of 0-based `page` (`rgba`,
    /// `width × height` RGBA pixels as displayed, after `/Rotate`), in place. Only the areas
    /// where overprinting objects change the ink plates change; RGB and Lab colours separate
    /// through [`Self::press_conversion`], with the session's Ink Manager settings. See [`pdfcraft_preflight::overprint`].
    pub fn simulate_overprint(&self, page: usize, rgba: &mut [u8], width: usize, height: usize) -> Result<OverprintSimulation, EditError> {
        let editor = self.editor.as_ref().ok_or_else(|| EditError::ReadOnly(self.read_only_reason.clone().unwrap_or_default()))?;
        let rotation = self.info.pages.get(page).map_or(0, |p| p.rotation);
        let base = self.press(&editor.cos);
        let conversion = Managed { conversion: base.as_ref(), inks: &self.ink_manager };
        crate::guard(|| pdfcraft_preflight::overprint::simulate(&editor.cos, page, rotation, rgba, width, height, &conversion))
            .map_err(|e| EditError::Invalid(format!("Overprint preview failed on page {}: {e}", page.saturating_add(1))))?
            .map_err(EditError::Invalid)
    }

    /// The objects of 0-based `page` painted with overprint on (at most `max_objects`), each
    /// with whether overprinting changes the page inside its box (separated at `dpi`). Read-only.
    pub fn overprinting_objects(&self, page: usize, dpi: f64, max_objects: usize) -> Result<OverprintObjects, EditError> {
        let editor = self.editor.as_ref().ok_or_else(|| EditError::ReadOnly(self.read_only_reason.clone().unwrap_or_default()))?;
        let base = self.press(&editor.cos);
        let conversion = Managed { conversion: base.as_ref(), inks: &self.ink_manager };
        crate::guard(|| pdfcraft_preflight::overprint::objects(&editor.cos, page, dpi, max_objects, &conversion))
            .map_err(|e| EditError::Invalid(format!("Listing overprinting objects failed: {e}")))?
            .map_err(EditError::Invalid)
    }

    /// Output Preview's Object Inspector: every object painted at user-space point (`x`, `y`)
    /// of 0-based `page`, topmost first, with its colour, overprint, transparency, font or image
    /// details, and every ink at the point (separated at `dpi`, overprint simulated when
    /// `overprint`), with the session's Ink Manager settings applied. Read-only.
    pub fn inspect_objects(&self, page: usize, x: f64, y: f64, dpi: f64, overprint: bool) -> Result<Inspection, EditError> {
        let editor = self.editor.as_ref().ok_or_else(|| EditError::ReadOnly(self.read_only_reason.clone().unwrap_or_default()))?;
        let base = self.press(&editor.cos);
        let conversion = Managed { conversion: base.as_ref(), inks: &self.ink_manager };
        crate::guard(|| pdfcraft_preflight::inspector::inspect(&editor.cos, page, x, y, dpi, overprint, &conversion))
            .map_err(|e| EditError::Invalid(format!("Inspecting page {} failed: {e}", page.saturating_add(1))))?
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
    fn tint_function(&self, doc: &pdfcraft_cos::Document, f: &pdfcraft_cos::Object, inputs: usize, outputs: usize) -> Option<Box<dyn TintFunction>> {
        edit_tint(doc, f, inputs, outputs)
    }
}

/// A press conversion whose CMYK displays through the document's resolved colour settings.
struct Displayed {
    inner: Box<dyn Conversion>,
    display: Option<pdfcraft_render::ColourProfiles>,
}

impl Conversion for Displayed {
    fn describe(&self) -> String {
        self.inner.describe()
    }
    fn uses_profile(&self) -> bool {
        self.inner.uses_profile()
    }
    fn rgb(&self, rgb: [f64; 3]) -> [f64; 4] {
        self.inner.rgb(rgb)
    }
    fn lab(&self, lab: [f64; 3]) -> [f64; 4] {
        self.inner.lab(lab)
    }
    fn tint_function(&self, doc: &pdfcraft_cos::Document, f: &pdfcraft_cos::Object, inputs: usize, outputs: usize) -> Option<Box<dyn TintFunction>> {
        self.inner.tint_function(doc, f, inputs, outputs)
    }
    fn display_cmyk(&self, quads: &[u8]) -> Option<Vec<u8>> {
        match &self.display {
            Some(d) => d.cmyk_to_rgb(quads),
            None => pdfcraft_render::device_cmyk_to_rgb(quads),
        }
    }
}

/// A tint transform read by Convert Colors' function reader (Types 0, 2, 3 and 4).
struct EditTint(pdfcraft_edit::convert_colors::Function);

impl TintFunction for EditTint {
    fn eval(&self, input: &[f64]) -> Option<Vec<f64>> {
        self.0.eval(input)
    }
}

fn edit_tint(doc: &pdfcraft_cos::Document, f: &pdfcraft_cos::Object, inputs: usize, outputs: usize) -> Option<Box<dyn TintFunction>> {
    let function = pdfcraft_edit::convert_colors::Function::parse(doc, f)?;
    let probe = function.eval(&vec![0.0; inputs])?;
    (probe.len() == outputs).then(|| Box::new(EditTint(function)) as Box<dyn TintFunction>)
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
    fn tint_function(&self, doc: &pdfcraft_cos::Document, f: &pdfcraft_cos::Object, inputs: usize, outputs: usize) -> Option<Box<dyn TintFunction>> {
        edit_tint(doc, f, inputs, outputs)
    }
}

/// The CMYK profile press conversion uses (the output intent's or the working space's), when
/// it uses one, and its label.
fn press_profile(doc: &pdfcraft_cos::Document, color: &crate::color_settings::ColorManagement) -> Option<(Vec<u8>, String)> {
    color.press_profile(doc).ok().map(|(_, bytes, label)| (bytes.to_vec(), label))
}

/// What [`Session::apply_ink_manager`] changed (or would change).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InkManagerReport {
    /// The aliases written (each spot ink → the ink it now names).
    pub aliases: BTreeMap<String, String>,
    pub renamed: pdfcraft_edit::AliasReport,
    /// The spot inks converted to process (`None`: every spot ink).
    pub to_process: Option<BTreeSet<String>>,
    /// The conversion of spot inks to process, when any were set to process.
    pub conversion: Option<pdfcraft_edit::ConversionReport>,
    /// What the spot inks were converted to (`DeviceCMYK` numbers, through the output intent's
    /// profile or the built-in approximation).
    pub destination: Option<String>,
}

impl InkManagerReport {
    pub fn changed(&self) -> bool {
        self.renamed.changed() || self.conversion.as_ref().is_some_and(|c| c.changed())
    }
}

/// The edit that makes `inks` permanent in `doc`: aliases renamed, then the inks set to process
/// converted (through the output intent's CMYK profile when it has a usable one).
fn ink_manager_edit(doc: &pdfcraft_cos::Document, inks: &InkManager, color: &crate::color_settings::ColorManagement) -> (Edit, InkManagerReport) {
    let aliases = inks.final_aliases();
    let bytes: BTreeMap<Vec<u8>, Vec<u8>> = aliases.iter().map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec())).collect();
    let mut report = InkManagerReport { aliases, ..Default::default() };
    let conversion = if inks.all_to_process || !inks.to_process.is_empty() {
        let spot_names = (!inks.all_to_process).then(|| inks.to_process.iter().map(|n| n.as_bytes().to_vec()).collect::<BTreeSet<_>>());
        report.to_process = (!inks.all_to_process).then(|| inks.to_process.clone());
        let mut c =
            pdfcraft_edit::ColorConversion { sources: pdfcraft_edit::SourceSpaces::NONE, convert_spots: true, spot_names, ..Default::default() };
        c.intent = color.settings().intent;
        match press_profile(doc, color) {
            Some((profile, label)) => {
                c.destination = pdfcraft_edit::Destination::Icc(std::sync::Arc::new(profile));
                c.device_output = true;
                report.destination = Some(format!("DeviceCMYK, through {label}"));
            }
            None => report.destination = Some("DeviceCMYK, through the built-in conversion (no usable CMYK output intent)".into()),
        }
        Some(c)
    } else {
        None
    };
    (Edit::ApplyInkManager { aliases: bytes, conversion }, report)
}

/// RGB and Lab to CMYK under the colour settings `color`: through the output intent's CMYK
/// profile (unless the settings say otherwise) or the working CMYK profile when it has tables
/// PdfCraft can use, else the built-in conversion, whose description says why.
fn press_conversion(doc: &pdfcraft_cos::Document, color: &crate::color_settings::ColorManagement) -> Box<dyn Conversion> {
    match color.press_profile(doc) {
        Ok((profile, _, label)) => Box::new(ProfileConversion { label, profile }),
        Err(why) => Box::new(Fallback(why)),
    }
}

impl Session {
    /// Set the Ink Manager for document `id` (session only: not saved, not undoable). The
    /// separations preview, ink listing and total area coverage honour it.
    pub fn set_ink_manager(&mut self, id: DocId, inks: InkManager) -> Result<(), EditError> {
        inks.validate().map_err(EditError::Invalid)?;
        self.doc_mut(id)?.ink_manager = inks;
        Ok(())
    }

    /// What [`Self::apply_ink_manager`] would change, tried on a copy.
    pub fn apply_ink_manager_preview(&self, id: DocId) -> Result<InkManagerReport, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let editor = doc.editor.as_ref().ok_or_else(|| EditError::ReadOnly(doc.read_only_reason.clone().unwrap_or_default()))?;
        let (edit, mut report) = ink_manager_edit(&editor.cos, &doc.ink_manager, &doc.color_management);
        let Edit::ApplyInkManager { aliases, conversion } = &edit else { return Ok(report) };
        let mut probe = editor.cos.clone();
        let (renamed, converted) = crate::guard(|| apply_ink_manager_to(&mut probe, aliases, conversion.as_ref())).map_err(EditError::Invalid)??;
        report.renamed = renamed;
        report.conversion = converted;
        Ok(report)
    }

    /// Make the Ink Manager permanent as one undo step: aliased spot inks are renamed in every
    /// Separation and DeviceN colour space, then the spot inks set to process are converted to
    /// DeviceCMYK. When nothing would change, no undo step is recorded. The settings stay.
    pub fn apply_ink_manager(&mut self, id: DocId) -> Result<InkManagerReport, EditError> {
        let report = self.apply_ink_manager_preview(id)?;
        if report.changed() {
            let doc = self.get(id).ok_or(EditError::NoDocument)?;
            let editor = doc.editor.as_ref().ok_or_else(|| EditError::ReadOnly(doc.read_only_reason.clone().unwrap_or_default()))?;
            let (edit, _) = ink_manager_edit(&editor.cos, &doc.ink_manager, &doc.color_management);
            self.apply(id, edit)?;
        }
        Ok(report)
    }

    /// Preferences ▸ Page Display ▸ Use Overprint Preview: when page renders simulate overprint
    /// (Only For PDF/X Files by default, as in Acrobat).
    pub fn overprint_preview(&self) -> OverprintPreview {
        self.overprint_preview
    }

    pub fn set_overprint_preview(&mut self, mode: OverprintPreview) {
        self.overprint_preview = mode;
    }

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

/// Rename `aliases`, then convert spot inks with `conversion`, on `doc` (all pages).
pub(crate) fn apply_ink_manager_to(
    doc: &mut pdfcraft_cos::Document,
    aliases: &BTreeMap<Vec<u8>, Vec<u8>>,
    conversion: Option<&pdfcraft_edit::ColorConversion>,
) -> Result<(pdfcraft_edit::AliasReport, Option<pdfcraft_edit::ConversionReport>), EditError> {
    let renamed = pdfcraft_edit::alias_inks(doc, aliases)?;
    let converted = match conversion {
        Some(c) => {
            let pages: Vec<usize> = (0..pdfcraft_model::pages(doc).len()).collect();
            Some(pdfcraft_edit::convert_colors(doc, &pages, c)?)
        }
        None => None,
    };
    Ok((renamed, converted))
}

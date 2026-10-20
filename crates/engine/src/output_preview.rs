//! Output Preview (Print Production ▸ Output Preview): per-document session settings — the
//! simulation profile, Simulate Overprinting / Paper Color / Black Ink, the Show filter, the
//! separations switched off and the total area coverage warning — and the preview rendered with
//! them. The settings are not saved with the document and are not undo steps.
//!
//! The simulation profile comes from the colour settings ([`crate::color_settings`]): by default
//! the document's output intent profile when the untagged-colour policy lets it override the
//! working spaces, else the working CMYK space; or the working CMYK space; or any CMYK profile
//! the colour settings hold (built-in or loaded with [`Session::load_color_profile`]). RGB and Lab
//! colours separate as the colour settings say ([`crate::Document::press_conversion`]); a chosen
//! profile with `BToA` tables separates them itself, with the settings' rendering intent.
//! Process inks display through the profile's `AToB` table (paper and black ink simulated from
//! it); with colour management of untagged colour off there is no profile and they display as
//! pages render them. The preview is composited from PdfCraft's ink plates
//! ([`pdfcraft_preflight::output_preview`]); the Ink Manager's session settings apply.

use std::collections::BTreeSet;
use std::sync::Arc;

use pdfcraft_edit::convert_colors::{Family, Profile};
use pdfcraft_preflight::coverage::{BuiltIn, Conversion, TintFunction};
use pdfcraft_preflight::ink_manager::Managed;
pub use pdfcraft_preflight::output_preview::{
    Coverage as PreviewCoverage, DEFAULT_LIMIT, DEFAULT_WARNING_COLOUR, Display, InkStat, MAX_HIDDEN_INKS, MAX_INK_NAME, MAX_LIMIT, MIN_LIMIT,
    Preview, Settings as PreviewSettings, Show, Simulate, synthetic_cmyk_profile,
};

use crate::color_settings::{ColorManagement, UntaggedPolicy, intent_code};
use crate::{DocId, EditError, Session};

/// Where the simulation profile comes from.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum SimulationProfile {
    /// The document's output intent profile when the colour settings let it override the working
    /// spaces, else the working CMYK space (the viewer's default).
    #[default]
    OutputIntent,
    /// The colour settings' working CMYK space.
    Working,
    /// A CMYK profile the colour settings held when it was chosen
    /// ([`Session::simulation_profile`]): its id (`color_profiles_list`), name and bytes.
    Profile { id: String, name: String, bytes: Arc<Vec<u8>> },
}

impl SimulationProfile {
    pub fn id(&self) -> &str {
        match self {
            SimulationProfile::OutputIntent => "output_intent",
            SimulationProfile::Working => "working",
            SimulationProfile::Profile { id, .. } => id,
        }
    }
}

/// Output Preview's settings for one document.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputPreview {
    pub profile: SimulationProfile,
    /// Simulate Overprinting (on by default).
    pub simulate_overprint: bool,
    /// Simulate Paper Color (implies black ink) and Simulate Black Ink.
    pub simulate_paper: bool,
    pub simulate_black: bool,
    pub show: Show,
    /// Separations switched off.
    pub hidden_inks: BTreeSet<String>,
    /// The Total Area Coverage warning and its limit (percent).
    pub coverage_warning: bool,
    pub coverage_limit: f64,
    /// The colour areas over the limit are shown in.
    pub warning_colour: [u8; 3],
}

impl Default for OutputPreview {
    fn default() -> Self {
        OutputPreview {
            profile: SimulationProfile::OutputIntent,
            simulate_overprint: true,
            simulate_paper: false,
            simulate_black: false,
            show: Show::All,
            hidden_inks: BTreeSet::new(),
            coverage_warning: false,
            coverage_limit: DEFAULT_LIMIT,
            warning_colour: DEFAULT_WARNING_COLOUR,
        }
    }
}

impl OutputPreview {
    /// The rendering settings.
    pub fn settings(&self) -> PreviewSettings {
        PreviewSettings {
            show: self.show,
            overprint: self.simulate_overprint,
            simulate: Simulate { paper: self.simulate_paper, black: self.simulate_black || self.simulate_paper },
            hidden_inks: self.hidden_inks.clone(),
            coverage_warning: self.coverage_warning.then_some(self.coverage_limit),
            warning_colour: self.warning_colour,
        }
    }

    /// Check the settings (a chosen profile is checked against the colour settings by
    /// [`Session::set_output_preview`]).
    pub fn validate(&self) -> Result<(), String> {
        if !(self.coverage_limit.is_finite() && (MIN_LIMIT..=MAX_LIMIT).contains(&self.coverage_limit)) {
            return Err(format!("the total area coverage limit must be between {MIN_LIMIT} and {MAX_LIMIT} percent"));
        }
        if self.simulate_paper && !self.simulate_black {
            return Err("simulate_black can't be off while simulate_paper is on (simulating paper colour simulates black ink)".into());
        }
        self.settings().validate()?;
        Ok(())
    }
}

/// What the simulation profile resolved to for a document.
#[derive(Clone, Debug, PartialEq)]
pub struct SimulationInfo {
    /// `output_intent`, `working` or `profile` (what was asked for).
    pub source: &'static str,
    /// Where the profile used came from: `output_intent`, `working` or `profile`; `none` when
    /// no profile displays the inks.
    pub used: &'static str,
    /// The profile's name: the output condition, or the colour-settings profile id.
    pub name: Option<String>,
    /// Whether a profile displays the inks (paper colour and black ink can be simulated).
    pub profile_used: bool,
    /// What displays the process inks.
    pub display: String,
    /// How RGB and Lab colours become process inks.
    pub conversion: String,
    /// Why no profile (or not all of it) is used, when that is so.
    pub problem: Option<String>,
}

/// RGB and Lab to CMYK through a chosen CMYK profile's `BToA` table; spot tint transforms as
/// the document's press conversion reads them.
struct Chosen {
    profile: pdfcraft_edit::convert_colors::Profile,
    label: String,
    inner: Box<dyn Conversion>,
}

impl Conversion for Chosen {
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
        self.inner.tint_function(doc, f, inputs, outputs)
    }
    fn display_cmyk(&self, quads: &[u8]) -> Option<Vec<u8>> {
        self.inner.display_cmyk(quads)
    }
}

/// A CMYK profile of the colour settings to simulate: its bytes and name.
fn held_cmyk(color: &ColorManagement, id: &str) -> Result<(Arc<Vec<u8>>, String), String> {
    let (info, bytes) = color.profile(id).ok_or_else(|| {
        let cmyk: Vec<String> = color.profiles().into_iter().filter(|p| p.family == Family::Cmyk).map(|p| p.id).collect();
        format!("the colour settings hold no profile {id:?}; their CMYK profiles are {}", cmyk.join(", "))
    })?;
    if info.family != Family::Cmyk {
        return Err(format!("{id:?} is not a CMYK profile"));
    }
    Ok((bytes, format!("{} ({})", info.id, info.description)))
}

/// The colour settings' CMYK profile `id` as a simulation profile, checked to display CMYK.
pub fn held_profile(color: &ColorManagement, id: &str) -> Result<SimulationProfile, String> {
    let (bytes, name) = held_cmyk(color, id)?;
    Display::from_profile(&bytes, &name).map_err(|e| format!("{id:?} can't be the simulation profile: {e}"))?;
    Ok(SimulationProfile::Profile { id: id.to_string(), name, bytes })
}

impl crate::Document {
    /// Output Preview's session settings for this document.
    pub fn output_preview(&self) -> &OutputPreview {
        &self.output_preview
    }

    /// The press conversion and display the simulation profile gives, and what they are.
    fn simulation(&self, cos: &pdfcraft_cos::Document) -> Result<(Box<dyn Conversion>, Display, SimulationInfo), EditError> {
        let press = self.press_conversion()?;
        let color = self.color_management.as_ref();
        let profile = &self.output_preview.profile;
        let source = match profile {
            SimulationProfile::OutputIntent => "output_intent",
            SimulationProfile::Working => "working",
            SimulationProfile::Profile { .. } => "profile",
        };
        let mut problem: Option<String> = None;
        // The profile to display through: (bytes, name, where it came from).
        let mut chosen: Option<(Arc<Vec<u8>>, String, &'static str)> = None;
        let unmanaged = color.settings().untagged == UntaggedPolicy::Unmanaged;
        match profile {
            SimulationProfile::Profile { name, bytes, .. } => chosen = Some((bytes.clone(), name.clone(), "profile")),
            _ if unmanaged => problem = Some("colour management of untagged colour is off in the colour settings".into()),
            SimulationProfile::OutputIntent if color.settings().untagged == UntaggedPolicy::OutputIntent => {
                match crate::guard(|| pdfcraft_preflight::separations::output_intent(cos)).ok().flatten() {
                    None => problem = Some("the document has no output intent, so the working CMYK space is simulated".into()),
                    Some(i) => {
                        let name = i.condition.clone().unwrap_or_else(|| format!("{} output intent", i.subtype));
                        match i.profile {
                            Some(bytes) if bytes.get(16..20) == Some(b"CMYK".as_slice()) => chosen = Some((Arc::new(bytes), name, "output_intent")),
                            Some(_) => {
                                problem = Some(format!("the output intent {name:?} is not a CMYK profile, so the working CMYK space is simulated"))
                            }
                            None => {
                                let why = i.problem.unwrap_or_else(|| "it has no DestOutputProfile".into());
                                problem = Some(format!("the output intent {name:?} is not used ({why}), so the working CMYK space is simulated"));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        if chosen.is_none()
            && !unmanaged
            && let Some((info, bytes)) = color.working(Family::Cmyk)
        {
            chosen = Some((bytes, format!("{} ({})", info.id, info.description), "working"));
        }
        let mut display = Display::renderer();
        let mut used = "none";
        let mut name = None;
        if let Some((bytes, label, from)) = &chosen {
            let described = match *from {
                "output_intent" => format!("the output intent's profile ({label})"),
                "working" => format!("the working CMYK space {label}"),
                _ => format!("the colour settings' profile {label}"),
            };
            match crate::guard(|| Display::from_profile(bytes, &described)) {
                Ok(Ok(d)) => {
                    display = d;
                    used = from;
                    name = Some(label.clone());
                }
                Ok(Err(e)) | Err(e) => {
                    if *from == "profile" {
                        return Err(EditError::Invalid(format!("{label} can't be the simulation profile: {e}")));
                    }
                    let why = format!("{described} can't display CMYK: {e}");
                    problem = Some(match problem {
                        Some(p) => format!("{p}; {why}"),
                        None => why,
                    });
                }
            }
        }
        // A chosen profile separates RGB and Lab itself when it has BToA tables; otherwise (and
        // for the output intent and working space) the colour settings' press conversion does.
        let (conversion, conversion_note): (Box<dyn Conversion>, Option<String>) = match (&chosen, used) {
            (Some((bytes, label, "profile")), "profile") => {
                let intent = intent_code(color.settings().intent);
                let parsed =
                    crate::guard(|| Profile::parse(bytes, intent)).ok().and_then(Result::ok).filter(|p| p.family == Family::Cmyk && p.exact());
                match parsed {
                    Some(p) => {
                        let label = format!("the simulation profile {label}, {}", color.settings().intent.name().replace('_', " "));
                        (Box::new(Chosen { profile: p, label, inner: press }), None)
                    }
                    None => {
                        let why = format!("{label} has no BToA table PdfCraft reads, so RGB and Lab separate through {}", press.describe());
                        (press, Some(why))
                    }
                }
            }
            _ => (press, None),
        };
        if let Some(n) = conversion_note {
            problem = Some(match problem {
                Some(p) => format!("{p}; {n}"),
                None => n,
            });
        }
        let info = SimulationInfo {
            source,
            used,
            name,
            profile_used: display.has_profile(),
            display: display.description.clone(),
            conversion: conversion.describe(),
            problem,
        };
        Ok((conversion, display, info))
    }

    /// What the simulation profile resolves to for this document.
    pub fn output_preview_simulation(&self) -> Result<SimulationInfo, EditError> {
        let editor = self.editor.as_ref().ok_or_else(|| EditError::ReadOnly(self.read_only_reason.clone().unwrap_or_default()))?;
        self.simulation(&editor.cos).map(|(_, _, info)| info)
    }

    /// Render Output Preview for 0-based `page` with the session's settings (and Ink Manager),
    /// separated at `dpi` (9–300, lowered on large pages or with many inks). Read-only.
    pub fn render_output_preview(&self, page: usize, dpi: f64) -> Result<(Preview, SimulationInfo), EditError> {
        let editor = self.editor.as_ref().ok_or_else(|| EditError::ReadOnly(self.read_only_reason.clone().unwrap_or_default()))?;
        let (base, display, info) = self.simulation(&editor.cos)?;
        let conversion = Managed { conversion: base.as_ref(), inks: &self.ink_manager };
        let settings = self.output_preview.settings();
        let preview = crate::guard(|| pdfcraft_preflight::output_preview::render(&editor.cos, page, dpi, &settings, &conversion, &display))
            .map_err(|e| EditError::Invalid(format!("Output preview failed on page {}: {e}", page.saturating_add(1))))?
            .map_err(EditError::Invalid)?;
        Ok((preview, info))
    }
}

impl Session {
    /// The colour settings' CMYK profile `id` (built-in, or loaded with
    /// [`Session::load_color_profile`]) as Output Preview's simulation profile.
    pub fn simulation_profile(&self, id: &str) -> Result<SimulationProfile, EditError> {
        crate::guard(|| held_profile(&self.color, id)).map_err(EditError::Invalid)?.map_err(EditError::Invalid)
    }

    /// Set Output Preview's settings for document `id` (session only: not saved, not
    /// undoable).
    pub fn set_output_preview(&mut self, id: DocId, settings: OutputPreview) -> Result<(), EditError> {
        crate::guard(|| settings.validate()).map_err(EditError::Invalid)?.map_err(EditError::Invalid)?;
        if let SimulationProfile::Profile { id: pid, name, bytes } = &settings.profile {
            crate::guard(|| Display::from_profile(bytes, name))
                .map_err(EditError::Invalid)?
                .map_err(|e| EditError::Invalid(format!("{pid:?} can't be the simulation profile: {e}")))?;
        }
        self.doc_mut(id)?.output_preview = settings;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_validate_and_paper_implies_black() {
        let mut s = OutputPreview::default();
        assert!(s.validate().is_ok());
        assert!(s.settings().overprint && s.settings().coverage_warning.is_none());
        s.simulate_paper = true;
        assert!(s.validate().is_err());
        s.simulate_black = true;
        assert!(s.validate().is_ok());
        s.coverage_warning = true;
        s.coverage_limit = 280.0;
        assert_eq!(s.settings().coverage_warning, Some(280.0));
        s.coverage_limit = 0.0;
        assert!(s.validate().is_err());
        s.coverage_limit = 300.0;
        assert!(s.validate().is_ok());
        let color = ColorManagement::default();
        assert!(held_profile(&color, "nope").unwrap_err().contains("CMYK profiles are"));
        assert!(held_profile(&color, "srgb").unwrap_err().contains("not a CMYK"));
        let cgats = held_profile(&color, "cgats001").unwrap();
        assert_eq!(cgats.id(), "cgats001");
        assert!(s.validate().is_ok());
    }
}

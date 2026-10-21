//! Preferences ▸ Color Management: the working spaces untagged colour (DeviceGray, DeviceRGB,
//! DeviceCMYK) is read in, the rendering intent, black point compensation and the policy for
//! untagged colour. Session settings, not saved with documents.
//!
//! **Profiles.** Built-in profiles are written in code from published colorimetry (RGB matrix
//! spaces from their primaries, gray spaces from a tone curve; see
//! [`pdfcraft_preflight::icc`]) plus the renderer's CC0 CMYK profile; users add ICC profiles
//! (gray, RGB or CMYK; output, display, input or colour-space class) with
//! [`Session::load_color_profile`].
//!
//! **What the settings change.**
//! - Rendering: DeviceGray, DeviceRGB and DeviceCMYK are converted to the (sRGB) display through
//!   the resolved profiles with the intent (a profile without that table uses its default one)
//!   and black point compensation; ICC-based colour of the document uses the intent and black
//!   point compensation too.
//! - Output Preview (separations, total area coverage, overprint preview, object inspector,
//!   Ink Manager conversions): RGB and Lab become process inks through the output intent's CMYK
//!   profile or the working CMYK profile (when it has `BToA` tables), with the intent; overprint
//!   preview composites its plates through the resolved display conversion.
//! - Convert Colors and Convert to PDF/X take their defaults from them (see the automation tools).
//!
//! **Untagged colour policy.** `output_intent` (the viewer's "Output Intent Overrides Working
//! Spaces", the default): a document's output intent profile replaces the working space of its
//! family; `working_space`: always the working spaces; `unmanaged`: no colour management of
//! untagged colour (gray and RGB as sRGB numbers, CMYK naively as R = (1 − C)(1 − K), RGB and Lab
//! separated by the built-in conversion).

use std::sync::{Arc, LazyLock};

use pdfcraft_edit::convert_colors::{Family, Intent, Profile};
use pdfcraft_render::{ColourProfiles, ColourSpec};

use crate::{DocId, EditError, Session};

pub use pdfcraft_edit::convert_colors::Family as ProfileFamily;

/// User profiles held at most, and the longest name.
pub const MAX_USER_PROFILES: usize = 32;
pub const MAX_PROFILE_NAME: usize = 64;
/// The prefix of user profile ids.
pub const USER_PREFIX: &str = "user:";

/// What happens to untagged (Device) colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UntaggedPolicy {
    /// The document's output intent profile replaces the working space of its family.
    #[default]
    OutputIntent,
    /// The working spaces, whatever the output intent.
    WorkingSpace,
    /// No colour management of untagged colour.
    Unmanaged,
}

impl UntaggedPolicy {
    pub const ALL: [UntaggedPolicy; 3] = [UntaggedPolicy::OutputIntent, UntaggedPolicy::WorkingSpace, UntaggedPolicy::Unmanaged];

    pub fn id(self) -> &'static str {
        match self {
            UntaggedPolicy::OutputIntent => "output_intent",
            UntaggedPolicy::WorkingSpace => "working_space",
            UntaggedPolicy::Unmanaged => "unmanaged",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            UntaggedPolicy::OutputIntent => "Output intent overrides working spaces",
            UntaggedPolicy::WorkingSpace => "Always use the working spaces",
            UntaggedPolicy::Unmanaged => "Don't colour-manage untagged colour",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }
}

/// The ICC intent number of `intent`.
pub fn intent_code(intent: Intent) -> u8 {
    match intent {
        Intent::Perceptual => 0,
        Intent::RelativeColorimetric => 1,
        Intent::Saturation => 2,
        Intent::AbsoluteColorimetric => 3,
    }
}

/// A family's name in settings and tool output.
pub fn family_id(f: Family) -> &'static str {
    match f {
        Family::Gray => "gray",
        Family::Rgb => "rgb",
        Family::Cmyk => "cmyk",
    }
}

/// A colour profile the settings can name.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileInfo {
    /// `srgb`, `cgats001`, … or `user:<name>`.
    pub id: String,
    pub family: Family,
    pub description: String,
    pub built_in: bool,
    /// Where a user profile was loaded from.
    pub source: Option<String>,
    pub size: usize,
    /// It can convert its colours to the display (the renderer accepts it).
    pub displays: bool,
    /// It has tables PdfCraft converts into it with (`BToA`, or a gray/RGB matrix/TRC): CMYK
    /// working spaces without them separate RGB with the built-in conversion.
    pub converts_into: bool,
}

#[derive(Clone, Debug)]
struct Entry {
    info: ProfileInfo,
    bytes: Arc<Vec<u8>>,
}

const CGATS: &str = "cgats001";

fn rgb_space(
    id: &str,
    description: &str,
    primaries: [[f64; 2]; 3],
    white: [f64; 2],
    curve: &dyn Fn(f64) -> f64,
) -> Option<(String, Family, String, Vec<u8>)> {
    let colorants = pdfcraft_preflight::icc::d50_colorants(primaries, white)?;
    Some((id.into(), Family::Rgb, description.into(), pdfcraft_preflight::icc::rgb_matrix_trc(description, colorants, curve)))
}

/// Dot gain `gain` at 50 % (parabolic tone value increase): gray value → luminance. The printed
/// tone is shown through the sRGB tone curve, so with no gain it is `sgray`.
fn dot_gain(gain: f64) -> impl Fn(f64) -> f64 {
    move |g: f64| {
        let ink = 1.0 - g;
        let printed = (ink + 4.0 * gain * ink * (1.0 - ink)).clamp(0.0, 1.0);
        pdfcraft_preflight::icc::srgb_curve(1.0 - printed)
    }
}

/// A tone curve: encoded value → linear light.
type ToneCurve = dyn Fn(f64) -> f64;

static BUILT_IN: LazyLock<Vec<Entry>> = LazyLock::new(|| {
    use pdfcraft_preflight::icc::{gray_trc, srgb, srgb_curve};
    const D65: [f64; 2] = [0.3127, 0.3290];
    let mut list: Vec<(String, Family, String, Vec<u8>)> = vec![("srgb".into(), Family::Rgb, "sRGB IEC 61966-2-1 (PdfCraft)".into(), srgb())];
    list.extend(rgb_space(
        "display-p3",
        "Display P3 (P3 primaries, D65, sRGB tone curve)",
        [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]],
        D65,
        &srgb_curve,
    ));
    list.extend(rgb_space(
        "a98-rgb",
        "A98 RGB compatible (1998 wide-gamut primaries, D65, gamma 2.2)",
        [[0.64, 0.33], [0.21, 0.71], [0.15, 0.06]],
        D65,
        &|v: f64| v.powf(563.0 / 256.0),
    ));
    list.extend(rgb_space(
        "romm-rgb",
        "ROMM RGB (ISO 22028-2 primaries, D50, gamma 1.8)",
        [[0.7347, 0.2653], [0.1596, 0.8404], [0.0366, 0.0001]],
        [0.3457, 0.3585],
        &|v: f64| v.powf(1.8),
    ));
    let grays: [(&str, &str, Box<ToneCurve>); 5] = [
        ("sgray", "Gray, sRGB tone curve (PdfCraft)", Box::new(srgb_curve)),
        ("gray-gamma-2.2", "Gray Gamma 2.2 (PdfCraft)", Box::new(|v: f64| v.powf(2.2))),
        ("gray-gamma-1.8", "Gray Gamma 1.8 (PdfCraft)", Box::new(|v: f64| v.powf(1.8))),
        ("dot-gain-15", "Dot Gain 15% (PdfCraft: parabolic tone value increase, sRGB tone curve)", Box::new(dot_gain(0.15))),
        ("dot-gain-20", "Dot Gain 20% (PdfCraft: parabolic tone value increase, sRGB tone curve)", Box::new(dot_gain(0.20))),
    ];
    for (id, description, curve) in grays {
        list.push((id.into(), Family::Gray, description.into(), gray_trc(description, &*curve)));
    }
    list.push((
        CGATS.into(),
        Family::Cmyk,
        "CGATS001Compat v2 micro (CC0; coated press; display table only)".into(),
        pdfcraft_render::default_cmyk_profile().to_vec(),
    ));
    list.into_iter()
        .map(|(id, family, description, bytes)| {
            let bytes = Arc::new(bytes);
            let (displays, converts_into) = capabilities_of(&bytes, family);
            let info = ProfileInfo { id, family, description, built_in: true, source: None, size: bytes.len(), displays, converts_into };
            Entry { info, bytes }
        })
        .collect()
});

/// Whether `bytes` (a `family` profile) displays and whether PdfCraft converts into it.
fn capabilities_of(bytes: &Arc<Vec<u8>>, family: Family) -> (bool, bool) {
    let mut spec = ColourSpec::default();
    match family {
        Family::Gray => spec.gray = Some(bytes.clone()),
        Family::Rgb => spec.rgb = Some(bytes.clone()),
        Family::Cmyk => spec.cmyk = Some(bytes.clone()),
    }
    let displays = ColourProfiles::new(spec).is_ok();
    let converts_into = Profile::parse(bytes, 1).is_ok_and(|p| p.family == family && p.exact());
    (displays, converts_into)
}

/// The colour settings.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorSettings {
    /// Working space profile ids.
    pub rgb: String,
    pub cmyk: String,
    pub gray: String,
    pub intent: Intent,
    pub black_point_compensation: bool,
    pub untagged: UntaggedPolicy,
}

impl Default for ColorSettings {
    fn default() -> Self {
        Self {
            rgb: "srgb".into(),
            cmyk: CGATS.into(),
            gray: "sgray".into(),
            intent: Intent::RelativeColorimetric,
            black_point_compensation: false,
            untagged: UntaggedPolicy::OutputIntent,
        }
    }
}

/// The settings with the user profiles they can name.
#[derive(Clone, Debug, Default)]
pub struct ColorManagement {
    settings: ColorSettings,
    user: Vec<Entry>,
}

/// One family's resolved working space.
#[derive(Clone, Debug, PartialEq)]
pub struct SpaceUse {
    pub family: Family,
    /// `working_space`, `output_intent` or `unmanaged`.
    pub source: &'static str,
    /// What is used, for reports.
    pub description: String,
    /// The working space's id (also when the output intent overrides it).
    pub working: String,
    /// Why the output intent's profile isn't used, when the policy asked for it.
    pub note: Option<String>,
}

/// The settings resolved for one document.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    /// The renderer's colour configuration.
    pub render: Option<ColourProfiles>,
    pub spaces: Vec<SpaceUse>,
    /// Why rendering fell back to the built-in conversions, when it did.
    pub problem: Option<String>,
}

impl ColorManagement {
    pub fn settings(&self) -> &ColorSettings {
        &self.settings
    }

    fn entries(&self) -> impl Iterator<Item = &Entry> {
        BUILT_IN.iter().chain(self.user.iter())
    }

    fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries().find(|e| e.info.id == id)
    }

    /// Every profile the settings can name: built-in ones first.
    pub fn profiles(&self) -> Vec<ProfileInfo> {
        self.entries().map(|e| e.info.clone()).collect()
    }

    pub fn profile(&self, id: &str) -> Option<(ProfileInfo, Arc<Vec<u8>>)> {
        self.entry(id).map(|e| (e.info.clone(), e.bytes.clone()))
    }

    /// The working space of `family`: its profile and bytes.
    pub fn working(&self, family: Family) -> Option<(ProfileInfo, Arc<Vec<u8>>)> {
        let id = match family {
            Family::Gray => &self.settings.gray,
            Family::Rgb => &self.settings.rgb,
            Family::Cmyk => &self.settings.cmyk,
        };
        self.profile(id)
    }

    /// Whether `family`'s working space is the factory one.
    pub fn working_is_default(&self, family: Family) -> bool {
        let d = ColorSettings::default();
        match family {
            Family::Gray => self.settings.gray == d.gray,
            Family::Rgb => self.settings.rgb == d.rgb,
            Family::Cmyk => self.settings.cmyk == d.cmyk,
        }
    }

    /// Convert Colors to a device space of `family`: the working space's profile, when it isn't
    /// the factory one (whose conversions the documented built-in approximation stands for)
    /// and PdfCraft can convert into it. With its description.
    pub fn working_destination(&self, family: Family) -> Option<(Arc<Vec<u8>>, String)> {
        if self.working_is_default(family) {
            return None;
        }
        let (info, bytes) = self.working(family)?;
        info.converts_into.then(|| (bytes, format!("the working {} space {:?} ({})", family_id(family).to_uppercase(), info.id, info.description)))
    }

    /// The working CMYK profile as a PDF/X output intent: an output (`prtr`) profile that isn't
    /// the factory one. With its description.
    pub fn working_output_profile(&self) -> Option<(Arc<Vec<u8>>, String)> {
        if self.working_is_default(Family::Cmyk) {
            return None;
        }
        let (info, bytes) = self.working(Family::Cmyk)?;
        (bytes.get(12..16) == Some(b"prtr".as_slice())).then_some((bytes, info.description))
    }

    /// Check `settings` against the profiles held.
    pub fn validate(&self, settings: &ColorSettings) -> Result<(), String> {
        for (family, id) in [(Family::Rgb, &settings.rgb), (Family::Cmyk, &settings.cmyk), (Family::Gray, &settings.gray)] {
            let name = family_id(family);
            let Some(e) = self.entry(id) else {
                let ids: Vec<String> = self.entries().filter(|e| e.info.family == family).map(|e| e.info.id.clone()).collect();
                return Err(format!("unknown {name} profile {id:?} (one of {})", ids.join(", ")));
            };
            if e.info.family != family {
                return Err(format!("{id:?} is a {} profile, not a {name} profile", family_id(e.info.family)));
            }
            if !e.info.displays && !e.info.converts_into {
                return Err(format!("{id:?} can't be used: no table converts its colours"));
            }
        }
        Ok(())
    }

    /// Resolve the settings for `doc`.
    pub fn resolve(&self, doc: Option<&pdfcraft_cos::Document>, previous: Option<&ColourProfiles>) -> Resolved {
        let s = &self.settings;
        let intent = intent_code(s.intent);
        let unmanaged = s.untagged == UntaggedPolicy::Unmanaged;
        // The output intent's profile, with its family, when the policy lets it override.
        let mut oi_note = None;
        let oi = if s.untagged == UntaggedPolicy::OutputIntent {
            doc.and_then(pdfcraft_preflight::separations::output_intent).and_then(|oi| {
                let name = match &oi.condition {
                    Some(c) => format!("{} output intent {c:?}", oi.subtype),
                    None => format!("{} output intent", oi.subtype),
                };
                let Some(bytes) = oi.profile else {
                    oi_note = Some(format!("the {name} has no usable DestOutputProfile"));
                    return None;
                };
                match Profile::parse(&bytes, intent) {
                    Ok(p) => Some((p.family, name, Arc::new(bytes))),
                    Err(e) => {
                        oi_note = Some(format!("the {name}'s profile could not be read: {e}"));
                        None
                    }
                }
            })
        } else {
            None
        };
        let mut spec = ColourSpec { intent, black_point_compensation: s.black_point_compensation, naive_cmyk: unmanaged, ..ColourSpec::default() };
        let mut spaces = Vec::new();
        for family in [Family::Rgb, Family::Cmyk, Family::Gray] {
            let (winfo, wbytes) = match self.working(family) {
                Some(w) => w,
                None => match BUILT_IN.iter().find(|e| e.info.family == family) {
                    Some(e) => (e.info.clone(), e.bytes.clone()),
                    None => continue,
                },
            };
            let identity = matches!(winfo.id.as_str(), "srgb" | "sgray") || (family == Family::Cmyk && winfo.id == CGATS);
            // An output intent that can't be read is reported with the CMYK space (PDF/X).
            let mut note = if family == Family::Cmyk { oi_note.clone() } else { None };
            let mut chosen: Option<(Arc<Vec<u8>>, &'static str, String)> = None;
            if unmanaged {
                spaces.push(SpaceUse {
                    family,
                    source: "unmanaged",
                    description: match family {
                        Family::Cmyk => "uncalibrated: R = (1 − C)(1 − K) and so on".into(),
                        _ => "uncalibrated: the numbers as sRGB".into(),
                    },
                    working: winfo.id.clone(),
                    note: None,
                });
                continue;
            }
            if let Some((f, name, bytes)) = &oi
                && *f == family
            {
                let (displays, _) = capabilities_of(bytes, family);
                if displays {
                    chosen = Some((bytes.clone(), "output_intent", format!("the {name}'s profile")));
                } else {
                    note = Some(format!("the {name}'s profile has no table that converts its colours to the display"));
                }
            }
            let (bytes, source, description) = match chosen {
                Some(c) => {
                    note = None;
                    (Some(c.0), c.1, c.2)
                }
                None if identity => (None, "working_space", winfo.description.clone()),
                None => (Some(wbytes), "working_space", winfo.description.clone()),
            };
            match family {
                Family::Gray => spec.gray = bytes,
                Family::Rgb => spec.rgb = bytes,
                Family::Cmyk => spec.cmyk = bytes,
            }
            spaces.push(SpaceUse { family, source, description, working: winfo.id.clone(), note });
        }
        let (render, problem) = match previous {
            Some(p) if *p.spec() == spec => (Some(p.clone()), None),
            _ => match ColourProfiles::new(spec) {
                Ok(c) => (Some(c), None),
                Err(e) => (None, Some(format!("{e}; rendering with the built-in conversions"))),
            },
        };
        Resolved { render, spaces, problem }
    }

    /// The CMYK profile RGB and Lab separate through for `doc` under these settings (with a
    /// label), or why the built-in conversion is used.
    pub fn press_profile(&self, doc: &pdfcraft_cos::Document) -> Result<(Profile, Arc<Vec<u8>>, String), String> {
        let s = &self.settings;
        let intent = intent_code(s.intent);
        let intent_name = s.intent.name().replace('_', " ");
        if s.untagged == UntaggedPolicy::Unmanaged {
            return Err("colour management of untagged colour is off".into());
        }
        let mut why = "the document has no output intent".to_string();
        if s.untagged == UntaggedPolicy::OutputIntent
            && let Some(oi) = pdfcraft_preflight::separations::output_intent(doc)
        {
            let name = match &oi.condition {
                Some(c) => format!("{} output intent {c:?}", oi.subtype),
                None => format!("{} output intent", oi.subtype),
            };
            match oi.profile {
                None => why = format!("the {name} is not used: {}", oi.problem.unwrap_or_else(|| "it has no DestOutputProfile".into())),
                Some(bytes) => match Profile::parse(&bytes, intent) {
                    Ok(p) if p.family == Family::Cmyk && p.exact() => {
                        let described = if p.description.is_empty() { String::new() } else { format!(" ({})", p.description) };
                        return Ok((p, Arc::new(bytes), format!("the {name}'s CMYK profile{described}, {intent_name}")));
                    }
                    Ok(p) if p.family != Family::Cmyk => why = format!("the {name}'s profile is not a CMYK profile"),
                    Ok(_) => why = format!("the {name}'s profile has no BToA tables PdfCraft can use"),
                    Err(e) => why = format!("the {name}'s profile could not be read: {e}"),
                },
            }
        } else if s.untagged == UntaggedPolicy::WorkingSpace {
            why = "the colour settings use the working spaces".into();
        }
        let Some((info, bytes)) = self.working(Family::Cmyk) else { return Err(why) };
        match Profile::parse(&bytes, intent) {
            Ok(p) if p.family == Family::Cmyk && p.exact() => {
                Ok((p, bytes, format!("the working CMYK space {:?} ({}), {intent_name}", info.id, info.description)))
            }
            _ if info.id == CGATS => Err(why),
            _ => Err(format!("{why}; the working CMYK space {:?} has no BToA tables PdfCraft can use", info.id)),
        }
    }
}

/// The name a user profile is held under: `name`, or the file's stem, made of letters, digits,
/// spaces and `._-` only.
pub fn profile_name(name: Option<&str>, path: &str) -> Result<String, String> {
    let raw = match name {
        Some(n) => n.trim().to_string(),
        None => {
            let stem = std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            stem.chars().map(|c| if c.is_ascii_alphanumeric() || " ._-".contains(c) { c } else { '-' }).collect::<String>().trim().to_string()
        }
    };
    if raw.is_empty() || raw.chars().count() > MAX_PROFILE_NAME {
        return Err(format!("a profile name is 1 to {MAX_PROFILE_NAME} characters"));
    }
    if !raw.chars().all(|c| c.is_ascii_alphanumeric() || " ._-".contains(c)) {
        return Err("a profile name has only letters, digits, spaces and . _ -".into());
    }
    Ok(raw)
}

impl Session {
    /// The colour settings and the profiles they can name.
    pub fn color_management(&self) -> &ColorManagement {
        &self.color
    }

    /// Change the colour settings (session only, not an undo step). Open documents re-render
    /// with them.
    pub fn set_color_settings(&mut self, settings: ColorSettings) -> Result<(), EditError> {
        self.color.validate(&settings).map_err(EditError::Invalid)?;
        self.color.settings = settings;
        self.refresh_color();
        Ok(())
    }

    /// Hold the ICC profile `bytes` (read from `source`) as `user:<name>`, replacing one of the
    /// same name. Returns what was loaded.
    pub fn load_color_profile(&mut self, name: &str, source: &str, bytes: Vec<u8>) -> Result<ProfileInfo, EditError> {
        let name = profile_name(Some(name), source).map_err(EditError::Invalid)?;
        let max = crate::color_profile_limit();
        if bytes.len() > max {
            return Err(EditError::Invalid(format!("the profile is larger than {} MB", max >> 20)));
        }
        let parsed = crate::guard(|| Profile::parse(&bytes, 1))
            .map_err(EditError::Invalid)?
            .map_err(|e| EditError::Invalid(format!("the profile can't be used: {e}")))?;
        let family = parsed.family;
        let bytes = Arc::new(bytes);
        let (displays, converts_into) = crate::guard(|| capabilities_of(&bytes, family)).map_err(EditError::Invalid)?;
        if !displays && !converts_into {
            return Err(EditError::Invalid(format!(
                "the {} profile has no table that converts its colours (neither to the display nor into it)",
                family_id(family)
            )));
        }
        let id = format!("{USER_PREFIX}{name}");
        let replacing = self.color.user.iter().position(|e| e.info.id == id);
        if replacing.is_none() && self.color.user.len() >= MAX_USER_PROFILES {
            return Err(EditError::Invalid(format!("at most {MAX_USER_PROFILES} profiles can be loaded")));
        }
        let in_use = [&self.color.settings.rgb, &self.color.settings.cmyk, &self.color.settings.gray].contains(&&id);
        if let Some(i) = replacing
            && in_use
            && self.color.user.get(i).is_some_and(|e| e.info.family != family)
        {
            return Err(EditError::Invalid(format!("{id:?} is a working space; a replacement must be a {} profile too", family_id(family))));
        }
        let description = if parsed.description.is_empty() { name.clone() } else { parsed.description.clone() };
        let info = ProfileInfo { id, family, description, built_in: false, source: Some(source.into()), size: bytes.len(), displays, converts_into };
        let entry = Entry { info: info.clone(), bytes };
        match replacing {
            Some(i) => {
                if let Some(slot) = self.color.user.get_mut(i) {
                    *slot = entry;
                }
            }
            None => self.color.user.push(entry),
        }
        if in_use {
            self.refresh_color();
        }
        Ok(info)
    }

    /// Whether document `id` has a `GTS_PDFX` output intent.
    pub fn has_pdfx_output_intent(&self, id: DocId) -> Result<bool, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let Some(editor) = doc.editor.as_ref() else { return Ok(false) };
        Ok(crate::guard(|| pdfcraft_preflight::separations::output_intent(&editor.cos).is_some_and(|oi| oi.subtype == "GTS_PDFX")).unwrap_or(false))
    }

    /// How the colour settings resolve for document `id`.
    pub fn color_resolution(&self, id: DocId) -> Result<Vec<SpaceUse>, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        Ok(doc.color.spaces.clone())
    }

    /// Re-resolve the settings for every open document and restart their renderers.
    fn refresh_color(&mut self) {
        let color = Arc::new(self.color.clone());
        for doc in &mut self.docs {
            doc.color_management = color.clone();
            crate::resolve_doc_color(doc);
            doc.renderer = pdfcraft_render::RenderPool::new(doc.display.clone(), crate::render_threads(), doc.config.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_profiles_display_and_the_defaults_are_valid() {
        let cm = ColorManagement::default();
        let list = cm.profiles();
        for f in [Family::Rgb, Family::Cmyk, Family::Gray] {
            assert!(list.iter().any(|p| p.family == f), "{f:?}");
        }
        for p in &list {
            assert!(p.displays, "{} displays", p.id);
            if p.family != Family::Cmyk {
                assert!(p.converts_into, "{} converts into", p.id);
            }
        }
        cm.validate(&ColorSettings::default()).unwrap();
        let bad = ColorSettings { rgb: CGATS.into(), ..Default::default() };
        assert!(cm.validate(&bad).unwrap_err().contains("not a rgb profile"));
        let bad = ColorSettings { gray: "nope".into(), ..Default::default() };
        assert!(cm.validate(&bad).unwrap_err().contains("unknown gray profile"));
    }

    #[test]
    fn default_settings_render_as_the_built_in_conversions() {
        let r = ColorManagement::default().resolve(None, None);
        let c = r.render.unwrap();
        assert!(!c.manages_gray_or_rgb());
        let q = [10u8, 200, 30, 40];
        assert_eq!(c.cmyk_to_rgb(&q), pdfcraft_render::device_cmyk_to_rgb(&q));
        assert_eq!(r.spaces.len(), 3);
    }

    #[test]
    fn profile_names_are_checked() {
        assert_eq!(profile_name(None, "dir/My Press (v2).icc").unwrap(), "My Press -v2-");
        assert_eq!(profile_name(Some(" coated "), "x.icc").unwrap(), "coated");
        assert!(profile_name(Some(""), "x.icc").is_err());
        assert!(profile_name(Some("a/b"), "x.icc").is_err());
        assert!(profile_name(Some(&"x".repeat(65)), "x.icc").is_err());
    }
}

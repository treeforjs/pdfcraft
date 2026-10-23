//! Colour management for rendering: the profiles DeviceGray, DeviceRGB and DeviceCMYK are read
//! in, the rendering intent and black point compensation (the engine's colour settings,
//! resolved for one document). Conversion is done by the bootstrap renderer's colour engine
//! (moxcms, through the vendored hayro-interpret); the display is sRGB.

use std::sync::Arc;

use hayro::hayro_interpret::color::{CmykConversion, DeviceColor, DeviceColorSettings};

/// The CMYK profile DeviceCMYK renders through by default (CGATS001Compat v2 micro, CC0,
/// `ATTRIBUTION.toml`): a coated-press characterisation with one perceptual `AToB0` table.
pub fn default_cmyk_profile() -> &'static [u8] {
    hayro::hayro_interpret::color::default_cmyk_profile()
}

/// What to manage. Profiles left `None` keep the built-in conversion of that space.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColourSpec {
    /// The gray profile DeviceGray is read in; `None`: sRGB gray.
    pub gray: Option<Arc<Vec<u8>>>,
    /// The RGB profile DeviceRGB is read in; `None`: sRGB.
    pub rgb: Option<Arc<Vec<u8>>>,
    /// The CMYK profile DeviceCMYK is read in; `None`: [`default_cmyk_profile`].
    pub cmyk: Option<Arc<Vec<u8>>>,
    /// DeviceCMYK without colour management (R = (1 − C)(1 − K) …), ignoring `cmyk`.
    pub naive_cmyk: bool,
    /// ICC rendering intent: 0 perceptual, 1 relative colorimetric, 2 saturation, 3 absolute
    /// colorimetric (a profile without that table uses its default one).
    pub intent: u8,
    pub black_point_compensation: bool,
}

/// Recently prepared settings (shared: preparing is a pure function of the spec).
static PREPARED: std::sync::Mutex<Vec<ColourProfiles>> = std::sync::Mutex::new(Vec::new());
const PREPARED_MAX: usize = 8;

/// Prepared colour settings for [`crate::RenderConfig::colour`].
#[derive(Clone, Debug)]
pub struct ColourProfiles {
    device: DeviceColor,
    spec: Arc<ColourSpec>,
}

impl PartialEq for ColourProfiles {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.spec, &other.spec) || self.spec == other.spec
    }
}

impl ColourProfiles {
    /// Prepare `spec`; an error says which profile can't be used and why.
    pub fn new(spec: ColourSpec) -> Result<Self, String> {
        if spec.intent > 3 {
            return Err(format!("rendering intent {} is not 0 to 3", spec.intent));
        }
        let settings = DeviceColorSettings {
            gray: spec.gray.clone(),
            rgb: spec.rgb.clone(),
            cmyk: spec.cmyk.clone(),
            cmyk_conversion: if spec.naive_cmyk { CmykConversion::Naive } else { CmykConversion::Profile },
            intent: spec.intent,
            black_point_compensation: spec.black_point_compensation,
        };
        // Preparing a profile builds its conversion tables (milliseconds each, every document
        // open): reuse the last few preparations of the same settings.
        let cached = PREPARED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().find(|p| *p.spec == spec).cloned();
        if let Some(prepared) = cached {
            return Ok(prepared);
        }
        let device = std::panic::catch_unwind(|| DeviceColor::new(&settings)).map_err(|_| "the colour engine failed on a profile".to_string())??;
        let prepared = Self { device, spec: Arc::new(spec) };
        let mut list = PREPARED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if list.len() >= PREPARED_MAX {
            list.remove(0);
        }
        list.push(prepared.clone());
        Ok(prepared)
    }

    pub fn spec(&self) -> &ColourSpec {
        &self.spec
    }

    pub(crate) fn device(&self) -> &DeviceColor {
        &self.device
    }

    /// Whether DeviceGray or DeviceRGB is read in a profile (not plain sRGB).
    pub fn manages_gray_or_rgb(&self) -> bool {
        self.spec.gray.is_some() || self.spec.rgb.is_some()
    }

    /// DeviceCMYK quadruples (0–255 each) as RGB triples, as pages render with these settings.
    pub fn cmyk_to_rgb(&self, cmyk: &[u8]) -> Option<Vec<u8>> {
        if !cmyk.len().is_multiple_of(4) {
            return None;
        }
        let mut out = vec![0u8; cmyk.len() / 4 * 3];
        self.device.cmyk_to_rgb8(cmyk, &mut out)?;
        Some(out)
    }

    /// DeviceRGB triples as displayed (sRGB) with these settings.
    pub fn rgb_to_rgb(&self, rgb: &[u8]) -> Option<Vec<u8>> {
        let mut out = vec![0u8; rgb.len()];
        self.device.rgb_to_rgb8(rgb, &mut out)?;
        Some(out)
    }

    /// DeviceGray values as displayed RGB triples with these settings.
    pub fn gray_to_rgb(&self, gray: &[u8]) -> Option<Vec<u8>> {
        let mut out = vec![0u8; gray.len().checked_mul(3)?];
        self.device.gray_to_rgb8(gray, &mut out)?;
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_spec_matches_the_renderers_own_conversion() {
        let c = ColourProfiles::new(ColourSpec::default()).unwrap();
        let quads = [0u8, 0, 0, 255, 255, 0, 0, 0, 0, 0, 0, 0, 30, 60, 90, 20];
        assert_eq!(c.cmyk_to_rgb(&quads), crate::device_cmyk_to_rgb(&quads));
        assert_eq!(c.rgb_to_rgb(&[10, 20, 30]).unwrap(), vec![10, 20, 30]);
        assert_eq!(c.gray_to_rgb(&[77]).unwrap(), vec![77, 77, 77]);
        assert!(!c.manages_gray_or_rgb());
    }

    #[test]
    fn naive_cmyk_and_black_point_compensation_change_the_colours() {
        let naive = ColourProfiles::new(ColourSpec { naive_cmyk: true, ..Default::default() }).unwrap();
        assert_eq!(naive.cmyk_to_rgb(&[255, 0, 0, 0]).unwrap(), vec![0, 255, 255]);
        assert_eq!(naive.cmyk_to_rgb(&[0, 0, 0, 255]).unwrap(), vec![0, 0, 0]);
        // The default CMYK profile's perceptual table already maps its darkest colour to display
        // black, so black point compensation leaves it as it is.
        let plain = ColourProfiles::new(ColourSpec::default()).unwrap();
        let bpc = ColourProfiles::new(ColourSpec { black_point_compensation: true, ..Default::default() }).unwrap();
        let quads = [255u8, 255, 255, 255, 0, 0, 0, 255, 0, 0, 0, 0, 40, 80, 120, 60];
        assert_eq!(bpc.cmyk_to_rgb(&quads), plain.cmyk_to_rgb(&quads));
    }

    #[test]
    fn unusable_profiles_are_refused_without_panicking() {
        for bytes in [Vec::new(), vec![0u8; 200], b"not a profile at all".to_vec()] {
            let spec = ColourSpec { cmyk: Some(Arc::new(bytes.clone())), ..Default::default() };
            assert!(ColourProfiles::new(spec).is_err());
            let spec = ColourSpec { rgb: Some(Arc::new(bytes)), ..Default::default() };
            assert!(ColourProfiles::new(spec).is_err());
        }
        // A CMYK profile is not an RGB profile.
        let spec = ColourSpec { rgb: Some(Arc::new(default_cmyk_profile().to_vec())), ..Default::default() };
        assert!(ColourProfiles::new(spec).unwrap_err().contains("RGB"));
        assert!(ColourProfiles::new(ColourSpec { intent: 9, ..Default::default() }).is_err());
    }
}

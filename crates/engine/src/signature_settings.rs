//! Preferences → Signatures beyond the plain keys of [`crate::prefs`]: the timestamp servers,
//! saved signature appearances (text items, a picture or drawn signature, a logo), per-certificate
//! trust settings, the verification time the Signatures panel uses and the revocation policy.
//!
//! Everything here is settings data the app saves and reads back, so every value is checked
//! on the way in: lists are capped, names cut, images bounded, and a bad entry is refused with
//! a message (or skipped when loading saved settings) rather than trusted.

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine as _;
use pdfcraft_sign::appearance::{GraphicImage, InkGraphic, SignatureGraphic};
use pdfcraft_sign::{Appearance, CertTrust, Certificate, VerificationTime};
use serde::{Deserialize, Serialize};

use crate::signature_image::SignatureImage;
use crate::signature_workflow::TimestampServer;
use crate::{EditError, Session};

/// At most this many timestamp servers and saved appearances are kept.
pub const MAX_TIMESTAMP_SERVERS: usize = 32;
pub const MAX_APPEARANCES: usize = 32;
/// The longest appearance name kept.
pub const MAX_APPEARANCE_NAME: usize = 100;
/// The pictures of all saved appearances together (they are saved with the app's settings).
pub const MAX_APPEARANCE_PICTURE_BYTES: usize = 16 << 20;
/// The longest maximum age for revocation evidence without a next update (ten years, in hours).
pub const MAX_REVOCATION_AGE_HOURS: u32 = 87_600;

/// Which time the Signatures panel checks certificates and revocation at (Acrobat:
/// Verification Time: the time at which the signature was created, secure time, current time).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerifyAt {
    /// A trusted embedded timestamp, else the signer's claimed signing time.
    #[default]
    Best,
    /// The signer's claimed signing time.
    Signing,
    /// The current time.
    Current,
}

impl VerifyAt {
    pub const IDS: [&'static str; 3] = ["best", "signing", "current"];

    pub fn id(self) -> &'static str {
        match self {
            Self::Best => "best",
            Self::Signing => "signing",
            Self::Current => "current",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        [Self::Best, Self::Signing, Self::Current].into_iter().find(|v| v.id().eq_ignore_ascii_case(id.trim()))
    }
}

/// A picture for a signature, kept as the normalized PNG it came from (bounded by
/// [`crate::signature_image::MAX_SIGNATURE_IMAGE_BYTES`]).
#[derive(Clone, Debug, PartialEq)]
pub struct AppearanceImage(pub SignatureImage);

impl AppearanceImage {
    /// The picture for the signer: scaled down to the signer's size limit when larger.
    pub fn graphic(&self) -> Result<GraphicImage, EditError> {
        let [w, h] = self.0.size();
        let max = pdfcraft_sign::appearance::MAX_IMAGE_SIDE as usize;
        let to_err = |e: pdfcraft_sign::SignError| EditError::Invalid(e.to_string());
        if w <= max && h <= max {
            return GraphicImage::new(w as u32, h as u32, self.0.rgba().to_vec()).map_err(to_err);
        }
        let img = image::RgbaImage::from_raw(w as u32, h as u32, self.0.rgba().to_vec())
            .ok_or_else(|| EditError::Invalid("the signature picture's pixels don't match its size".into()))?;
        let k = max as f64 / w.max(h) as f64;
        let (nw, nh) = (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1));
        let small = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle);
        GraphicImage::new(nw, nh, small.into_raw()).map_err(to_err)
    }
}

/// The graphic of a saved appearance.
#[derive(Clone, Debug, PartialEq)]
pub enum AppearanceGraphic {
    Image(AppearanceImage),
    /// Strokes in the signer's unit box (`0..=1000`, y down).
    Drawn(Vec<Vec<[u16; 2]>>),
}

/// A named signature appearance (Acrobat: Configure Signature Appearance → Save).
#[derive(Clone, Debug, PartialEq)]
pub struct SavedAppearance {
    pub name: String,
    pub show_name: bool,
    pub date: bool,
    pub reason: bool,
    pub location: bool,
    pub distinguished_name: bool,
    pub labels: bool,
    pub graphic: Option<AppearanceGraphic>,
    pub logo: Option<AppearanceImage>,
}

impl SavedAppearance {
    /// The signer's appearance (pictures decoded and bounded).
    pub fn appearance(&self) -> Result<Appearance, EditError> {
        let to_err = |e: pdfcraft_sign::SignError| EditError::Invalid(e.to_string());
        let graphic = match &self.graphic {
            None => None,
            Some(AppearanceGraphic::Image(i)) => Some(SignatureGraphic::Image(i.graphic()?)),
            Some(AppearanceGraphic::Drawn(s)) => Some(SignatureGraphic::Ink(InkGraphic::new(s.clone()).map_err(to_err)?)),
        };
        Ok(Appearance {
            name: self.show_name,
            date: self.date,
            reason: self.reason,
            location: self.location,
            distinguished_name: self.distinguished_name,
            labels: self.labels,
            graphic,
            logo: self.logo.as_ref().map(AppearanceImage::graphic).transpose()?,
        })
    }

    /// The settings form: pictures as base64 PNG.
    pub fn to_json(&self) -> serde_json::Value {
        let b64 = |i: &AppearanceImage| base64::engine::general_purpose::STANDARD.encode(i.0.bytes().as_slice());
        serde_json::json!({
            "name": self.name,
            "show_name": self.show_name,
            "date": self.date,
            "reason": self.reason,
            "location": self.location,
            "distinguished_name": self.distinguished_name,
            "labels": self.labels,
            "graphic": match &self.graphic {
                None => serde_json::Value::Null,
                Some(AppearanceGraphic::Image(i)) => serde_json::json!({"image": b64(i)}),
                Some(AppearanceGraphic::Drawn(s)) => serde_json::json!({"strokes": s}),
            },
            "logo": self.logo.as_ref().map(b64),
        })
    }

    /// Read one saved appearance (untrusted settings): every field is checked.
    pub fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        let name = clean_name(v["name"].as_str().unwrap_or_default())?;
        let flag = |k: &str, d: bool| v[k].as_bool().unwrap_or(d);
        let image = |v: &serde_json::Value| -> Result<AppearanceImage, String> {
            let text = v.as_str().ok_or("a picture must be base64 PNG or JPEG data")?;
            // Base64 is a third bigger than the bytes; refuse before decoding something huge.
            if text.len() > crate::signature_image::MAX_SIGNATURE_IMAGE_BYTES / 3 * 4 + 8 {
                return Err("a signature picture is limited to 4 MiB".into());
            }
            let bytes = base64::engine::general_purpose::STANDARD.decode(text.trim()).map_err(|_| "a picture must be base64 PNG or JPEG data")?;
            SignatureImage::from_bytes(&bytes).map(AppearanceImage).map_err(|e| e.to_string())
        };
        let graphic = match &v["graphic"] {
            serde_json::Value::Null => None,
            g if !g["image"].is_null() => Some(AppearanceGraphic::Image(image(&g["image"])?)),
            g if !g["strokes"].is_null() => {
                let strokes: Vec<Vec<[u16; 2]>> =
                    serde_json::from_value(g["strokes"].clone()).map_err(|_| "strokes must be lists of [x, y] points from 0 to 1000")?;
                let ink = InkGraphic::new(strokes).map_err(|e| e.to_string())?;
                Some(AppearanceGraphic::Drawn(ink.strokes().to_vec()))
            }
            _ => return Err("graphic must be {image} or {strokes}".into()),
        };
        let logo = match &v["logo"] {
            serde_json::Value::Null => None,
            l => Some(image(l)?),
        };
        Ok(Self {
            name,
            show_name: flag("show_name", true),
            date: flag("date", true),
            reason: flag("reason", false),
            location: flag("location", false),
            distinguished_name: flag("distinguished_name", false),
            labels: flag("labels", true),
            graphic,
            logo,
        })
    }
}

/// A name without control characters, trimmed, 1 to [`MAX_APPEARANCE_NAME`] characters.
fn clean_name(s: &str) -> Result<String, String> {
    let (name, _) = crate::prefs::clean_text(s, MAX_APPEARANCE_NAME);
    if name.is_empty() { Err("an appearance needs a name".into()) } else { Ok(name) }
}

/// A fingerprint as typed or pasted (`AA:BB…`, `aabb…`, `AA BB …`) in the form
/// [`Certificate::fingerprint`] writes.
pub fn normalize_fingerprint(s: &str) -> Option<String> {
    let hex: String = s.chars().filter(char::is_ascii_hexdigit).take(65).collect::<String>().to_ascii_uppercase();
    if hex.len() != 64 {
        return None;
    }
    let pairs: Vec<&str> = (0..32).filter_map(|i| hex.get(i * 2..i * 2 + 2)).collect();
    Some(pairs.join(" "))
}

/// The session's signature settings.
#[derive(Clone, Debug, Default)]
pub(crate) struct SignatureSettings {
    pub(crate) verify_at: VerifyAt,
    /// 0: no maximum age.
    pub(crate) revocation_max_age_hours: u32,
    /// Fetch revocation information when adding verification information (where internet
    /// access allows; on by default, as in Acrobat).
    pub(crate) fetch_revocation_off: bool,
    pub(crate) timestamp_servers: Vec<TimestampServer>,
    /// Passwords of timestamp servers with a user name, by server name: held for the session,
    /// never saved.
    pub(crate) timestamp_passwords: HashMap<String, String>,
    pub(crate) appearances: Vec<SavedAppearance>,
}

impl Session {
    /// The verification time the Signatures panel uses.
    pub fn verify_at(&self) -> VerifyAt {
        self.signature_settings.verify_at
    }

    /// Set it and revalidate every open document.
    pub fn set_verify_at(&mut self, v: VerifyAt) {
        if v == self.signature_settings.verify_at && v != VerifyAt::Current {
            return;
        }
        self.signature_settings.verify_at = v;
        let trust = (*self.trust).clone();
        self.replace_trust(trust);
    }

    /// [`VerifyAt`] as the validator's choice, with the current time read now.
    pub(crate) fn verification_time(&self) -> VerificationTime {
        match self.signature_settings.verify_at {
            VerifyAt::Best => VerificationTime::BestAvailable,
            VerifyAt::Signing => VerificationTime::ClaimedSigningTime,
            VerifyAt::Current => VerificationTime::At(pdfcraft_sign::Time::from_unix(self.now_secs())),
        }
    }

    /// The maximum age (hours) of revocation evidence without a next update; 0 is no limit.
    pub fn revocation_max_age_hours(&self) -> u32 {
        self.signature_settings.revocation_max_age_hours
    }

    pub fn set_revocation_max_age_hours(&mut self, hours: u32) {
        let hours = hours.min(MAX_REVOCATION_AGE_HOURS);
        if hours == self.signature_settings.revocation_max_age_hours {
            return;
        }
        self.signature_settings.revocation_max_age_hours = hours;
        let trust = (*self.trust).clone();
        self.replace_trust(trust);
    }

    /// Whether adding verification information fetches revocation evidence online.
    pub fn fetch_revocation(&self) -> bool {
        !self.signature_settings.fetch_revocation_off
    }

    pub fn set_fetch_revocation(&mut self, on: bool) {
        self.signature_settings.fetch_revocation_off = !on;
    }

    /// The configured timestamp servers (none by default).
    pub fn timestamp_servers(&self) -> &[TimestampServer] {
        &self.signature_settings.timestamp_servers
    }

    /// Replace the timestamp servers: all are checked first, names must differ, at most
    /// [`MAX_TIMESTAMP_SERVERS`]. Passwords of servers no longer listed are forgotten.
    pub fn set_timestamp_servers(&mut self, servers: Vec<TimestampServer>) -> Result<(), String> {
        if servers.len() > MAX_TIMESTAMP_SERVERS {
            return Err(format!("At most {MAX_TIMESTAMP_SERVERS} timestamp servers may be configured."));
        }
        for (i, s) in servers.iter().enumerate() {
            s.validate()?;
            if servers.iter().take(i).any(|o| o.name == s.name) {
                return Err(format!("Two timestamp servers are named {:?}; give each its own name.", s.name));
            }
        }
        self.signature_settings.timestamp_passwords.retain(|name, _| servers.iter().any(|s| &s.name == name && s.username.is_some()));
        self.signature_settings.timestamp_servers = servers;
        Ok(())
    }

    /// The configured server named `name`.
    pub fn timestamp_server(&self, name: &str) -> Option<&TimestampServer> {
        self.signature_settings.timestamp_servers.iter().find(|s| s.name == name)
    }

    /// Hold (or forget, with `None`) the password of server `name` for this session.
    pub fn set_timestamp_password(&mut self, name: &str, password: Option<String>) -> Result<(), String> {
        let server = self.timestamp_server(name).ok_or_else(|| format!("No timestamp server is named {name:?}."))?;
        if server.username.is_none() {
            return Err("This timestamp server has no user name; it doesn't need a password.".into());
        }
        match password {
            Some(p) if p.len() > 1024 => return Err("The password is too long.".into()),
            Some(p) => {
                self.signature_settings.timestamp_passwords.insert(name.to_string(), p);
            }
            None => {
                self.signature_settings.timestamp_passwords.remove(name);
            }
        }
        Ok(())
    }

    /// The password held for server `name`.
    pub fn timestamp_password(&self, name: &str) -> Option<&str> {
        self.signature_settings.timestamp_passwords.get(name).map(String::as_str)
    }

    /// The saved signature appearances.
    pub fn signature_appearances(&self) -> &[SavedAppearance] {
        &self.signature_settings.appearances
    }

    /// The saved appearance named `name`.
    pub fn signature_appearance(&self, name: &str) -> Option<&SavedAppearance> {
        self.signature_settings.appearances.iter().find(|a| a.name == name)
    }

    /// Save an appearance, replacing one of the same name. The pictures are checked by
    /// building the signer's appearance once.
    pub fn save_signature_appearance(&mut self, mut a: SavedAppearance) -> Result<(), EditError> {
        a.name = clean_name(&a.name).map_err(EditError::Invalid)?;
        a.appearance()?;
        let pictures = |x: &SavedAppearance| {
            let g = match &x.graphic {
                Some(AppearanceGraphic::Image(i)) => i.0.bytes().len(),
                _ => 0,
            };
            g + x.logo.as_ref().map_or(0, |l| l.0.bytes().len())
        };
        let others: usize = self.signature_settings.appearances.iter().filter(|x| x.name != a.name).map(pictures).sum();
        if others.saturating_add(pictures(&a)) > MAX_APPEARANCE_PICTURE_BYTES {
            return Err(EditError::Invalid("Saved appearances can hold 16 MiB of pictures in all; delete one or use a smaller picture.".into()));
        }
        let list = &mut self.signature_settings.appearances;
        if let Some(x) = list.iter_mut().find(|x| x.name == a.name) {
            *x = a;
        } else if list.len() >= MAX_APPEARANCES {
            return Err(EditError::Invalid(format!("At most {MAX_APPEARANCES} signature appearances can be saved; delete one first.")));
        } else {
            list.push(a);
        }
        Ok(())
    }

    /// Delete the saved appearance named `name`; whether one was.
    pub fn delete_signature_appearance(&mut self, name: &str) -> bool {
        let before = self.signature_settings.appearances.len();
        self.signature_settings.appearances.retain(|a| a.name != name);
        before != self.signature_settings.appearances.len()
    }

    /// The trust settings of the trusted certificate with SHA-256 `fingerprint`.
    pub fn certificate_trust(&self, fingerprint: &str) -> Option<(Certificate, CertTrust)> {
        let fp = normalize_fingerprint(fingerprint)?;
        let c = self.trust.certs.iter().find(|c| c.fingerprint() == fp)?;
        Some((c.clone(), self.trust.trust_of(c)))
    }

    /// Change what the trusted certificate with `fingerprint` is trusted for, and revalidate.
    pub fn set_certificate_trust(&mut self, fingerprint: &str, settings: CertTrust) -> Result<(), String> {
        let fp = normalize_fingerprint(fingerprint).ok_or("Give the certificate's SHA-256 fingerprint (64 hexadecimal digits).")?;
        if !self.trust.certs.iter().any(|c| c.fingerprint() == fp) {
            return Err("That certificate is not in your trusted certificates.".into());
        }
        let mut trust = (*self.trust).clone();
        let mut map = (*trust.settings).clone();
        if settings == CertTrust::default() {
            map.remove(&fp);
        } else {
            map.insert(fp, settings);
        }
        trust.settings = Arc::new(map);
        self.replace_trust(trust);
        Ok(())
    }

    /// Remove the trusted certificate with `fingerprint` (and its settings); whether it was there.
    pub fn remove_trusted_certificate(&mut self, fingerprint: &str) -> bool {
        let Some(fp) = normalize_fingerprint(fingerprint) else { return false };
        let mut certs = self.trust.certs.clone();
        let before = certs.len();
        certs.retain(|c| c.fingerprint() != fp);
        if certs.len() == before {
            return false;
        }
        self.set_trusted_certificates(certs);
        true
    }

    /// Every trusted certificate's settings that differ from the default, by fingerprint
    /// (saved with the app's settings).
    pub fn certificate_trust_settings(&self) -> &HashMap<String, CertTrust> {
        &self.trust.settings
    }

    /// Restore saved settings (untrusted): fingerprints of certificates not trusted are dropped.
    pub fn load_certificate_trust_settings(&mut self, saved: &serde_json::Value) {
        let Some(list) = saved.as_array() else { return };
        let mut map = HashMap::new();
        for v in list.iter().take(4096) {
            let Some(fp) = v["sha256"].as_str().and_then(normalize_fingerprint) else { continue };
            if !self.trust.certs.iter().any(|c| c.fingerprint() == fp) {
                continue;
            }
            let flag = |k: &str| v[k].as_bool().unwrap_or(true);
            let s = CertTrust { root: flag("root"), signatures: flag("signatures"), certified_documents: flag("certified_documents") };
            if s != CertTrust::default() {
                map.insert(fp, s);
            }
        }
        let mut trust = (*self.trust).clone();
        trust.settings = Arc::new(map);
        self.replace_trust(trust);
    }

    /// The settings to save: `[{sha256, root, signatures, certified_documents}]`.
    pub fn saved_certificate_trust_settings(&self) -> serde_json::Value {
        let mut list: Vec<_> = self.trust.settings.iter().collect();
        list.sort_by(|a, b| a.0.cmp(b.0));
        serde_json::Value::Array(
            list.into_iter()
                .map(|(fp, s)| serde_json::json!({"sha256": fp, "root": s.root, "signatures": s.signatures, "certified_documents": s.certified_documents}))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_normalize_and_reject_garbage() {
        let fp = "ab".repeat(32);
        assert_eq!(normalize_fingerprint(&fp).unwrap().len(), 95);
        assert_eq!(normalize_fingerprint(&format!("AB:{}", "ab:".repeat(31))), normalize_fingerprint(&fp));
        assert!(normalize_fingerprint("abc").is_none());
        assert!(normalize_fingerprint(&"a".repeat(65)).is_none());
        assert!(normalize_fingerprint(&"é".repeat(64)).is_none());
    }

    #[test]
    fn hostile_saved_appearances_are_refused() {
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"name": "\u{0}\u{7}"}),
            serde_json::json!({"name": "x", "graphic": {"image": "not base64!"}}),
            serde_json::json!({"name": "x", "graphic": {"image": "AAAA"}}),
            serde_json::json!({"name": "x", "graphic": {"strokes": [[[1, 2, 3]]]}}),
            serde_json::json!({"name": "x", "graphic": {"strokes": []}}),
            serde_json::json!({"name": "x", "graphic": {"strokes": [[[70000, 1]]]}}),
            serde_json::json!({"name": "x", "graphic": 5}),
            serde_json::json!({"name": "x", "logo": "A".repeat(8 << 20)}),
        ] {
            assert!(SavedAppearance::from_json(&bad).is_err(), "{bad}");
        }
        let ok = SavedAppearance::from_json(&serde_json::json!({"name": "  Mine  ", "graphic": {"strokes": [[[0, 0], [1000, 1000]]]}})).unwrap();
        assert_eq!(ok.name, "Mine");
        assert_eq!(SavedAppearance::from_json(&ok.to_json()).unwrap(), ok);
    }

    #[test]
    fn appearance_lists_are_capped_and_named() {
        let mut s = Session::new();
        let a = |n: &str| SavedAppearance {
            name: n.into(),
            show_name: true,
            date: true,
            reason: false,
            location: false,
            distinguished_name: false,
            labels: true,
            graphic: None,
            logo: None,
        };
        for i in 0..MAX_APPEARANCES {
            s.save_signature_appearance(a(&format!("A{i}"))).unwrap();
        }
        assert!(s.save_signature_appearance(a("one more")).is_err());
        s.save_signature_appearance(a("A0")).unwrap();
        assert!(s.save_signature_appearance(a("   ")).is_err());
        assert!(s.delete_signature_appearance("A0"));
        assert!(!s.delete_signature_appearance("A0"));
        assert_eq!(s.signature_appearances().len(), MAX_APPEARANCES - 1);
    }
}

//! The pinned recognition models, checked byte for byte before they are used.
//!
//! Desktop packages ship the two `.rten` files that `cargo xtask models` fetched and verified
//! against the SHA-256 sums pinned in ATTRIBUTION.toml. The web app can't read those files: it
//! downloads them from its own site the first time text is recognized and keeps them in the
//! browser's storage. Either way the bytes must be exactly the pinned files, so a damaged,
//! truncated or swapped download (or a tampered browser cache) never reaches the model loader:
//! [`verify`] checks the length first (nothing is hashed or kept past [`MAX_MODEL_BYTES`]), then
//! the SHA-256 sum, and [`Ocr::from_bytes`](crate::Ocr::from_bytes) loads only verified bytes.

use sha2::{Digest, Sha256};

use crate::{DETECTION_MODEL, OcrError, RECOGNITION_MODEL};

/// One pinned model file: its name, exact length and SHA-256 sum (lowercase hex).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinnedModel {
    pub file: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// The models, as pinned in ATTRIBUTION.toml (a test keeps them in step).
pub const PINNED: [PinnedModel; 2] = [
    PinnedModel { file: DETECTION_MODEL, bytes: 2_510_284, sha256: "f15cfb56bd02c4bf478a20343986504a1f01e1665c2b3a0ad66340f054b1b5ca" },
    PinnedModel { file: RECOGNITION_MODEL, bytes: 9_716_568, sha256: "e484866d4cce403175bd8d00b128feb08ab42e208de30e42cd9889d8f1735a6e" },
];

/// No model download or cached copy larger than this is read (the pinned files are far smaller).
pub const MAX_MODEL_BYTES: u64 = 32 * 1024 * 1024;

/// Both models together, for progress.
pub fn total_bytes() -> u64 {
    PINNED.iter().map(|m| m.bytes).sum()
}

/// The pinned model called `file`.
pub fn pinned(file: &str) -> Option<&'static PinnedModel> {
    PINNED.iter().find(|m| m.file == file)
}

/// The SHA-256 sum of `data`, lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().fold(String::with_capacity(64), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Whether a download announcing `length` bytes (a Content-Length, or a stored file's size) can
/// be the pinned `file`, before any of it is read.
pub fn check_length(file: &str, length: u64) -> Result<&'static PinnedModel, OcrError> {
    let model = pinned(file).ok_or_else(|| OcrError::Load(file.into(), "not one of the text recognition models".into()))?;
    if length > MAX_MODEL_BYTES || length != model.bytes {
        return Err(OcrError::Load(file.into(), format!("{length} bytes, but the pinned model is {} bytes", model.bytes)));
    }
    Ok(model)
}

/// Check that `data` is exactly the pinned `file`: its length, then its SHA-256 sum.
pub fn verify(file: &str, data: &[u8]) -> Result<(), OcrError> {
    let model = check_length(file, data.len() as u64)?;
    let sum = sha256_hex(data);
    if sum != model.sha256 {
        return Err(OcrError::Load(file.into(), format!("checksum {sum} does not match the pinned model ({})", model.sha256)));
    }
    Ok(())
}

/// Load model bytes. Only [`verify`]d bytes reach this outside tests; malformed bytes still end
/// in an error (a panic inside the loader is contained too).
pub(crate) fn load(file: &str, data: Vec<u8>) -> Result<rten::Model, OcrError> {
    std::panic::catch_unwind(move || rten::Model::load(data))
        .map_err(|_| OcrError::Load(file.into(), "the model loader failed unexpectedly".into()))?
        .map_err(|e| OcrError::Load(file.into(), e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pins are ATTRIBUTION.toml's, which `cargo xtask models` verifies downloads against.
    #[test]
    fn pins_match_the_attribution_manifest() {
        let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../ATTRIBUTION.toml")).unwrap();
        for m in PINNED {
            let entry = manifest.split("[[fetched]]").find(|e| e.contains(&format!("file = \"{}\"", m.file))).unwrap();
            assert!(entry.contains(&format!("sha256 = \"{}\"", m.sha256)), "{} is pinned differently in ATTRIBUTION.toml", m.file);
            assert!(entry.contains("kind = \"model\""));
        }
        assert_eq!(total_bytes(), 12_226_852);
    }

    /// Installed models (`cargo xtask models`) are the pinned bytes.
    #[test]
    fn installed_models_verify() {
        let Some(models) = crate::Models::find() else {
            eprintln!("skipped: OCR models not installed");
            return;
        };
        for path in [&models.detection, &models.recognition] {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap();
            let data = std::fs::read(path).unwrap();
            if data.len() < 1000 {
                continue; // a stand-in written by another test
            }
            verify(name, &data).unwrap();
        }
    }

    #[test]
    fn wrong_length_name_or_checksum_is_refused_before_loading() {
        assert!(matches!(verify("text-detection.rten", b"model"), Err(OcrError::Load(..))));
        assert!(matches!(verify("other.rten", b"model"), Err(OcrError::Load(..))));
        assert!(check_length(DETECTION_MODEL, u64::MAX).is_err());
        assert!(check_length(DETECTION_MODEL, 2_510_283).is_err());
        assert!(check_length(DETECTION_MODEL, 2_510_284).is_ok());
        // The right length with the wrong bytes.
        let zeros = vec![0u8; 2_510_284];
        let err = verify(DETECTION_MODEL, &zeros).unwrap_err().to_string();
        assert!(err.contains("checksum"), "{err}");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    /// Hostile model bytes end in an error, never a crash (the loader is never handed them
    /// outside tests, since they can't match a pinned sum).
    #[test]
    fn hostile_model_bytes_are_an_error() {
        let mut header = b"RTEN".to_vec();
        header.extend_from_slice(&1u32.to_le_bytes());
        header.extend_from_slice(&u64::MAX.to_le_bytes());
        header.extend_from_slice(&u64::MAX.to_le_bytes());
        let mut big = header.clone();
        big.resize(4096, 0xff);
        for data in [Vec::new(), b"RTEN".to_vec(), header, big, vec![0u8; 1024], (0..=255u8).cycle().take(65_536).collect()] {
            assert!(load("text-detection.rten", data).is_err());
        }
        if let Some(models) = crate::Models::find()
            && let Ok(data) = std::fs::read(&models.detection)
            && data.len() > 1000
        {
            for cut in [16, 1000, data.len() / 2, data.len() - 1] {
                assert!(load(DETECTION_MODEL, data[..cut].to_vec()).is_err(), "truncated at {cut}");
            }
        }
    }
}

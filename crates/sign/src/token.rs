//! Signing with cryptographic tokens: smart cards, USB tokens and HSMs reached through a
//! PKCS #11 module (sign.pkcs11).
//!
//! The token is abstracted behind [`TokenProvider`] (finds and loads modules), [`TokenModule`]
//! (one loaded module: its slots) and [`TokenSession`] (one open session on a token). The real
//! implementation, over the native module, lives in `pdfcraft-platform` (the only place that
//! loads native code); [`mock`] is a software token with the same behaviour for tests. When
//! neither is available, [`Unavailable`] says so and the file-based digital IDs still work.
//!
//! The flow: [`login`] with the PIN (which is zeroed as soon as it has been used and never
//! kept), [`Login::identities`] lists the certificates that have a private key on the token, and
//! [`Login::digital_id`] turns one into a [`DigitalId`] whose key signs on the token: the private
//! key never leaves it. Everything a module returns is treated as hostile: lists, sizes and
//! labels are capped, certificates are parsed defensively, and every signature it returns is
//! checked against the certificate's public key before it goes into a document.

use std::sync::{Arc, Mutex, PoisonError};

use zeroize::Zeroize;

use crate::keys::{DigestAlg, ExternalKey, PrivateKey, PublicKey, Scheme};
use crate::{Certificate, DigitalId, SignError, der};

/// The most slots read from a module.
pub const MAX_SLOTS: usize = 64;
/// The most certificates or keys read from one token.
pub const MAX_OBJECTS: usize = 256;
/// The largest certificate read from a token (bytes).
pub const MAX_CERT_LEN: usize = 64 * 1024;
/// The largest identifier (CKA_ID) or label attribute read from a token (bytes).
pub const MAX_ATTR_LEN: usize = 4096;
/// Labels and descriptions are cut to this many characters.
pub const MAX_LABEL_CHARS: usize = 64;
/// The longest PIN passed to a token (bytes).
pub const MAX_PIN_LEN: usize = 256;
/// The longest signature accepted from a token (bytes; RSA-16384 is 2048).
pub const MAX_SIGNATURE_LEN: usize = 2048;

/// A token PIN. Its bytes are zeroed when it is dropped, and it never prints.
pub struct Pin(Vec<u8>);

impl Pin {
    /// Take the PIN out of `typed` (a text field's contents), clearing it there.
    pub fn take(typed: &mut String) -> Pin {
        let pin = Pin(typed.as_bytes().to_vec());
        typed.zeroize();
        pin
    }

    /// A PIN from raw bytes (some tokens accept non-UTF-8 PINs).
    pub fn from_bytes(bytes: Vec<u8>) -> Pin {
        Pin(bytes)
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for Pin {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for Pin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pin(…)")
    }
}

/// Why a token operation failed. Messages never include the PIN.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("signing with smart cards and tokens is not available in this build")]
    NotAvailable,
    #[error("no PKCS #11 module was found; choose the module file your card or token came with")]
    NoModule,
    #[error("the PKCS #11 module {0} was not found")]
    ModuleNotFound(String),
    #[error("the PKCS #11 module couldn't be loaded: {0}")]
    ModuleLoad(String),
    #[error("there is no slot {0} on this module")]
    NoSuchSlot(u64),
    #[error("no card or token is in this slot")]
    TokenNotPresent,
    #[error("enter the PIN")]
    PinRequired,
    #[error("the PIN is too long")]
    PinTooLong,
    #[error("{}", if *final_try { "the PIN is incorrect; one more wrong PIN locks the token" } else { "the PIN is incorrect" })]
    WrongPin { final_try: bool },
    #[error("the token's PIN is locked after too many wrong PINs; unlock it with the tool that came with the token")]
    PinLocked,
    #[error("the token's PIN has expired; change it with the tool that came with the token")]
    PinExpired,
    #[error("the PIN entry was cancelled")]
    Cancelled,
    #[error("the token is not unlocked; enter the PIN again")]
    NotLoggedIn,
    #[error("no certificate {0} with a private key is on the token")]
    NoIdentity(String),
    #[error("the token has no private key for this certificate")]
    NoKey,
    #[error("not supported by the token: {0}")]
    Unsupported(String),
    #[error("the token module returned something unusable: {0}")]
    Hostile(String),
    #[error("the token reported an error: {0}")]
    Device(String),
}

impl From<TokenError> for SignError {
    fn from(e: TokenError) -> SignError {
        SignError::Crypto(e.to_string())
    }
}

/// What a loaded module says about itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleInfo {
    pub description: String,
    pub manufacturer: String,
    pub version: String,
}

/// One slot (a card reader, a USB port, an HSM partition).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlotInfo {
    pub id: u64,
    pub description: String,
    /// The token in it, if any.
    pub token: Option<TokenInfo>,
}

/// A token (card, USB key) and the state of its PIN.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenInfo {
    pub label: String,
    pub manufacturer: String,
    pub model: String,
    pub serial: String,
    /// The token needs a PIN before its keys can be used.
    pub login_required: bool,
    /// The PIN is entered on the reader's own keypad, not in PdfCraft.
    pub protected_path: bool,
    pub pin_locked: bool,
    pub pin_count_low: bool,
    pub pin_final_try: bool,
}

/// A certificate object read from a token.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenObject {
    /// CKA_ID: what pairs a certificate with its private key.
    pub id: Vec<u8>,
    pub label: String,
    /// The certificate's DER encoding.
    pub value: Vec<u8>,
}

/// The ways a token can be asked to sign.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mechanism {
    /// CKM_RSA_PKCS over a DER `DigestInfo` (the token adds the padding).
    RsaPkcs,
    /// CKM_SHA256_RSA_PKCS and friends: the token hashes the message.
    RsaPkcsSha(DigestAlg),
    /// CKM_ECDSA over a digest; the token returns `r ‖ s`.
    Ecdsa,
    /// CKM_ECDSA_SHA256 and friends: the token hashes the message.
    EcdsaSha(DigestAlg),
}

/// One open session on a token. Implementations needn't be `Sync`: [`Login`] serialises calls.
pub trait TokenSession: Send {
    /// Log the user in with `pin`, or through the reader's keypad when `None`.
    fn login(&mut self, pin: Option<&Pin>) -> Result<(), TokenError>;
    /// The X.509 certificates on the token.
    fn certificates(&mut self) -> Result<Vec<TokenObject>, TokenError>;
    /// The CKA_IDs of the token's private keys.
    fn private_key_ids(&mut self) -> Result<Vec<Vec<u8>>, TokenError>;
    /// The signing mechanisms the token offers (empty when it doesn't say).
    fn mechanisms(&mut self) -> Result<Vec<Mechanism>, TokenError>;
    /// Sign `data` with the private key whose CKA_ID is `key_id`.
    fn sign(&mut self, key_id: &[u8], mechanism: Mechanism, data: &[u8]) -> Result<Vec<u8>, TokenError>;
    /// Log out (errors are ignored: the session closes anyway).
    fn logout(&mut self);
    /// The token's current state (to say how many PIN tries are left after a wrong one).
    fn token_info(&mut self) -> Option<TokenInfo>;
}

/// A loaded PKCS #11 module.
pub trait TokenModule: Send + Sync {
    fn info(&self) -> Result<ModuleInfo, TokenError>;
    /// The module's slots (at most [`MAX_SLOTS`] are used).
    fn slots(&self) -> Result<Vec<SlotInfo>, TokenError>;
    /// Open a session on the token in `slot`.
    fn open(&self, slot: u64) -> Result<Box<dyn TokenSession>, TokenError>;
}

/// Finds and loads PKCS #11 modules.
pub trait TokenProvider: Send + Sync {
    /// Module files found in the usual install locations (OpenSC, SoftHSM, vendor middleware).
    fn discover(&self) -> Vec<String>;
    /// Load the module at `path` (a module loaded before is reused).
    fn load(&self, path: &str) -> Result<Arc<dyn TokenModule>, TokenError>;
}

/// The provider used when no PKCS #11 support is built in (the browser, or the feature off).
#[derive(Clone, Copy, Debug, Default)]
pub struct Unavailable;

impl TokenProvider for Unavailable {
    fn discover(&self) -> Vec<String> {
        Vec::new()
    }
    fn load(&self, _path: &str) -> Result<Arc<dyn TokenModule>, TokenError> {
        Err(TokenError::NotAvailable)
    }
}

/// Load `module`, or the first module found when it is `None` or empty.
pub fn load_module(provider: &dyn TokenProvider, module: Option<&str>) -> Result<(String, Arc<dyn TokenModule>), TokenError> {
    let path = match module.map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => m.to_string(),
        None => provider.discover().into_iter().next().ok_or(TokenError::NoModule)?,
    };
    let loaded = provider.load(&path)?;
    Ok((path, loaded))
}

/// A module's slots, capped and with their texts made safe to show.
pub fn slots(module: &dyn TokenModule) -> Result<Vec<SlotInfo>, TokenError> {
    let mut out: Vec<SlotInfo> = module.slots()?.into_iter().take(MAX_SLOTS).map(clean_slot).collect();
    out.dedup_by_key(|s| s.id);
    Ok(out)
}

fn clean_slot(mut s: SlotInfo) -> SlotInfo {
    s.description = clean_label(&s.description);
    if let Some(t) = s.token.as_mut() {
        for f in [&mut t.label, &mut t.manufacturer, &mut t.model, &mut t.serial] {
            *f = clean_label(f);
        }
    }
    s
}

/// A label from a token: control characters dropped, blank-padded fields trimmed, at most
/// [`MAX_LABEL_CHARS`] characters.
pub fn clean_label(s: &str) -> String {
    let cleaned: String = s.chars().filter(|c| !c.is_control()).take(MAX_LABEL_CHARS.saturating_mul(4)).collect();
    cleaned.trim().chars().take(MAX_LABEL_CHARS).collect()
}

/// A session with its user logged in; logs out when the last user of it is gone.
struct SessionCell {
    session: Mutex<Box<dyn TokenSession>>,
}

impl SessionCell {
    fn with<T>(&self, f: impl FnOnce(&mut dyn TokenSession) -> Result<T, TokenError>) -> Result<T, TokenError> {
        let mut guard = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        // A module that panics through the wrapper must not take the app down.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(guard.as_mut())))
            .unwrap_or_else(|_| Err(TokenError::Device("the token module failed".into())))
    }
}

impl Drop for SessionCell {
    fn drop(&mut self) {
        let mut guard = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| guard.logout()));
    }
}

/// An unlocked token.
#[derive(Clone)]
pub struct Login {
    cell: Arc<SessionCell>,
    pub slot: u64,
    pub token: TokenInfo,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Login(slot {}, {:?})", self.slot, self.token.label)
    }
}

/// Unlock the token in `slot` with `pin` (`None` or an empty PIN: the reader's keypad, when the
/// token has one, or no PIN when the token needs none). The PIN is zeroed before this returns.
/// A token whose PIN is already locked is not tried again.
pub fn login(module: &dyn TokenModule, slot: u64, pin: Option<Pin>) -> Result<Login, TokenError> {
    let pin = pin.filter(|p| !p.is_empty());
    let info = slots(module)?.into_iter().find(|s| s.id == slot).ok_or(TokenError::NoSuchSlot(slot))?;
    let token = info.token.ok_or(TokenError::TokenNotPresent)?;
    if token.pin_locked {
        return Err(TokenError::PinLocked);
    }
    if pin.as_ref().is_some_and(|p| p.expose().len() > MAX_PIN_LEN) {
        return Err(TokenError::PinTooLong);
    }
    if token.login_required && pin.is_none() && !token.protected_path {
        return Err(TokenError::PinRequired);
    }
    let session = module.open(slot)?;
    let cell = Arc::new(SessionCell { session: Mutex::new(session) });
    if token.login_required {
        let result = cell.with(|s| s.login(pin.as_ref()));
        drop(pin);
        if let Err(e) = result {
            return Err(match e {
                // Say whether the next wrong PIN locks the token.
                TokenError::WrongPin { .. } => match cell.with(|s| Ok(s.token_info())).ok().flatten() {
                    Some(t) if t.pin_locked => TokenError::PinLocked,
                    Some(t) => TokenError::WrongPin { final_try: t.pin_final_try },
                    None => TokenError::WrongPin { final_try: false },
                },
                e => e,
            });
        }
    }
    Ok(Login { cell, slot, token })
}

/// A signing identity on a token: a certificate with a private key.
#[derive(Clone, Debug, PartialEq)]
pub struct TokenIdentity {
    /// `pkcs11:<SHA-256 of the certificate>`.
    pub reference: String,
    pub label: String,
    pub certificate: Certificate,
    /// The CA certificates on the token that issued it (signer's issuer first).
    pub chain: Vec<Certificate>,
    key_id: Vec<u8>,
}

/// A certificate on the token that can't be used for signing, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub label: String,
    pub reason: &'static str,
}

/// What [`Login::identities`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Identities {
    pub ids: Vec<TokenIdentity>,
    pub skipped: Vec<Skipped>,
}

/// The `pkcs11:<SHA-256 of the certificate DER>` reference kept for a token identity.
pub fn reference(c: &Certificate) -> String {
    let d = DigestAlg::Sha256.digest(&[&c.raw]);
    format!("pkcs11:{}", d.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// The `r ‖ s` half-length of an ECDSA signature with this key, or `None` for other keys.
fn ec_field_len(k: &PublicKey) -> Option<usize> {
    match k {
        PublicKey::P256(_) | PublicKey::BrainpoolP256(_) => Some(32),
        PublicKey::P384(_) | PublicKey::BrainpoolP384(_) => Some(48),
        PublicKey::BrainpoolP512(_) => Some(64),
        PublicKey::P521(_) => Some(66),
        _ => None,
    }
}

impl Login {
    /// The certificates on the token that have a private key, and those left out.
    pub fn identities(&self) -> Result<Identities, TokenError> {
        let certs: Vec<TokenObject> = self.cell.with(|s| s.certificates())?.into_iter().take(MAX_OBJECTS).collect();
        let keys: Vec<Vec<u8>> = self.cell.with(|s| s.private_key_ids())?.into_iter().take(MAX_OBJECTS).collect();
        let mut out = Identities::default();
        let mut parsed: Vec<(TokenObject, Certificate)> = Vec::new();
        for obj in certs {
            let label = clean_label(&obj.label);
            if obj.value.len() > MAX_CERT_LEN || obj.id.len() > MAX_ATTR_LEN {
                out.skipped.push(Skipped { label, reason: "too large" });
                continue;
            }
            match Certificate::parse(&obj.value) {
                Ok(c) if !parsed.iter().any(|(_, p)| p.raw == c.raw) => parsed.push((TokenObject { label, ..obj }, c)),
                Ok(_) => {}
                Err(_) => out.skipped.push(Skipped { label, reason: "unreadable certificate" }),
            }
        }
        let pool: Vec<Certificate> = parsed.iter().map(|(_, c)| c.clone()).collect();
        let mut keyless = Vec::new();
        for (obj, cert) in &parsed {
            let has_key = keys.contains(&obj.id);
            let label = if obj.label.is_empty() { cert.display_name() } else { obj.label.clone() };
            let reason = if !has_key {
                keyless.push((label, cert));
                continue;
            } else if !matches!(cert.public_key, PublicKey::Rsa { .. }) && ec_field_len(&cert.public_key).is_none() {
                Some("key type not supported")
            } else if cert.key_usage.is_some_and(|ku| ku & 0b11 == 0) {
                Some("not for signing")
            } else {
                None
            };
            if let Some(reason) = reason {
                out.skipped.push(Skipped { label, reason });
                continue;
            }
            let chain = crate::x509::build_chain(cert, &pool, None).into_iter().skip(1).cloned().collect();
            out.ids.push(TokenIdentity { reference: reference(cert), label, certificate: cert.clone(), chain, key_id: obj.id.clone() });
        }
        // Certificates without a key that complete an identity's chain are there for that.
        for (label, cert) in keyless {
            if !out.ids.iter().any(|i| i.chain.iter().any(|c| c.raw == cert.raw)) {
                out.skipped.push(Skipped { label, reason: "no private key on the token" });
            }
        }
        Ok(out)
    }

    /// A digital ID that signs with `id`'s key on this token.
    pub fn digital_id(&self, id: &TokenIdentity) -> DigitalId {
        let public = id.certificate.public_key.clone();
        let key = TokenKey { cell: self.cell.clone(), key_id: id.key_id.clone(), public: public.clone() };
        DigitalId {
            key: PrivateKey::external(public, Arc::new(key)),
            certificate: id.certificate.clone(),
            chain: id.chain.clone(),
            friendly_name: Some(id.label.clone()),
        }
    }

    /// The identity a `pkcs11:` reference, a bare fingerprint or a common name names.
    pub fn find(&self, reference_or_name: &str) -> Result<DigitalId, TokenError> {
        let want = reference_or_name.trim();
        let bare = want.strip_prefix("pkcs11:").unwrap_or(want);
        let ids = self.identities()?.ids;
        ids.iter()
            .find(|i| i.reference == want || i.reference.strip_prefix("pkcs11:") == Some(&bare.to_ascii_lowercase()))
            .or_else(|| ids.iter().find(|i| i.certificate.subject.common_name() == Some(bare) || i.label == bare))
            .map(|i| self.digital_id(i))
            .ok_or_else(|| TokenError::NoIdentity(clean_label(want)))
    }
}

/// A private key on a token.
struct TokenKey {
    cell: Arc<SessionCell>,
    key_id: Vec<u8>,
    public: PublicKey,
}

impl TokenKey {
    fn sign_on_token(&self, alg: DigestAlg, msg: &[u8]) -> Result<Vec<u8>, TokenError> {
        let digest = alg.digest(&[msg]);
        let offered = self.cell.with(|s| s.mechanisms()).unwrap_or_default();
        // A token that doesn't list its mechanisms gets the raw ones every token has.
        let has = |m: Mechanism| offered.is_empty() || offered.contains(&m);
        let rsa = matches!(self.public, PublicKey::Rsa { .. });
        let (mechanism, data) = if rsa {
            if has(Mechanism::RsaPkcs) {
                (Mechanism::RsaPkcs, crate::rsa_pad::digest_info(alg, &digest, true))
            } else if offered.contains(&Mechanism::RsaPkcsSha(alg)) {
                (Mechanism::RsaPkcsSha(alg), msg.to_vec())
            } else {
                return Err(TokenError::Unsupported(format!("RSA PKCS #1 v1.5 signing with {}", alg.name())));
            }
        } else if has(Mechanism::Ecdsa) {
            (Mechanism::Ecdsa, digest.clone())
        } else if offered.contains(&Mechanism::EcdsaSha(alg)) {
            (Mechanism::EcdsaSha(alg), msg.to_vec())
        } else {
            return Err(TokenError::Unsupported(format!("ECDSA signing with {}", alg.name())));
        };
        let raw = self.cell.with(|s| s.sign(&self.key_id, mechanism, &data))?;
        if raw.is_empty() || raw.len() > MAX_SIGNATURE_LEN {
            return Err(TokenError::Hostile(format!("a signature of {} bytes", raw.len())));
        }
        let (scheme, sig) = match &self.public {
            PublicKey::Rsa { n, .. } => {
                let k = n.iter().skip_while(|b| **b == 0).count();
                if raw.len() > k {
                    return Err(TokenError::Hostile("a signature longer than the key".into()));
                }
                // Some tokens drop leading zero bytes.
                let mut sig = vec![0u8; k.saturating_sub(raw.len())];
                sig.extend_from_slice(&raw);
                (Scheme::RsaPkcs1, sig)
            }
            other => {
                let field = ec_field_len(other).ok_or_else(|| TokenError::Unsupported(other.describe()))?;
                if raw.len() != field.saturating_mul(2) {
                    return Err(TokenError::Hostile(format!("an ECDSA signature of {} bytes", raw.len())));
                }
                let (r, s) = raw.split_at(field);
                (Scheme::Ecdsa, der::seq(&[&der::uint(r), &der::uint(s)]))
            }
        };
        // Never put a signature in the document that the certificate doesn't verify.
        match self.public.verify(scheme, alg, &digest, &sig) {
            Ok(true) => Ok(sig),
            _ => Err(TokenError::Hostile("a signature that doesn't match the certificate".into())),
        }
    }
}

impl ExternalKey for TokenKey {
    fn sign(&self, alg: DigestAlg, msg: &[u8]) -> Result<Vec<u8>, SignError> {
        self.sign_on_token(alg, msg).map_err(SignError::from)
    }
}

pub mod mock {
    //! A software token for tests and demos: slots, PINs with a retry counter, certificates and
    //! keys, and switches that make it misbehave like a broken or hostile module. Its keys are in
    //! memory, so it is no substitute for a real token.

    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, PoisonError};

    use super::{Mechanism, ModuleInfo, Pin, SlotInfo, TokenError, TokenInfo, TokenModule, TokenObject, TokenProvider, TokenSession};
    use crate::PrivateKey;
    use crate::keys::PublicKey;

    /// Ways the module misbehaves.
    #[derive(Clone, Debug, Default)]
    pub struct Hostile {
        /// Report this many extra empty slots.
        pub slot_flood: usize,
        /// Return random bytes of the right length as a signature.
        pub garbage_signature: bool,
        /// Return a signature far too long.
        pub oversized_signature: bool,
        /// Add a certificate object that isn't DER.
        pub junk_certificate: bool,
        /// Give every certificate a huge label full of control characters.
        pub huge_label: bool,
        /// Fail listing certificates.
        pub fail_certificates: bool,
        /// Panic while signing (a module crashing through its wrapper).
        pub panic_on_sign: bool,
    }

    /// A card in a slot.
    #[derive(Clone, Debug)]
    pub struct MockToken {
        pub label: String,
        pub serial: String,
        pin: Vec<u8>,
        pub tries_left: u8,
        pub login_required: bool,
        pub protected_path: bool,
        pub certificates: Vec<TokenObject>,
        keys: Vec<(Vec<u8>, Arc<PrivateKey>)>,
    }

    impl MockToken {
        pub fn new(label: &str, pin: &str) -> MockToken {
            MockToken {
                label: label.into(),
                serial: "0001".into(),
                pin: pin.as_bytes().to_vec(),
                tries_left: 3,
                login_required: true,
                protected_path: false,
                certificates: Vec::new(),
                keys: Vec::new(),
            }
        }

        /// Put a certificate on the token under `id`.
        pub fn with_certificate(mut self, id: &[u8], label: &str, der: &[u8]) -> MockToken {
            self.certificates.push(TokenObject { id: id.to_vec(), label: label.into(), value: der.to_vec() });
            self
        }

        /// Put a private key on the token under `id`.
        pub fn with_key(mut self, id: &[u8], key: PrivateKey) -> MockToken {
            self.keys.push((id.to_vec(), Arc::new(key)));
            self
        }

        fn info(&self) -> TokenInfo {
            TokenInfo {
                label: self.label.clone(),
                manufacturer: "PdfCraft".into(),
                model: "Software token".into(),
                serial: self.serial.clone(),
                login_required: self.login_required,
                protected_path: self.protected_path,
                pin_locked: self.tries_left == 0,
                pin_count_low: self.tries_left < 3,
                pin_final_try: self.tries_left == 1,
            }
        }
    }

    #[derive(Debug, Default)]
    struct State {
        slots: Vec<(u64, Option<MockToken>)>,
        logins: usize,
        signatures: usize,
    }

    /// A module with slots.
    #[derive(Debug, Default)]
    pub struct MockModule {
        state: Arc<Mutex<State>>,
        pub hostile: Hostile,
    }

    impl MockModule {
        pub fn new() -> MockModule {
            MockModule::default()
        }

        /// Add a slot, empty or with a token.
        pub fn with_slot(self, id: u64, token: Option<MockToken>) -> MockModule {
            self.state.lock().unwrap_or_else(PoisonError::into_inner).slots.push((id, token));
            self
        }

        pub fn hostile(mut self, hostile: Hostile) -> MockModule {
            self.hostile = hostile;
            self
        }

        /// How many logins were attempted and signatures made (for tests).
        pub fn counts(&self) -> (usize, usize) {
            let s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            (s.logins, s.signatures)
        }

        /// The PIN tries left on the token in `slot`.
        pub fn tries_left(&self, slot: u64) -> Option<u8> {
            let s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            s.slots.iter().find(|(id, _)| *id == slot).and_then(|(_, t)| t.as_ref()).map(|t| t.tries_left)
        }
    }

    impl TokenModule for MockModule {
        fn info(&self) -> Result<ModuleInfo, TokenError> {
            Ok(ModuleInfo { description: "PdfCraft software token".into(), manufacturer: "PdfCraft".into(), version: "1.0".into() })
        }

        fn slots(&self) -> Result<Vec<SlotInfo>, TokenError> {
            let s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let mut out: Vec<SlotInfo> = s
                .slots
                .iter()
                .map(|(id, t)| SlotInfo { id: *id, description: format!("Reader {id}"), token: t.as_ref().map(MockToken::info) })
                .collect();
            for i in 0..self.hostile.slot_flood {
                out.push(SlotInfo { id: 10_000u64.saturating_add(i as u64), description: "\u{7}flood\u{0}".repeat(100), token: None });
            }
            Ok(out)
        }

        fn open(&self, slot: u64) -> Result<Box<dyn TokenSession>, TokenError> {
            let s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            match s.slots.iter().find(|(id, _)| *id == slot) {
                None => Err(TokenError::NoSuchSlot(slot)),
                Some((_, None)) => Err(TokenError::TokenNotPresent),
                Some(_) => Ok(Box::new(MockSession { state: self.state.clone(), slot, hostile: self.hostile.clone(), logged_in: false })),
            }
        }
    }

    struct MockSession {
        state: Arc<Mutex<State>>,
        slot: u64,
        hostile: Hostile,
        logged_in: bool,
    }

    impl MockSession {
        fn token<T>(&self, f: impl FnOnce(&mut MockToken) -> T) -> Result<T, TokenError> {
            let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let slot = self.slot;
            s.slots.iter_mut().find(|(id, _)| *id == slot).and_then(|(_, t)| t.as_mut()).map(f).ok_or(TokenError::TokenNotPresent)
        }

        fn check_login(&self) -> Result<(), TokenError> {
            let required = self.token(|t| t.login_required)?;
            if required && !self.logged_in { Err(TokenError::NotLoggedIn) } else { Ok(()) }
        }
    }

    impl TokenSession for MockSession {
        fn login(&mut self, pin: Option<&Pin>) -> Result<(), TokenError> {
            self.state.lock().unwrap_or_else(PoisonError::into_inner).logins += 1;
            let ok = self.token(|t| {
                if t.tries_left == 0 {
                    return Err(TokenError::PinLocked);
                }
                let matches = match pin {
                    Some(p) => p.expose() == t.pin.as_slice(),
                    // The keypad: the mock's "user" types the right PIN.
                    None if t.protected_path => true,
                    None => return Err(TokenError::PinRequired),
                };
                if matches {
                    t.tries_left = 3;
                    Ok(())
                } else {
                    t.tries_left = t.tries_left.saturating_sub(1);
                    Err(if t.tries_left == 0 { TokenError::PinLocked } else { TokenError::WrongPin { final_try: t.tries_left == 1 } })
                }
            })?;
            ok?;
            self.logged_in = true;
            Ok(())
        }

        fn certificates(&mut self) -> Result<Vec<TokenObject>, TokenError> {
            if self.hostile.fail_certificates {
                return Err(TokenError::Device("CKR_DEVICE_ERROR".into()));
            }
            let mut certs = self.token(|t| t.certificates.clone())?;
            if self.hostile.huge_label {
                for c in &mut certs {
                    c.label = format!("{}\u{1b}[31m{}", "\n".repeat(50), "A".repeat(100_000));
                }
            }
            if self.hostile.junk_certificate {
                certs.push(TokenObject { id: vec![0xEE], label: "junk".into(), value: vec![0x30, 0x84, 0xFF, 0xFF, 0xFF, 0xFF, 1, 2, 3] });
            }
            Ok(certs)
        }

        fn private_key_ids(&mut self) -> Result<Vec<Vec<u8>>, TokenError> {
            self.check_login()?;
            self.token(|t| t.keys.iter().map(|(id, _)| id.clone()).collect())
        }

        fn mechanisms(&mut self) -> Result<Vec<Mechanism>, TokenError> {
            use crate::keys::DigestAlg::{Sha256, Sha384, Sha512};
            // Like many cards: RSA only with on-token hashing, ECDSA only over a digest.
            Ok(vec![Mechanism::RsaPkcsSha(Sha256), Mechanism::RsaPkcsSha(Sha384), Mechanism::RsaPkcsSha(Sha512), Mechanism::Ecdsa])
        }

        // `panic_on_sign` stands for a module crashing through its wrapper.
        #[allow(clippy::panic)]
        fn sign(&mut self, key_id: &[u8], mechanism: Mechanism, data: &[u8]) -> Result<Vec<u8>, TokenError> {
            self.check_login()?;
            if self.hostile.panic_on_sign {
                panic!("mock token module crashed");
            }
            let key = self.token(|t| t.keys.iter().find(|(id, _)| id == key_id).map(|(_, k)| k.clone()))?.ok_or(TokenError::NoKey)?;
            let sig = match (mechanism, key.public_key()) {
                (Mechanism::RsaPkcsSha(alg), PublicKey::Rsa { .. }) => key.sign(alg, data).map_err(|e| TokenError::Device(e.to_string()))?,
                (Mechanism::Ecdsa, PublicKey::P256(_) | PublicKey::P384(_)) => {
                    key.sign_digest_ec(data).map_err(|e| TokenError::Device(e.to_string()))?
                }
                _ => return Err(TokenError::Device("CKR_MECHANISM_INVALID".into())),
            };
            self.state.lock().unwrap_or_else(PoisonError::into_inner).signatures += 1;
            if self.hostile.oversized_signature {
                return Ok(vec![0x5A; 1 << 20]);
            }
            if self.hostile.garbage_signature {
                return Ok(sig.iter().enumerate().map(|(i, b)| b ^ (i as u8 | 1)).collect());
            }
            Ok(sig)
        }

        fn logout(&mut self) {
            self.logged_in = false;
        }

        fn token_info(&mut self) -> Option<TokenInfo> {
            self.token(|t| t.info()).ok()
        }
    }

    /// A provider with named mock modules.
    #[derive(Default)]
    pub struct MockProvider {
        modules: HashMap<String, Arc<MockModule>>,
        order: Vec<String>,
    }

    impl MockProvider {
        pub fn new() -> MockProvider {
            MockProvider::default()
        }

        /// Offer `module` at `path` (discovered in the order added).
        pub fn with_module(mut self, path: &str, module: MockModule) -> MockProvider {
            self.order.push(path.to_string());
            self.modules.insert(path.to_string(), Arc::new(module));
            self
        }

        pub fn module(&self, path: &str) -> Option<Arc<MockModule>> {
            self.modules.get(path).cloned()
        }
    }

    impl TokenProvider for MockProvider {
        fn discover(&self) -> Vec<String> {
            self.order.clone()
        }

        fn load(&self, path: &str) -> Result<Arc<dyn TokenModule>, TokenError> {
            match self.modules.get(path) {
                Some(m) => Ok(m.clone()),
                None => Err(TokenError::ModuleNotFound(path.to_string())),
            }
        }
    }
}

//! PKCS #11 modules (smart cards, USB tokens, HSMs) for signing (sign.pkcs11), through the
//! `cryptoki` safe wrapper. This is the only place PdfCraft loads a token's native module; the
//! rest of the app sees it through `pdfcraft_sign::token`'s traits, and without the `pkcs11`
//! feature (or in the browser) [`provider`] returns one that reports token signing unavailable.
//!
//! A module is loaded only when the user names it or picks one found in the usual install
//! locations, and stays loaded for the rest of the session (PKCS #11 allows one `C_Initialize`
//! per process). Everything it returns is capped (see the `MAX_*` limits in
//! `pdfcraft_sign::token`), and calls run under `catch_unwind`. A native module runs inside the
//! process, so a module that corrupts memory can still bring it down: only use modules you trust.

use std::sync::Arc;

use pdfcraft_sign::token::TokenProvider;

/// The provider the app uses: native PKCS #11 when built in, otherwise one that says it isn't.
pub fn provider() -> Arc<dyn TokenProvider> {
    #[cfg(all(feature = "pkcs11", not(target_arch = "wasm32")))]
    return Arc::new(native::Native);
    #[cfg(not(all(feature = "pkcs11", not(target_arch = "wasm32"))))]
    Arc::new(pdfcraft_sign::token::Unavailable)
}

/// Where PKCS #11 modules are usually installed on this platform (OpenSC, p11-kit, SoftHSM and
/// common vendor middleware), in the order they are offered.
pub fn common_module_paths() -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    return &[
        "/Library/OpenSC/lib/opensc-pkcs11.so",
        "/opt/homebrew/lib/opensc-pkcs11.so",
        "/usr/local/lib/opensc-pkcs11.so",
        "/usr/local/lib/libykcs11.dylib",
        "/opt/homebrew/lib/libykcs11.dylib",
        "/usr/local/lib/libeTPkcs11.dylib",
        "/opt/homebrew/lib/softhsm/libsofthsm2.so",
        "/usr/local/lib/softhsm/libsofthsm2.so",
    ];
    #[cfg(target_os = "windows")]
    return &[
        "C:\\Windows\\System32\\opensc-pkcs11.dll",
        "C:\\Program Files\\OpenSC Project\\OpenSC\\pkcs11\\opensc-pkcs11.dll",
        "C:\\Program Files\\Yubico\\Yubico PIV Tool\\bin\\libykcs11.dll",
        "C:\\Windows\\System32\\eTPKCS11.dll",
        "C:\\SoftHSM2\\lib\\softhsm2-x64.dll",
    ];
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    &[
        "/usr/lib/x86_64-linux-gnu/opensc-pkcs11.so",
        "/usr/lib/aarch64-linux-gnu/opensc-pkcs11.so",
        "/usr/lib64/opensc-pkcs11.so",
        "/usr/lib/opensc-pkcs11.so",
        "/usr/local/lib/opensc-pkcs11.so",
        "/usr/lib/x86_64-linux-gnu/p11-kit-proxy.so",
        "/usr/lib64/p11-kit-proxy.so",
        "/usr/lib/x86_64-linux-gnu/libykcs11.so",
        "/usr/lib/libeTPkcs11.so",
        "/usr/lib/softhsm/libsofthsm2.so",
        "/usr/lib/x86_64-linux-gnu/softhsm/libsofthsm2.so",
        "/usr/lib64/softhsm/libsofthsm2.so",
        "/usr/local/lib/softhsm/libsofthsm2.so",
    ]
}

/// The common module paths that exist here (regular files; nothing is loaded).
pub fn discover() -> Vec<String> {
    #[cfg(target_arch = "wasm32")]
    return Vec::new();
    #[cfg(not(target_arch = "wasm32"))]
    common_module_paths().iter().filter(|p| std::path::Path::new(p).is_file()).map(|p| p.to_string()).collect()
}

#[cfg(all(feature = "pkcs11", not(target_arch = "wasm32")))]
mod native {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};

    use cryptoki::context::{CInitializeArgs, CInitializeFlags, Pkcs11};
    use cryptoki::error::{Error, RvError};
    use cryptoki::mechanism::{Mechanism as CkMechanism, MechanismType};
    use cryptoki::object::{Attribute, AttributeInfo, AttributeType, CertificateType, ObjectClass, ObjectHandle};
    use cryptoki::session::{Session, UserType};
    use cryptoki::slot::Slot;
    use cryptoki::types::RawAuthPin;
    use pdfcraft_sign::keys::DigestAlg;
    use pdfcraft_sign::token::{
        MAX_ATTR_LEN, MAX_CERT_LEN, MAX_OBJECTS, MAX_SLOTS, Mechanism, ModuleInfo, Pin, SlotInfo, TokenError, TokenInfo, TokenModule, TokenObject,
        TokenProvider, TokenSession,
    };

    pub struct Native;

    /// Modules loaded this session, by canonical path.
    fn loaded() -> &'static Mutex<HashMap<PathBuf, Arc<Module>>> {
        static LOADED: OnceLock<Mutex<HashMap<PathBuf, Arc<Module>>>> = OnceLock::new();
        LOADED.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Run a call into the module, turning a panic in the wrapper into an error.
    fn guarded<T>(f: impl FnOnce() -> Result<T, TokenError>) -> Result<T, TokenError> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err(TokenError::Device("the token module failed".into())))
    }

    fn map_err(e: Error) -> TokenError {
        match e {
            Error::Pkcs11(rv, _) => match rv {
                RvError::PinIncorrect => TokenError::WrongPin { final_try: false },
                RvError::PinLocked => TokenError::PinLocked,
                RvError::PinExpired => TokenError::PinExpired,
                RvError::PinLenRange | RvError::PinInvalid => TokenError::WrongPin { final_try: false },
                RvError::TokenNotPresent | RvError::DeviceRemoved | RvError::TokenNotRecognized => TokenError::TokenNotPresent,
                RvError::FunctionCanceled | RvError::Cancel => TokenError::Cancelled,
                RvError::UserNotLoggedIn => TokenError::NotLoggedIn,
                RvError::SlotIdInvalid => TokenError::Device("no such slot".into()),
                other => TokenError::Device(other.to_string().chars().take(200).collect()),
            },
            Error::LibraryLoading(e) => TokenError::ModuleLoad(e.to_string()),
            Error::MissingSymbol(s) => TokenError::ModuleLoad(format!("it has no {s}; it is not a PKCS #11 module")),
            Error::NullFunctionPointer => TokenError::Unsupported("a function the module doesn't provide".into()),
            other => TokenError::Hostile(other.to_string()),
        }
    }

    impl TokenProvider for Native {
        fn discover(&self) -> Vec<String> {
            super::discover()
        }

        fn load(&self, path: &str) -> Result<Arc<dyn TokenModule>, TokenError> {
            let p = Path::new(path);
            if !p.is_file() {
                return Err(TokenError::ModuleNotFound(path.to_string()));
            }
            let canonical = p.canonicalize().map_err(|_| TokenError::ModuleNotFound(path.to_string()))?;
            let mut map = loaded().lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(m) = map.get(&canonical) {
                return Ok(m.clone());
            }
            let ctx = guarded(|| {
                let ctx = Pkcs11::new(&canonical).map_err(map_err)?;
                match ctx.initialize(CInitializeArgs::new(CInitializeFlags::OS_LOCKING_OK)) {
                    Ok(()) | Err(Error::Pkcs11(RvError::CryptokiAlreadyInitialized, _)) => Ok(ctx),
                    Err(e) => Err(map_err(e)),
                }
            })?;
            let module = Arc::new(Module { ctx });
            map.insert(canonical, module.clone());
            Ok(module)
        }
    }

    struct Module {
        ctx: Pkcs11,
    }

    fn version(v: cryptoki::types::Version) -> String {
        v.to_string()
    }

    impl TokenModule for Module {
        fn info(&self) -> Result<ModuleInfo, TokenError> {
            guarded(|| {
                let i = self.ctx.get_library_info().map_err(map_err)?;
                Ok(ModuleInfo {
                    description: i.library_description().to_string(),
                    manufacturer: i.manufacturer_id().to_string(),
                    version: version(i.library_version()),
                })
            })
        }

        fn slots(&self) -> Result<Vec<SlotInfo>, TokenError> {
            guarded(|| {
                let slots = self.ctx.get_all_slots().map_err(map_err)?;
                let mut out = Vec::new();
                for slot in slots.into_iter().take(MAX_SLOTS) {
                    let Ok(info) = self.ctx.get_slot_info(slot) else { continue };
                    let token = if info.token_present() { self.ctx.get_token_info(slot).ok().map(|t| token_info(&t)) } else { None };
                    out.push(SlotInfo { id: slot.id(), description: info.slot_description().to_string(), token });
                }
                Ok(out)
            })
        }

        fn open(&self, slot: u64) -> Result<Box<dyn TokenSession>, TokenError> {
            guarded(|| {
                let slot = Slot::try_from(slot).map_err(|_| TokenError::NoSuchSlot(slot))?;
                let session = self.ctx.open_ro_session(slot).map_err(map_err)?;
                Ok(Box::new(NativeSession { ctx: self.ctx.clone(), slot, session, mechanisms: None }) as Box<dyn TokenSession>)
            })
        }
    }

    fn token_info(t: &cryptoki::slot::TokenInfo) -> TokenInfo {
        TokenInfo {
            label: t.label().to_string(),
            manufacturer: t.manufacturer_id().to_string(),
            model: t.model().to_string(),
            serial: t.serial_number().to_string(),
            login_required: t.login_required(),
            protected_path: t.protected_authentication_path(),
            pin_locked: t.user_pin_locked(),
            pin_count_low: t.user_pin_count_low(),
            pin_final_try: t.user_pin_final_try(),
        }
    }

    struct NativeSession {
        ctx: Pkcs11,
        slot: Slot,
        session: Session,
        mechanisms: Option<Vec<Mechanism>>,
    }

    impl NativeSession {
        /// At most [`MAX_OBJECTS`] objects matching `template`.
        fn find(&self, template: &[Attribute]) -> Result<Vec<ObjectHandle>, TokenError> {
            let mut out = Vec::new();
            for h in self.session.iter_objects(template).map_err(map_err)?.take(MAX_OBJECTS) {
                out.push(h.map_err(map_err)?);
            }
            Ok(out)
        }

        /// The attributes `types` of `object`, refusing values larger than `cap` bytes before
        /// they are read (the module chooses the sizes).
        fn attributes(&self, object: ObjectHandle, types: &[AttributeType], cap: usize) -> Result<Vec<Attribute>, TokenError> {
            let sizes = self.session.get_attribute_info(object, types).map_err(map_err)?;
            let wanted: Vec<AttributeType> = types
                .iter()
                .zip(&sizes)
                .filter_map(|(t, s)| match s {
                    AttributeInfo::Available(n) if *n <= cap => Some(*t),
                    _ => None,
                })
                .collect();
            if wanted.is_empty() {
                return Ok(Vec::new());
            }
            self.session.get_attributes(object, &wanted).map_err(map_err)
        }

        fn key(&self, id: &[u8]) -> Result<ObjectHandle, TokenError> {
            self.find(&[Attribute::Class(ObjectClass::PRIVATE_KEY), Attribute::Id(id.to_vec())])?.into_iter().next().ok_or(TokenError::NoKey)
        }
    }

    impl TokenSession for NativeSession {
        fn login(&mut self, pin: Option<&Pin>) -> Result<(), TokenError> {
            guarded(|| {
                let result = match pin {
                    // `RawAuthPin` zeroes its copy when dropped.
                    Some(p) => self.session.login_with_raw(UserType::User, &RawAuthPin::new(Box::new(p.expose().to_vec()))),
                    None => self.session.login(UserType::User, None),
                };
                match result {
                    Ok(()) | Err(Error::Pkcs11(RvError::UserAlreadyLoggedIn, _)) => Ok(()),
                    Err(e) => Err(map_err(e)),
                }
            })
        }

        fn certificates(&mut self) -> Result<Vec<TokenObject>, TokenError> {
            guarded(|| {
                let handles = self.find(&[Attribute::Class(ObjectClass::CERTIFICATE), Attribute::CertificateType(CertificateType::X_509)])?;
                let mut out = Vec::new();
                for h in handles {
                    let mut obj = TokenObject::default();
                    let Ok(attrs) = self.attributes(h, &[AttributeType::Id, AttributeType::Label], MAX_ATTR_LEN) else { continue };
                    for a in attrs {
                        match a {
                            Attribute::Id(id) => obj.id = id,
                            Attribute::Label(l) => obj.label = String::from_utf8_lossy(&l).into_owned(),
                            _ => {}
                        }
                    }
                    let Ok(value) = self.attributes(h, &[AttributeType::Value], MAX_CERT_LEN) else { continue };
                    for a in value {
                        if let Attribute::Value(v) = a {
                            obj.value = v;
                        }
                    }
                    out.push(obj);
                }
                Ok(out)
            })
        }

        fn private_key_ids(&mut self) -> Result<Vec<Vec<u8>>, TokenError> {
            guarded(|| {
                let handles = self.find(&[Attribute::Class(ObjectClass::PRIVATE_KEY)])?;
                let mut out = Vec::new();
                for h in handles {
                    let Ok(attrs) = self.attributes(h, &[AttributeType::Id], MAX_ATTR_LEN) else { continue };
                    for a in attrs {
                        if let Attribute::Id(id) = a {
                            out.push(id);
                        }
                    }
                }
                Ok(out)
            })
        }

        fn mechanisms(&mut self) -> Result<Vec<Mechanism>, TokenError> {
            if let Some(m) = &self.mechanisms {
                return Ok(m.clone());
            }
            let list = guarded(|| self.ctx.get_mechanism_list(self.slot).map_err(map_err))?;
            let table = [
                (MechanismType::RSA_PKCS, Mechanism::RsaPkcs),
                (MechanismType::SHA256_RSA_PKCS, Mechanism::RsaPkcsSha(DigestAlg::Sha256)),
                (MechanismType::SHA384_RSA_PKCS, Mechanism::RsaPkcsSha(DigestAlg::Sha384)),
                (MechanismType::SHA512_RSA_PKCS, Mechanism::RsaPkcsSha(DigestAlg::Sha512)),
                (MechanismType::ECDSA, Mechanism::Ecdsa),
                (MechanismType::ECDSA_SHA256, Mechanism::EcdsaSha(DigestAlg::Sha256)),
                (MechanismType::ECDSA_SHA384, Mechanism::EcdsaSha(DigestAlg::Sha384)),
                (MechanismType::ECDSA_SHA512, Mechanism::EcdsaSha(DigestAlg::Sha512)),
            ];
            let out: Vec<Mechanism> = table.iter().filter(|(t, _)| list.iter().take(4096).any(|m| m == t)).map(|(_, m)| *m).collect();
            self.mechanisms = Some(out.clone());
            Ok(out)
        }

        fn sign(&mut self, key_id: &[u8], mechanism: Mechanism, data: &[u8]) -> Result<Vec<u8>, TokenError> {
            let m = match mechanism {
                Mechanism::RsaPkcs => CkMechanism::RsaPkcs,
                Mechanism::RsaPkcsSha(DigestAlg::Sha384) => CkMechanism::Sha384RsaPkcs,
                Mechanism::RsaPkcsSha(DigestAlg::Sha512) => CkMechanism::Sha512RsaPkcs,
                Mechanism::RsaPkcsSha(_) => CkMechanism::Sha256RsaPkcs,
                Mechanism::Ecdsa => CkMechanism::Ecdsa,
                Mechanism::EcdsaSha(DigestAlg::Sha384) => CkMechanism::EcdsaSha384,
                Mechanism::EcdsaSha(DigestAlg::Sha512) => CkMechanism::EcdsaSha512,
                Mechanism::EcdsaSha(_) => CkMechanism::EcdsaSha256,
            };
            guarded(|| {
                let key = self.key(key_id)?;
                self.session.sign(&m, key, data).map_err(map_err)
            })
        }

        fn logout(&mut self) {
            let _ = guarded(|| self.session.logout().map_err(map_err));
        }

        fn token_info(&mut self) -> Option<TokenInfo> {
            guarded(|| self.ctx.get_token_info(self.slot).map(|t| token_info(&t)).map_err(map_err)).ok()
        }
    }
}

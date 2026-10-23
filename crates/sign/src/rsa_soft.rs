//! RSA signing in pure Rust (RSASSA-PKCS1-v1_5, RFC 8017 §8.2.1) for targets without
//! aws-lc-rs: the web app. The private-key operation is the `rsa` crate's, blinded with
//! randomness from the platform (Web Crypto in the browser) and checked against the public key
//! afterwards, so a faulty computation never leaks the key. Native targets sign with aws-lc-rs
//! (ADR-0009); this module is compiled there too so tests can hold both to the same bytes.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use rsa::traits::PublicKeyParts;
use rsa::{BoxedUint, RsaPrivateKey};

use crate::SignError;
use crate::der::{self, Tlv, tag};
use crate::keys::DigestAlg;

/// The largest modulus read (8192 bits, the `rsa` crate's limit). Components beyond it are
/// refused before any arithmetic, which bounds the work a hostile key file can ask for.
const MAX_MODULUS_BYTES: usize = 1024;

/// An RSA private key that signs with the `rsa` crate.
pub(crate) struct SoftRsa {
    key: RsaPrivateKey,
    /// The modulus length in bytes: every signature has exactly this length.
    k: usize,
}

fn bad(what: &str) -> SignError {
    SignError::Malformed(format!("RSA private key: {what}"))
}

impl SoftRsa {
    /// From an `RSAPrivateKey` (RFC 8017 A.1.2): version, n, e, d, p, q, dp, dq, qinv. The key is
    /// validated (the primes multiply to the modulus and the exponents match).
    pub(crate) fn from_der(rsa_private_key: &[u8]) -> Result<Self, SignError> {
        let fields = Tlv::parse_all(rsa_private_key)?.children()?;
        let [version, n, e, d, p, q, ..] = fields.as_slice() else { return Err(bad("missing components")) };
        if version.tag != tag::INTEGER || version.value != [0] {
            return Err(SignError::Unsupported("multi-prime RSA keys".into()));
        }
        let mut parts = Vec::with_capacity(5);
        for (t, what) in [(n, "modulus"), (e, "public exponent"), (d, "private exponent"), (p, "first prime"), (q, "second prime")] {
            if t.tag != tag::INTEGER || t.value.first().is_some_and(|b| b & 0x80 != 0) {
                return Err(bad(&format!("the {what} is not a positive INTEGER")));
            }
            let bytes = t.uint_bytes();
            if bytes.is_empty() || bytes.iter().all(|b| *b == 0) {
                return Err(bad(&format!("the {what} is zero")));
            }
            if bytes.len() > MAX_MODULUS_BYTES {
                return Err(SignError::Unsupported("RSA keys larger than 8192 bits".into()));
            }
            parts.push(BoxedUint::from_be_slice_vartime(bytes));
        }
        let [n, e, d, p, q]: [BoxedUint; 5] = parts.try_into().map_err(|_| bad("missing components"))?;
        let key = RsaPrivateKey::from_components(n, e, d, vec![p, q]).map_err(|err| bad(&err.to_string()))?;
        let k = key.size();
        Ok(SoftRsa { key, k })
    }

    /// RSASSA-PKCS1-v1_5 over `msg` hashed with `alg`: the same bytes aws-lc-rs writes (the
    /// scheme is deterministic).
    pub(crate) fn sign(&self, alg: DigestAlg, msg: &[u8]) -> Result<Vec<u8>, SignError> {
        let digest = alg.digest(&[msg]);
        let info = der::seq(&[&der::algorithm(alg.oid(), Some(&der::null())), &der::octets(&digest)]);
        let k = self.k;
        // EM = 0x00 || 0x01 || PS (0xFF, at least 8) || 0x00 || DigestInfo (RFC 8017 §9.2).
        let ps = k
            .checked_sub(info.len().saturating_add(3))
            .filter(|ps| *ps >= 8)
            .ok_or_else(|| SignError::Crypto("the RSA key is too short for this digest".into()))?;
        let mut em = Vec::with_capacity(k);
        em.extend_from_slice(&[0x00, 0x01]);
        em.resize(2 + ps, 0xFF);
        em.push(0x00);
        em.extend_from_slice(&info);
        let m = BoxedUint::from_be_slice(&em, self.key.n_bits_precision()).map_err(|_| SignError::Crypto("RSA signing failed".into()))?;
        let mut rng = getrandom::SysRng;
        let s = rsa::hazmat::rsa_decrypt_and_check(&self.key, Some(&mut rng), &m).map_err(|_| SignError::Crypto("RSA signing failed".into()))?;
        let bytes = s.to_be_bytes();
        let mut out = vec![0u8; k.saturating_sub(bytes.len())];
        out.extend_from_slice(bytes.get(bytes.len().saturating_sub(k)..).unwrap_or_default());
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{PrivateKey, PublicKey};

    /// The `RSAPrivateKey` inside a PKCS #8 `PrivateKeyInfo`.
    fn rsa_private_key(pkcs8: &[u8]) -> Vec<u8> {
        let t = Tlv::parse_all(pkcs8).unwrap().children().unwrap();
        t[2].value.to_vec()
    }

    #[test]
    fn signs_the_same_bytes_as_aws_lc_and_verifies() {
        let key = PrivateKey::generate_rsa(2048).unwrap();
        let soft = SoftRsa::from_der(&rsa_private_key(key.pkcs8())).unwrap();
        for alg in [DigestAlg::Sha256, DigestAlg::Sha384, DigestAlg::Sha512] {
            let msg = b"the signed attributes";
            let ours = soft.sign(alg, msg).unwrap();
            assert_eq!(ours.len(), 256);
            assert_eq!(ours, key.sign(alg, msg).unwrap(), "{}", alg.name());
            let PublicKey::Rsa { n, e } = key.public_key() else { panic!("an RSA key") };
            let mut notes = Vec::new();
            assert!(crate::rsa_pad::pkcs1_verify(n, e, alg, &alg.digest(&[msg]), &ours, &mut notes).unwrap());
        }
    }

    #[test]
    fn hostile_keys_are_errors() {
        let key = PrivateKey::generate_rsa(2048).unwrap();
        let good = rsa_private_key(key.pkcs8());
        let fields = Tlv::parse_all(&good).unwrap().children().unwrap();
        let raw: Vec<&[u8]> = fields.iter().map(|f| f.raw).collect();
        let rebuild = |replace: usize, with: &[u8]| {
            let parts: Vec<&[u8]> = raw.iter().enumerate().map(|(i, r)| if i == replace { with } else { *r }).collect();
            der::seq(&parts)
        };
        let big = der::uint(&vec![0x7F; 2000]);
        for bytes in [
            Vec::new(),
            vec![0x30, 0x00],
            der::seq(&[&der::int(0), &der::int(5)]),
            rebuild(0, &der::int(1)),
            rebuild(1, &der::int(0)),
            rebuild(1, &big),
            rebuild(3, &der::int(3)),
            rebuild(4, &der::int(7)),
            rebuild(2, &[0x04, 0x01, 0x03]),
            good[..good.len() - 1].to_vec(),
        ] {
            assert!(SoftRsa::from_der(&bytes).is_err(), "{bytes:02x?}");
        }
        // A modulus too short for the digest: refused, not padded wrongly.
        let tiny = SoftRsa { key: soft_key_512(), k: 64 };
        assert!(tiny.sign(DigestAlg::Sha512, b"x").is_err());
        assert!(tiny.sign(DigestAlg::Sha256, b"x").is_ok());
    }

    /// A 512-bit test key: too short for a SHA-512 DigestInfo.
    fn soft_key_512() -> RsaPrivateKey {
        // Generated deterministically by the crate's own key generator with a seeded RNG.
        let mut rng = Seeded(0x5eed);
        RsaPrivateKey::new_unchecked(&mut rng, 512).unwrap()
    }

    struct Seeded(u64);
    impl rsa::rand_core::TryRng for Seeded {
        type Error = core::convert::Infallible;
        fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
            Ok(self.try_next_u64()? as u32)
        }
        fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            Ok(self.0)
        }
        fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
            for b in dst {
                *b = self.try_next_u64()? as u8;
            }
            Ok(())
        }
    }
    impl rsa::rand_core::TryCryptoRng for Seeded {}
}

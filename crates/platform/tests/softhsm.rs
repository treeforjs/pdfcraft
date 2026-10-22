//! Signing on a real PKCS #11 module (sign.pkcs11): SoftHSM, with a token in a throwaway
//! directory. Skips when SoftHSM (or OpenSC's pkcs11-tool, used to put a certificate on the
//! token) isn't installed. The work runs in a child copy of this test binary, so the token
//! directory can be passed through SOFTHSM2_CONF without changing this process's environment.

#![cfg(all(feature = "pkcs11", not(target_arch = "wasm32")))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use base64::Engine;
use pdfcraft_cos::Document;
use pdfcraft_sign::token::{self, Pin, TokenError};
use pdfcraft_sign::{SignOptions, Status, TrustStore, pkcs12, signatures};

const CHILD: &str = "PDFCRAFT_SOFTHSM_CHILD";
const LABEL: &str = "PdfCraft Test";

fn softhsm() -> Option<String> {
    pdfcraft_platform::pkcs11::common_module_paths().iter().find(|p| p.contains("softhsm") && Path::new(p).is_file()).map(|p| p.to_string())
}

fn tool(name: &str) -> bool {
    Command::new(name).arg("--help").output().is_ok()
}

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../sign/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{cmd:?}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// One empty page.
fn fixture() -> Vec<u8> {
    let objs: Vec<&[u8]> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] >>",
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn signing_on_softhsm() {
    let (Some(lib), true) = (softhsm(), tool("softhsm2-util") && tool("pkcs11-tool")) else {
        eprintln!("SoftHSM or pkcs11-tool not installed; skipped");
        return;
    };
    let dir = TempDir(std::env::temp_dir().join(format!("pdfcraft-softhsm-{}", std::process::id())));
    let tokens = dir.0.join("tokens");
    std::fs::create_dir_all(&tokens).unwrap();
    let conf = dir.0.join("softhsm2.conf");
    std::fs::write(&conf, format!("directories.tokendir = {}\nobjectstore.backend = file\nlog.level = ERROR\n", tokens.display())).unwrap();
    let env = |c: &mut Command| {
        c.env("SOFTHSM2_CONF", &conf);
    };
    let mut init = Command::new("softhsm2-util");
    init.args(["--init-token", "--free", "--label", LABEL, "--pin", "1234", "--so-pin", "12345678"]);
    env(&mut init);
    run(&mut init);
    for (file, hex) in [("rsa-aes.p12", "01"), ("ec-p256.p12", "02")] {
        let id = pkcs12::open(&data(file), "test").unwrap();
        let pem =
            format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n", base64::engine::general_purpose::STANDARD.encode(id.key.pkcs8()));
        let key = dir.0.join(format!("{hex}.pem"));
        std::fs::write(&key, pem).unwrap();
        let mut import = Command::new("softhsm2-util");
        import.args(["--import"]).arg(&key).args(["--token", LABEL, "--label", file, "--id", hex, "--pin", "1234"]);
        env(&mut import);
        run(&mut import);
        let cert = dir.0.join(format!("{hex}.der"));
        std::fs::write(&cert, &id.certificate.raw).unwrap();
        let mut write = Command::new("pkcs11-tool");
        write
            .args(["--module", &lib, "--token-label", LABEL, "--login", "--pin", "1234", "--write-object"])
            .arg(&cert)
            .args(["--type", "cert", "--id", hex, "--label", file]);
        env(&mut write);
        run(&mut write);
    }
    // The signing runs in a child with SOFTHSM2_CONF pointing at the throwaway tokens.
    let mut child = Command::new(std::env::current_exe().unwrap());
    child.args(["--ignored", "--exact", "softhsm_child", "--nocapture"]).env(CHILD, &lib);
    env(&mut child);
    let out = run(&mut child);
    assert!(out.contains("test softhsm_child ... ok"), "the child ran: {out}");
}

#[test]
#[ignore = "run by signing_on_softhsm with a throwaway SoftHSM token"]
fn softhsm_child() {
    let Ok(lib) = std::env::var(CHILD) else { return };
    let provider = pdfcraft_platform::pkcs11::provider();
    assert!(provider.discover().contains(&lib));
    assert!(matches!(provider.load("/nonexistent/module.so").err(), Some(TokenError::ModuleNotFound(_))));
    let (_, module) = token::load_module(provider.as_ref(), Some(&lib)).unwrap();
    // Loading again reuses the module (C_Initialize runs once).
    provider.load(&lib).unwrap();
    assert!(!module.info().unwrap().manufacturer.is_empty());
    let slot = token::slots(module.as_ref()).unwrap().into_iter().find(|s| s.token.as_ref().is_some_and(|t| t.label == LABEL)).unwrap();
    // A wrong PIN is refused (and SoftHSM counts it).
    assert!(matches!(token::login(module.as_ref(), slot.id, Some(Pin::from_bytes(b"9999".to_vec()))), Err(TokenError::WrongPin { .. })));
    let login = token::login(module.as_ref(), slot.id, Some(Pin::from_bytes(b"1234".to_vec()))).unwrap();
    let found = login.identities().unwrap();
    assert_eq!(found.ids.len(), 2, "{:?}", found.skipped);
    for ident in &found.ids {
        let id = login.digital_id(ident);
        let opts = SignOptions { page: 0, rect: None, date: "D:20261002120000Z".into(), ..SignOptions::default() };
        let signed = pdfcraft_sign::sign(&Document::open(Arc::new(fixture())).unwrap(), &id, &opts).unwrap();
        let doc = Document::open(Arc::new(signed.clone())).unwrap();
        let s = signatures(&doc, &signed, &TrustStore::new(vec![id.certificate.clone()])).into_iter().find(|s| s.signed).unwrap();
        assert_eq!(s.status, Status::Valid, "{}: {:?}", ident.label, s.details);
    }
}

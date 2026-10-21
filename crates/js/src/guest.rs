//! Trusted guest entry point. The host meters this code, Boa and every allocation together.
use std::io::{Read, Write};

use serde::Deserialize;

use crate::wire::{Envelope, Job, MAX_INPUT, MAX_OUTPUT, Reply, VERSION, encode};

pub fn main() -> Result<(), String> {
    let mut input = Vec::new();
    std::io::stdin().take(MAX_INPUT as u64 + 1).read_to_end(&mut input).map_err(|e| e.to_string())?;
    if input.len() > MAX_INPUT {
        return Err("script input exceeds its byte budget".into());
    }
    // XFA trees are checked to depth 64 by the host. Deserializing them occurs only inside
    // the capped guest stack and fuel budget, including when this executable is run directly.
    let mut decoder = serde_json::Deserializer::from_slice(&input);
    decoder.disable_recursion_limit();
    let envelope = Envelope::deserialize(&mut decoder).map_err(|e| e.to_string())?;
    decoder.end().map_err(|e| e.to_string())?;
    if envelope.version != VERSION {
        return Err("script kernel protocol version differs; rebuild the sandbox".into());
    }
    let platform = match envelope.platform.as_str() {
        "WIN" => "WIN",
        "MAC" => "MAC",
        "UNIX" => "UNIX",
        _ => return Err("unknown script platform".into()),
    };
    crate::PLATFORM.with(|p| p.set(platform));
    let reply = match envelope.job {
        Job::Field { script, event, doc, fields, doc_scripts, limits, dialogs } => {
            Reply::Field(crate::run_with(&script, &event, &doc, &fields, &doc_scripts, limits, &dialogs))
        }
        Job::Xfa { script, event, doc, root, limits, formcalc } => {
            let root = root.into_owned();
            let out = if formcalc {
                crate::formcalc::run_formcalc_at(&script, &event, &doc, root, limits, None)
            } else {
                crate::xfa::run_xfa(&script, &event, &doc, &root, limits)
            };
            Reply::Xfa(out)
        }
    };
    let output = encode(&reply, MAX_OUTPUT)?;
    std::io::stdout().write_all(&output).map_err(|e| e.to_string())
}

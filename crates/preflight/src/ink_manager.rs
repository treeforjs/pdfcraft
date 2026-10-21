//! Ink Manager (PDF: Output Preview / Print ▸ Output ▸ Ink Manager): session settings that
//! decide which plate each ink prints on: alias a spot ink to another ink (spot or process),
//! convert spot inks to process (CMYK), and preview spot colours from their Lab values.
//!
//! The settings don't change the document: the separations preview, ink listing and total area
//! coverage honour them when the [`crate::coverage::Conversion`] they run with returns them
//! from `ink_manager` (wrap one in [`Managed`]). Making them permanent is a separate, explicit
//! edit (the engine's `apply_ink_manager`).
//!
//! Aliases chain (A → B, B → C prints A on C's plate, at most [`MAX_CHAIN`] steps; cycles are
//! refused). An ink is converted to process when the ink it finally prints on is set to process
//! (or every spot ink is): its tint transform gives its alternate colour, and gray, RGB, CMYK and
//! Lab alternates become process inks (RGB and Lab through the document's press conversion).

use std::collections::{BTreeMap, BTreeSet};

use pdfcraft_cos::{Document, Object};

use crate::coverage::{Conversion, PROCESS_INKS, TintFunction};

/// Aliases and spot inks set to process, at most.
pub const MAX_ALIASES: usize = 256;
pub const MAX_TO_PROCESS: usize = 256;
/// The longest ink name accepted (PDF names are limited to 127 bytes).
pub const MAX_NAME: usize = 127;
/// Alias chains are followed at most this far.
pub const MAX_CHAIN: usize = 16;

/// The Ink Manager's settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InkManager {
    /// Spot ink → the ink (spot or process) it prints on instead.
    pub aliases: BTreeMap<String, String>,
    /// Spot inks converted to process.
    pub to_process: BTreeSet<String>,
    /// Every spot ink is converted to process (the viewer's "All Spots to Process").
    pub all_to_process: bool,
    /// Preview spot colours whose alternate space is Lab from their Lab values (the viewer's
    /// "Use Lab values for spot colors"); otherwise they preview mid gray.
    pub use_lab: bool,
}

/// Where an ink goes under the Ink Manager.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// Its own plate.
    Own,
    /// Another ink's plate (a process ink or a spot ink), through aliases.
    Ink(String),
    /// Converted to process (CMYK).
    Process,
}

fn is_process(name: &str) -> bool {
    PROCESS_INKS.contains(&name)
}

impl InkManager {
    /// Nothing set: every ink prints on its own plate.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether any ink leaves its own plate.
    pub fn moves_inks(&self) -> bool {
        !self.aliases.is_empty() || !self.to_process.is_empty() || self.all_to_process
    }

    /// Check the settings: names 1–127 bytes, not `All`/`None`, process inks neither aliased nor
    /// set to process, no alias to itself, no cycles, nothing both aliased and set to process.
    pub fn validate(&self) -> Result<(), String> {
        if self.aliases.len() > MAX_ALIASES {
            return Err(format!("at most {MAX_ALIASES} ink aliases"));
        }
        if self.to_process.len() > MAX_TO_PROCESS {
            return Err(format!("at most {MAX_TO_PROCESS} inks can be set to process"));
        }
        let name_ok = |n: &str| -> Result<(), String> {
            if n.is_empty() || n.len() > MAX_NAME {
                return Err(format!("ink names are 1 to {MAX_NAME} bytes ({n:?})"));
            }
            if n == "All" || n == "None" {
                return Err("All (registration) and None are not inks".into());
            }
            Ok(())
        };
        for (from, to) in &self.aliases {
            name_ok(from)?;
            name_ok(to)?;
            if is_process(from) {
                return Err(format!("{from} is a process ink; only spot inks can be aliased"));
            }
            if from == to {
                return Err(format!("{from} can't be aliased to itself"));
            }
        }
        for name in &self.to_process {
            name_ok(name)?;
            if is_process(name) {
                return Err(format!("{name} is already a process ink"));
            }
            if let Some(to) = self.aliases.get(name) {
                return Err(format!("{name} is aliased to {to}, so it prints as {to} does: set {to} to process instead"));
            }
        }
        for from in self.aliases.keys() {
            let mut at = from.as_str();
            let mut steps = 0;
            while let Some(next) = self.aliases.get(at) {
                steps += 1;
                if next == from || steps > MAX_CHAIN {
                    return Err(format!("the aliases of {from} form a cycle (or a chain longer than {MAX_CHAIN})"));
                }
                at = next;
            }
        }
        Ok(())
    }

    /// The ink `name` finally prints as, following aliases.
    pub fn resolve<'a>(&'a self, name: &'a str) -> &'a str {
        let mut at = name;
        for _ in 0..MAX_CHAIN {
            match self.aliases.get(at) {
                Some(next) if next != name => at = next,
                _ => break,
            }
        }
        at
    }

    /// Where ink `name` goes.
    pub fn route(&self, name: &str) -> Route {
        if is_process(name) || name == "All" || name == "None" {
            return Route::Own;
        }
        let last = self.resolve(name);
        if !is_process(last) && (self.all_to_process || self.to_process.contains(last)) {
            return Route::Process;
        }
        if last == name { Route::Own } else { Route::Ink(last.to_string()) }
    }

    /// Every alias flattened to the ink it finally prints on.
    pub fn final_aliases(&self) -> BTreeMap<String, String> {
        self.aliases.keys().map(|k| (k.clone(), self.resolve(k).to_string())).filter(|(k, v)| k != v).collect()
    }
}

/// A [`Conversion`] that also applies Ink Manager settings.
pub struct Managed<'a> {
    pub conversion: &'a dyn Conversion,
    pub inks: &'a InkManager,
}

impl Conversion for Managed<'_> {
    fn describe(&self) -> String {
        self.conversion.describe()
    }
    fn uses_profile(&self) -> bool {
        self.conversion.uses_profile()
    }
    fn rgb(&self, rgb: [f64; 3]) -> [f64; 4] {
        self.conversion.rgb(rgb)
    }
    fn lab(&self, lab: [f64; 3]) -> [f64; 4] {
        self.conversion.lab(lab)
    }
    fn tint_function(&self, doc: &Document, function: &Object, inputs: usize, outputs: usize) -> Option<Box<dyn TintFunction>> {
        self.conversion.tint_function(doc, function, inputs, outputs)
    }
    fn ink_manager(&self) -> Option<&InkManager> {
        (!self.inks.is_default()).then_some(self.inks)
    }
    fn display_cmyk(&self, quads: &[u8]) -> Option<Vec<u8>> {
        self.conversion.display_cmyk(quads)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(aliases: &[(&str, &str)], to_process: &[&str]) -> InkManager {
        InkManager {
            aliases: aliases.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
            to_process: to_process.iter().map(|s| s.to_string()).collect(),
            ..InkManager::default()
        }
    }

    #[test]
    fn routes_follow_alias_chains_to_process_or_another_plate() {
        let m = manager(&[("A", "B"), ("B", "C"), ("D", "Magenta")], &["C"]);
        assert!(m.validate().is_ok());
        assert_eq!(m.route("A"), Route::Process);
        assert_eq!(m.route("C"), Route::Process);
        assert_eq!(m.route("D"), Route::Ink("Magenta".into()));
        assert_eq!(m.route("E"), Route::Own);
        assert_eq!(m.route("Cyan"), Route::Own);
        let m = manager(&[("A", "B")], &[]);
        assert_eq!(m.route("A"), Route::Ink("B".into()));
        assert_eq!(m.final_aliases().get("A").map(String::as_str), Some("B"));
        let all = InkManager { all_to_process: true, ..manager(&[("D", "Magenta")], &[]) };
        assert_eq!(all.route("E"), Route::Process);
        assert_eq!(all.route("D"), Route::Ink("Magenta".into()), "aliased to a process ink");
    }

    #[test]
    fn bad_settings_are_refused_and_cycles_never_loop() {
        for bad in [
            manager(&[("A", "B"), ("B", "A")], &[]),
            manager(&[("A", "A")], &[]),
            manager(&[("Cyan", "A")], &[]),
            manager(&[("A", "All")], &[]),
            manager(&[], &["Black"]),
            manager(&[("A", "B")], &["A"]),
            manager(&[("", "B")], &[]),
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
            // Routing never loops, even on settings that were not validated.
            let _ = bad.route("A");
        }
    }
}

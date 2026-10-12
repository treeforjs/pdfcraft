//! Page transitions (ISO 32000-2 §12.4.4.1, Table 164) and automatic advance (`/Dur`, Table 31):
//! Acrobat's Organize Pages ▸ More ▸ Page Transitions, execution plan M4.
//!
//! Reading is lenient: an unknown style reads as `R` (the spec's default), out-of-range numbers
//! fall back to their defaults, and entries that do not apply to the style are ignored. Writing is
//! strict: only the entries that apply to the style are written, with values the spec allows, so
//! a viewer never has to guess. Every page is validated before any page changes.

use pdfcraft_cos::{Dict, Document, Object};

use crate::{OrganizeError, check, walk};

/// Longest transition and auto-advance accepted when writing, in seconds (the spec sets no limit;
/// this keeps values meaningful and bounded).
pub const MAX_TRANSITION_SECONDS: f64 = 3600.0;

/// The transition style (`/S`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionStyle {
    Split,
    Blinds,
    Box,
    Wipe,
    Dissolve,
    Glitter,
    /// `R`: the new page simply replaces the old one (the default).
    Replace,
    Fly,
    Push,
    Cover,
    Uncover,
    Fade,
}

impl TransitionStyle {
    pub const ALL: [TransitionStyle; 12] = [
        Self::Split,
        Self::Blinds,
        Self::Box,
        Self::Wipe,
        Self::Dissolve,
        Self::Glitter,
        Self::Replace,
        Self::Fly,
        Self::Push,
        Self::Cover,
        Self::Uncover,
        Self::Fade,
    ];

    /// The PDF name (`/S` value).
    pub fn pdf_name(self) -> &'static str {
        match self {
            Self::Split => "Split",
            Self::Blinds => "Blinds",
            Self::Box => "Box",
            Self::Wipe => "Wipe",
            Self::Dissolve => "Dissolve",
            Self::Glitter => "Glitter",
            Self::Replace => "R",
            Self::Fly => "Fly",
            Self::Push => "Push",
            Self::Cover => "Cover",
            Self::Uncover => "Uncover",
            Self::Fade => "Fade",
        }
    }

    /// Parse a PDF name or a lower-case word ("replace" for `R`).
    pub fn from_name(s: &str) -> Option<Self> {
        if s == "R" || s.eq_ignore_ascii_case("replace") {
            return Some(Self::Replace);
        }
        Self::ALL.into_iter().find(|t| t.pdf_name().eq_ignore_ascii_case(s))
    }

    fn uses_dimension(self) -> bool {
        matches!(self, Self::Split | Self::Blinds)
    }
    fn uses_motion(self) -> bool {
        matches!(self, Self::Split | Self::Box | Self::Fly)
    }
    fn uses_direction(self) -> bool {
        matches!(self, Self::Wipe | Self::Glitter | Self::Fly | Self::Cover | Self::Uncover | Self::Push)
    }
}

/// `/Dm`: the dimension of a Split or Blinds effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Dimension {
    #[default]
    Horizontal,
    Vertical,
}

/// `/M`: the motion of a Split, Box or Fly effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Motion {
    #[default]
    Inward,
    Outward,
}

/// `/Di`: the direction of a Wipe, Glitter, Fly, Cover, Uncover or Push effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Degrees counterclockwise from left-to-right: 0, 90 (Wipe only), 180 (Wipe only),
    /// 270, or 315 (Glitter only).
    Angle(u16),
    /// `/None`: Fly only, when the scale is not 1.
    None,
}

impl Default for Direction {
    fn default() -> Self {
        Direction::Angle(0)
    }
}

/// A transition dictionary (`/Trans`), with every optional entry made explicit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transition {
    pub style: TransitionStyle,
    /// `/D`: seconds the effect takes (default 1).
    pub duration: f64,
    pub dimension: Dimension,
    pub motion: Motion,
    pub direction: Direction,
    /// `/SS`: Fly only — starting or ending scale (default 1).
    pub scale: f64,
    /// `/B`: Fly only — the flying area is opaque (default false).
    pub opaque: bool,
}

impl Transition {
    pub fn new(style: TransitionStyle) -> Self {
        Transition {
            style,
            duration: 1.0,
            dimension: Dimension::Horizontal,
            motion: Motion::Inward,
            direction: Direction::Angle(0),
            scale: 1.0,
            opaque: false,
        }
    }

    /// Check the values against Table 164; `Err` says what is wrong.
    pub fn validate(&self) -> Result<(), OrganizeError> {
        let bad = |m: String| Err(OrganizeError::Invalid(m));
        if !(self.duration.is_finite() && self.duration > 0.0 && self.duration <= MAX_TRANSITION_SECONDS) {
            return bad(format!("the transition duration must be more than 0 and at most {MAX_TRANSITION_SECONDS} seconds"));
        }
        if self.style.uses_direction() {
            let ok = match (self.direction, self.style) {
                (Direction::Angle(0 | 270), _) => true,
                (Direction::Angle(90 | 180), TransitionStyle::Wipe) => true,
                (Direction::Angle(315), TransitionStyle::Glitter) => true,
                (Direction::None, TransitionStyle::Fly) => self.scale != 1.0,
                _ => false,
            };
            if !ok {
                let allowed = match self.style {
                    TransitionStyle::Wipe => "0, 90, 180 or 270",
                    TransitionStyle::Glitter => "0, 270 or 315",
                    TransitionStyle::Fly => "0, 270, or none when the scale is not 1",
                    _ => "0 or 270",
                };
                return bad(format!("a {} transition's direction must be {allowed}", self.style.pdf_name()));
            }
        }
        if self.style == TransitionStyle::Fly && !(self.scale.is_finite() && self.scale > 0.0 && self.scale <= 100.0) {
            return bad("the fly scale must be more than 0 and at most 100".into());
        }
        Ok(())
    }

    /// The `/Trans` dictionary: only the entries that apply to the style.
    pub fn to_dict(&self) -> Dict {
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name("Trans"));
        d.set(b"S".to_vec(), Object::name(self.style.pdf_name()));
        d.set(b"D".to_vec(), Object::Real(self.duration));
        if self.style.uses_dimension() {
            d.set(b"Dm".to_vec(), Object::name(if self.dimension == Dimension::Vertical { "V" } else { "H" }));
        }
        if self.style.uses_motion() {
            d.set(b"M".to_vec(), Object::name(if self.motion == Motion::Outward { "O" } else { "I" }));
        }
        if self.style.uses_direction() {
            d.set(
                b"Di".to_vec(),
                match self.direction {
                    Direction::Angle(a) => Object::Int(i64::from(a)),
                    Direction::None => Object::name("None"),
                },
            );
        }
        if self.style == TransitionStyle::Fly {
            d.set(b"SS".to_vec(), Object::Real(self.scale));
            d.set(b"B".to_vec(), Object::Bool(self.opaque));
        }
        d
    }

    /// Read a `/Trans` dictionary leniently (see the module comment).
    pub fn from_dict(d: &Dict, doc: &Document) -> Self {
        let get = |k: &[u8]| d.get(k).map(|o| doc.resolve(o));
        let name = |k: &[u8]| get(k).and_then(|o| o.as_name().map(<[u8]>::to_vec));
        let num = |k: &[u8]| get(k).and_then(|o| o.as_f64()).filter(|v| v.is_finite());
        let style = name(b"S").and_then(|n| std::str::from_utf8(&n).ok().and_then(TransitionStyle::from_name)).unwrap_or(TransitionStyle::Replace);
        let mut t = Transition::new(style);
        if let Some(v) = num(b"D").filter(|v| *v > 0.0) {
            t.duration = v;
        }
        if name(b"Dm").as_deref() == Some(b"V") {
            t.dimension = Dimension::Vertical;
        }
        if name(b"M").as_deref() == Some(b"O") {
            t.motion = Motion::Outward;
        }
        if name(b"Di").as_deref() == Some(b"None") {
            t.direction = Direction::None;
        } else if let Some(v) = num(b"Di") {
            let a = v.rem_euclid(360.0).round();
            if [0.0, 90.0, 180.0, 270.0, 315.0].contains(&a) {
                t.direction = Direction::Angle(a as u16);
            }
        }
        if let Some(v) = num(b"SS").filter(|v| *v > 0.0) {
            t.scale = v;
        }
        if let Some(Object::Bool(b)) = get(b"B").as_deref() {
            t.opaque = *b;
        }
        t
    }
}

/// A page's presentation settings: its transition (`/Trans`) and how many seconds it is shown
/// before advancing automatically (`/Dur`; `None` = advance manually).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct PageTransition {
    pub transition: Option<Transition>,
    pub advance: Option<f64>,
}

/// Every page's transition settings, in page order.
pub fn page_transitions(doc: &Document) -> Result<Vec<PageTransition>, OrganizeError> {
    Ok(walk(doc)?
        .iter()
        .map(|(page, _)| {
            let obj = doc.get(*page);
            let Some(d) = obj.as_dict() else { return PageTransition::default() };
            let transition = d.get(b"Trans").map(|t| doc.resolve(t)).and_then(|t| t.as_dict().map(|t| Transition::from_dict(t, doc)));
            let advance = d.get(b"Dur").and_then(|o| doc.resolve(o).as_f64()).filter(|v| v.is_finite() && *v >= 0.0);
            PageTransition { transition, advance }
        })
        .collect())
}

/// Set (or with `None`, remove) the transition and the auto-advance time of `pages` (0-based).
/// The two are set together, as Acrobat's Page Transitions dialog does. Atomic: everything is
/// validated before any page changes.
pub fn set_page_transition(doc: &mut Document, pages: &[usize], settings: &PageTransition) -> Result<(), OrganizeError> {
    let all = walk(doc)?;
    check(pages, all.len())?;
    if pages.is_empty() {
        return Err(OrganizeError::Invalid("no pages to change".into()));
    }
    if let Some(t) = &settings.transition {
        t.validate()?;
    }
    if let Some(a) = settings.advance
        && !(a.is_finite() && (0.0..=MAX_TRANSITION_SECONDS).contains(&a))
    {
        return Err(OrganizeError::Invalid(format!("the page display time must be 0 to {MAX_TRANSITION_SECONDS} seconds")));
    }
    let trans = settings.transition.map(|t| t.to_dict());
    for &i in pages {
        doc.update_dict(all[i].0, |d| {
            match &trans {
                Some(t) => d.set(b"Trans".to_vec(), Object::Dict(t.clone())),
                None => {
                    d.remove(b"Trans");
                }
            }
            match settings.advance {
                Some(a) => d.set(b"Dur".to_vec(), Object::Real(a)),
                None => {
                    d.remove(b"Dur");
                }
            }
        })?;
    }
    Ok(())
}

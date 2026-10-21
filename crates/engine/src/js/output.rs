//! Admission before cloning script messages into retained document output.
use super::{Outcome, Request};

/// Retained messages share one byte/item budget until `Session::take_js_output`.
pub const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_OUTPUT_ITEMS: usize = 10_000;
const ITEM_BYTES: usize = 32;
const NOTICE: &str = "Script output exceeded its retained byte or message budget; further output was omitted";

#[derive(Clone, Copy, Default)]
struct Size {
    bytes: usize,
    items: usize,
}

impl Size {
    fn text(text: &str) -> Self {
        Self { bytes: text.len().saturating_add(ITEM_BYTES), items: 1 }
    }

    fn add(&mut self, other: Self) {
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.items = self.items.saturating_add(other.items);
    }

    fn request(request: &Request) -> Self {
        match request {
            Request::Reset(names) => {
                let mut size = Self { bytes: ITEM_BYTES, items: 1 };
                for name in names {
                    size.add(Self::text(name));
                }
                size
            }
            Request::LaunchUrl(s) | Request::Submit(s) | Request::Focus(s) => Self::text(s),
            _ => Self { bytes: ITEM_BYTES, items: 1 },
        }
    }

    fn fits(self, has_notice: bool) -> bool {
        // Reserve room for one honest refusal, even when the data budget is full.
        let reserve_bytes = if has_notice { 0 } else { NOTICE.len() + ITEM_BYTES };
        let reserve_items = usize::from(!has_notice);
        self.bytes <= MAX_OUTPUT_BYTES - reserve_bytes && self.items <= MAX_OUTPUT_ITEMS - reserve_items
    }
}

/// What scripts produced for the viewer since it last looked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JsOutput {
    pub alerts: Vec<String>,
    pub console: Vec<String>,
    pub requests: Vec<Request>,
    /// Script errors (as the viewer's console shows them).
    pub errors: Vec<String>,
    /// At least one script or retained-output budget was exhausted.
    pub limited: bool,
}

impl JsOutput {
    pub fn is_empty(&self) -> bool {
        self.alerts.is_empty() && self.console.is_empty() && self.requests.is_empty() && self.errors.is_empty()
    }

    fn size(&self) -> Size {
        let mut size = Size::default();
        for s in self.alerts.iter().chain(&self.console).chain(&self.errors) {
            size.add(Size::text(s));
        }
        for r in &self.requests {
            size.add(Size::request(r));
        }
        size
    }

    pub(crate) fn refuse(&mut self) {
        self.limited = true;
        if !self.errors.iter().any(|e| e == NOTICE) {
            self.errors.push(NOTICE.into());
        }
    }

    pub(crate) fn batch_refused(&mut self) {
        self.limited = true;
        if !self.errors.iter().any(|e| e == super::budget::BATCH_NOTICE) {
            self.error(super::budget::BATCH_NOTICE);
        }
    }

    /// Check the complete incoming group by reference before cloning any of its strings.
    pub(crate) fn absorb(&mut self, o: &Outcome) -> bool {
        let mut size = self.size();
        for s in o.alerts.iter().chain(&o.console).chain(o.error.iter()) {
            size.add(Size::text(s));
        }
        for r in &o.requests {
            size.add(Size::request(r));
        }
        if !size.fits(self.errors.iter().any(|e| e == NOTICE) || o.error.as_deref() == Some(NOTICE)) {
            self.refuse();
            return false;
        }
        self.alerts.extend(o.alerts.iter().cloned());
        self.console.extend(o.console.iter().cloned());
        self.requests.extend(o.requests.iter().cloned());
        self.errors.extend(o.error.iter().cloned());
        self.limited |= o.limited;
        true
    }

    pub(crate) fn append(&mut self, other: JsOutput) {
        let mut size = self.size();
        size.add(other.size());
        if !size.fits(self.errors.iter().chain(&other.errors).any(|e| e == NOTICE)) {
            self.refuse();
            return;
        }
        self.alerts.extend(other.alerts);
        self.console.extend(other.console);
        self.requests.extend(other.requests);
        self.errors.extend(other.errors);
        self.limited |= other.limited;
    }

    pub(crate) fn error(&mut self, text: &str) -> bool {
        self.text(text, |out| &mut out.errors)
    }

    pub(crate) fn console(&mut self, text: &str) -> bool {
        self.text(text, |out| &mut out.console)
    }

    pub(crate) fn alert(&mut self, text: &str) -> bool {
        self.text(text, |out| &mut out.alerts)
    }

    fn text(&mut self, text: &str, field: fn(&mut Self) -> &mut Vec<String>) -> bool {
        let mut size = self.size();
        size.add(Size::text(text));
        if !size.fits(self.errors.iter().any(|e| e == NOTICE)) {
            self.refuse();
            return false;
        }
        field(self).push(text.to_string());
        true
    }

    pub(crate) fn request(&mut self, request: &Request) -> bool {
        let mut size = self.size();
        size.add(Size::request(request));
        if !size.fits(self.errors.iter().any(|e| e == NOTICE)) {
            self.refuse();
            return false;
        }
        self.requests.push(request.clone());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_output_is_bounded_until_taken_and_reset_names_are_counted() {
        let mut out = JsOutput::default();
        let group = Outcome { console: vec!["x".repeat(1024)], ..Default::default() };
        for _ in 0..10_000 {
            out.absorb(&group);
        }
        assert!(out.limited);
        assert!(out.size().bytes <= MAX_OUTPUT_BYTES && out.size().items <= MAX_OUTPUT_ITEMS);
        assert_eq!(out.errors.iter().filter(|e| *e == NOTICE).count(), 1);
        let old = std::mem::take(&mut out);
        assert!(old.limited);
        assert!(out.absorb(&group));
        assert!(!out.limited);
        let before = out.clone();
        let large = Outcome { requests: vec![Request::Reset(vec![String::new(); MAX_OUTPUT_ITEMS])], ..Default::default() };
        assert!(!out.absorb(&large));
        assert_eq!(out.console, before.console);
        assert!(out.requests.is_empty());
    }

    #[test]
    fn refusal_does_not_clone_or_keep_partial_messages_and_append_is_bounded() {
        let mut out = JsOutput::default();
        let group = Outcome {
            alerts: vec!["should not leak".into()],
            console: vec!["x".repeat(MAX_OUTPUT_BYTES)],
            error: Some("also omitted".into()),
            ..Default::default()
        };
        assert!(!out.absorb(&group));
        assert!(out.alerts.is_empty() && out.console.is_empty());
        assert_eq!(out.errors, [NOTICE]);
        out.append(JsOutput { requests: vec![Request::LaunchUrl("y".repeat(MAX_OUTPUT_BYTES))], ..Default::default() });
        assert!(out.requests.is_empty());
        assert_eq!(out.errors, [NOTICE]);
        // A syntax error does not poison subsequent explicit calls.
        assert!(out.absorb(&Outcome { error: Some("SyntaxError".into()), ..Default::default() }));
        assert!(out.absorb(&Outcome { console: vec!["still usable".into()], ..Default::default() }));
        assert!(out.errors.iter().any(|e| e == "SyntaxError"));
    }

    #[test]
    fn exact_byte_and_item_edges_keep_one_notice_when_appended_to_a_document() {
        let mut bytes = JsOutput::default();
        assert!(bytes.console(&"x".repeat(MAX_OUTPUT_BYTES - NOTICE.len() - 2 * ITEM_BYTES)));
        assert!(!bytes.console("one too many"));
        assert_eq!(bytes.size().bytes, MAX_OUTPUT_BYTES);
        let mut document = JsOutput::default();
        document.append(bytes);
        assert_eq!(document.size().bytes, MAX_OUTPUT_BYTES);
        assert_eq!(document.console.len(), 1, "an already capped group retains its admitted messages");

        let mut items = JsOutput::default();
        assert!(items.absorb(&Outcome { requests: vec![Request::Beep; MAX_OUTPUT_ITEMS - 1], ..Default::default() }));
        assert!(!items.request(&Request::Print));
        items.batch_refused();
        assert_eq!(items.size().items, MAX_OUTPUT_ITEMS);
        assert_eq!(items.errors, [NOTICE]);
    }
}

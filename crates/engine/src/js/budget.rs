//! One automatic event/action batch cannot renew its deadline for every script.
use std::time::Duration;
use web_time::Instant;

pub(crate) const MAX_SCRIPTS: usize = 2_000;
pub(crate) const AUTOMATIC_TIMEOUT: Duration = Duration::from_secs(3);
pub(crate) const ACTION_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const BATCH_NOTICE: &str = "The script batch was stopped after reaching its resource budget; remaining actions were skipped";
pub(crate) const SNAPSHOT_NOTICE: &str = "The form's script snapshot exceeds its input budget";
pub(crate) const SNAPSHOT_ITEMS: usize = 4 * 1024 * 1024 / 256;

/// Admission for copies of existing model strings, before constructing a guest snapshot.
pub(crate) fn snapshot_fits<'a>(items: usize, strings: impl Iterator<Item = &'a str>) -> bool {
    let Some(mut remaining) = (4usize * 1024 * 1024).checked_sub(items.saturating_mul(256)) else { return false };
    for s in strings {
        let Some(left) = remaining.checked_sub(s.len().saturating_add(32)) else { return false };
        remaining = left;
    }
    true
}

pub(crate) struct Budget {
    started: Instant,
    timeout: Duration,
    runs: usize,
    stopped: bool,
}

impl Budget {
    pub fn new(timeout: Duration) -> Self {
        Self { started: Instant::now(), timeout, runs: 0, stopped: false }
    }

    pub fn begin(&mut self) -> Option<Duration> {
        self.begin_at(Instant::now())
    }

    fn begin_at(&mut self, now: Instant) -> Option<Duration> {
        let remaining = self.timeout.saturating_sub(now.saturating_duration_since(self.started));
        if self.stopped || self.runs >= MAX_SCRIPTS || remaining.is_zero() {
            self.stopped = true;
            return None;
        }
        self.runs += 1;
        Some(remaining)
    }

    pub fn stop(&mut self) {
        self.stopped = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_deadline_and_refusal_cannot_be_renewed_by_later_scripts() {
        let mut b = Budget::new(ACTION_TIMEOUT);
        for _ in 0..MAX_SCRIPTS {
            assert!(b.begin_at(b.started).is_some());
        }
        assert!(b.begin_at(b.started).is_none());
        let mut b = Budget::new(ACTION_TIMEOUT);
        assert_eq!(b.begin_at(b.started + Duration::from_secs(4)), Some(Duration::from_secs(1)));
        assert!(b.begin_at(b.started + ACTION_TIMEOUT).is_none());
        assert!(b.begin_at(b.started).is_none());
        let mut b = Budget::new(AUTOMATIC_TIMEOUT);
        b.stop();
        assert!(b.begin_at(b.started).is_none());
        assert!(Budget::new(AUTOMATIC_TIMEOUT).begin().is_some(), "a later explicit batch is usable");
    }

    #[test]
    fn borrowed_snapshot_admission_caps_empty_items_and_large_strings() {
        assert!(snapshot_fits(2, ["name", "value"].into_iter()));
        assert!(!snapshot_fits(usize::MAX, std::iter::empty()));
        assert!(!snapshot_fits(20_000, std::iter::empty()));
        assert!(!snapshot_fits(0, std::iter::once("x".repeat(4 * 1024 * 1024).as_str())));
    }
}

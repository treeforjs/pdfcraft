//! Resource policy shared by every password-based operation in one identity envelope.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

pub const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_ROUNDS: u64 = 2_000_000;
const MAX_WORK: u64 = 8_000_000;
pub(super) const MAX_SALT: usize = 64 * 1024;
pub(super) const MAX_PASSWORD: usize = 4096;
pub(super) const MAX_BAGS: usize = 256;

/// Cancellation is cooperative within derivation loops (checked at least every 256 rounds).
/// Native imports also have a 30-second absolute deadline. The deterministic work budget applies
/// on every target, including browsers where blocking cryptographic work has no wall-clock timer.
#[derive(Clone, Debug)]
pub struct ImportControl {
    cancelled: Arc<AtomicBool>,
    #[cfg(test)]
    checks: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(not(target_arch = "wasm32"))]
    start: Instant,
}

impl Default for ImportControl {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            checks: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(not(target_arch = "wasm32"))]
            start: Instant::now(),
        }
    }
}

impl ImportControl {
    /// Share an existing cancellation flag while starting a fresh operation deadline.
    pub fn with_cancellation(cancelled: Arc<AtomicBool>) -> Self {
        Self { cancelled, ..Self::default() }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
    /// Check cancellation and the native operation deadline before another unit of work.
    pub fn check(&self) -> Result<(), &'static str> {
        if self.is_cancelled() {
            return Err("identity operation cancelled");
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.start.elapsed() >= Duration::from_secs(30) {
            return Err("identity operation exceeded 30 seconds");
        }
        #[cfg(test)]
        self.checks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn successful_checks(&self) -> usize {
        self.checks.load(Ordering::SeqCst)
    }
}

pub(super) struct Budget<'a> {
    pub(super) control: &'a ImportControl,
    left: u64,
    bags_left: usize,
}

impl<'a> Budget<'a> {
    pub(super) fn new(control: &'a ImportControl) -> Self {
        Self { control, left: MAX_WORK, bags_left: MAX_BAGS }
    }
    pub(super) fn bag(&mut self) -> Result<(), &'static str> {
        self.control.check()?;
        self.bags_left = self.bags_left.checked_sub(1).ok_or("at most 256 identity bags are allowed")?;
        Ok(())
    }
    pub(super) fn charge(&mut self, rounds: u64, blocks: usize, input_bytes: usize) -> Result<u32, &'static str> {
        self.control.check()?;
        if rounds == 0 || rounds > MAX_ROUNDS {
            return Err("derivation count must be between 1 and 2000000");
        }
        let blocks = u64::try_from(blocks).map_err(|_| "derivation size exceeds the work budget")?;
        let input = u64::try_from(input_bytes.div_ceil(64)).map_err(|_| "derivation input exceeds the work budget")?;
        let work = rounds.checked_add(input).and_then(|n| n.checked_mul(blocks)).ok_or("derivation work overflow")?;
        self.left = self.left.checked_sub(work).ok_or("combined derivation work exceeds the identity budget")?;
        u32::try_from(rounds).map_err(|_| "derivation count overflow")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_are_not_truncated_or_normalized() {
        for rounds in [0, 2_000_001, u64::from(u32::MAX), u64::from(u32::MAX) + 1, u64::MAX] {
            assert!(Budget::new(&ImportControl::default()).charge(rounds, 1, 0).is_err());
        }
    }
    #[test]
    fn aggregate_budget_counts_all_bags_and_output_blocks() {
        let control = ImportControl::default();
        let mut budget = Budget::new(&control);
        assert!(budget.charge(2_000_000, 3, 0).is_ok());
        assert!(budget.charge(2_000_000, 1, 0).is_ok());
        assert!(budget.charge(1, 1, 0).is_err());
    }
    #[test]
    fn unknown_bags_also_consume_the_combined_count_budget() {
        let control = ImportControl::default();
        let mut budget = Budget::new(&control);
        for _ in 0..MAX_BAGS {
            assert!(budget.bag().is_ok());
        }
        assert!(budget.bag().is_err());
    }
    #[test]
    fn cancellation_is_shared_with_the_worker() {
        let control = ImportControl::default();
        let worker = control.clone();
        control.cancel();
        assert_eq!(worker.check(), Err("identity operation cancelled"));
        assert!(Budget::new(&worker).charge(1, 1, 0).is_err());
    }
}

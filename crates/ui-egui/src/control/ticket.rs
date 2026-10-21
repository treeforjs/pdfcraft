use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

const QUEUED: u8 = 0;
const STARTED: u8 = 1;
const CANCELLED: u8 = 2;

/// Cancellation and UI dispatch compete for one decision. Once claimed, an operation cannot
/// be described as cancelled merely because its response did not reach the client.
#[derive(Clone, Default)]
pub struct Ticket(Arc<AtomicU8>);

impl Ticket {
    pub fn cancel_before_start(&self) -> bool {
        self.0.compare_exchange(QUEUED, CANCELLED, Ordering::AcqRel, Ordering::Acquire).is_ok() || self.0.load(Ordering::Acquire) == CANCELLED
    }
    pub(super) fn start(&self) -> bool {
        self.0.compare_exchange(QUEUED, STARTED, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timeout_prevents_queued_work_from_ever_starting() {
        let ticket = Ticket::default();
        assert!(ticket.cancel_before_start());
        assert!(ticket.cancel_before_start());
        assert!(!ticket.start());
    }
    #[test]
    fn started_work_cannot_be_reported_as_cancelled() {
        let ticket = Ticket::default();
        assert!(ticket.start());
        assert!(!ticket.cancel_before_start());
        assert!(!ticket.start());
    }
    #[test]
    fn dispatch_and_timeout_have_exactly_one_winner() {
        for _ in 0..128 {
            let ticket = Ticket::default();
            let other = ticket.clone();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let ready = barrier.clone();
            let worker = std::thread::spawn(move || {
                ready.wait();
                other.start()
            });
            barrier.wait();
            assert_ne!(ticket.cancel_before_start(), worker.join().unwrap());
        }
    }
}

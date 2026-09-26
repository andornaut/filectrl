use std::{
    sync::mpsc::Sender,
    time::{Duration, Instant},
};

use super::path_info::PathInfo;
use crate::command::Command;

/// Interval after which a partial batch is flushed, so sparse results still
/// stream. Each flush redraws.
pub(super) const BATCH_FLUSH_INTERVAL: Duration = crate::UI_TIMER_FLOOR;

/// The send closure for a `ListingBatch` stamped with `generation`; returns
/// whether the channel is still open.
pub(super) fn batch_sender(
    tx: &Sender<Command>,
    generation: u64,
) -> impl Fn(Vec<PathInfo>) -> bool {
    move |items| tx.send(Command::ListingBatch { items, generation }).is_ok()
}

/// Accumulates `PathInfo`s and flushes them once `max_size` is reached or
/// `interval` elapses. One command per item would sit ahead of terminal input in
/// the single FIFO channel. `send` returns `false` once the channel is closed.
pub(super) struct Batcher {
    batch: Vec<PathInfo>,
    last_flush: Instant,
    max_size: usize,
    interval: Duration,
}

impl Batcher {
    pub(super) fn new(max_size: usize, interval: Duration) -> Self {
        Self {
            batch: Vec::new(),
            last_flush: Instant::now(),
            max_size,
            interval,
        }
    }

    /// Adds an item, flushing if the batch is full or the interval has elapsed.
    /// Returns `false` if the channel is closed.
    pub(super) fn push<F: Fn(Vec<PathInfo>) -> bool>(&mut self, item: PathInfo, send: &F) -> bool {
        self.batch.push(item);
        if self.batch.len() >= self.max_size {
            self.flush(send)
        } else {
            self.flush_if_due(send)
        }
    }

    /// Flushes the pending batch if the interval has elapsed. Returns `false` if
    /// the channel is closed.
    pub(super) fn flush_if_due<F: Fn(Vec<PathInfo>) -> bool>(&mut self, send: &F) -> bool {
        if self.last_flush.elapsed() >= self.interval {
            self.flush(send)
        } else {
            true
        }
    }

    /// Sends the pending batch, if any, and restarts the interval. Returns `false`
    /// if the channel is closed.
    pub(super) fn flush<F: Fn(Vec<PathInfo>) -> bool>(&mut self, send: &F) -> bool {
        self.last_flush = Instant::now();
        if self.batch.is_empty() {
            return true;
        }
        send(std::mem::take(&mut self.batch))
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, path::Path};

    use super::*;

    /// A never-elapsing interval, so only the size rule can flush.
    const NEVER: Duration = Duration::from_hours(24);

    fn item() -> PathInfo {
        PathInfo::try_from(Path::new(".")).unwrap()
    }

    fn recorder(sizes: &RefCell<Vec<usize>>) -> impl Fn(Vec<PathInfo>) -> bool + '_ {
        move |items| {
            sizes.borrow_mut().push(items.len());
            true
        }
    }

    #[test]
    fn items_are_held_until_the_batch_is_full() {
        let sizes = RefCell::new(Vec::new());
        let send = recorder(&sizes);
        let mut batcher = Batcher::new(3, NEVER);

        assert!(batcher.push(item(), &send));
        assert!(batcher.push(item(), &send));
        assert!(sizes.borrow().is_empty());

        assert!(batcher.push(item(), &send));
        assert_eq!(vec![3], *sizes.borrow());
    }

    #[test]
    fn an_elapsed_interval_flushes_a_partial_batch() {
        let sizes = RefCell::new(Vec::new());
        let send = recorder(&sizes);
        // A zero interval is always due.
        let mut batcher = Batcher::new(1_000, Duration::ZERO);

        assert!(batcher.push(item(), &send));
        assert_eq!(vec![1], *sizes.borrow());
    }

    #[test]
    fn flushing_an_empty_batch_sends_nothing() {
        let sizes = RefCell::new(Vec::new());
        let send = recorder(&sizes);
        let mut batcher = Batcher::new(3, Duration::ZERO);

        assert!(batcher.flush(&send));
        assert!(batcher.flush_if_due(&send));
        assert!(sizes.borrow().is_empty());
    }

    #[test]
    fn a_closed_channel_is_reported_so_the_producer_stops() {
        let closed = |_: Vec<PathInfo>| false;
        let mut batcher = Batcher::new(1, NEVER);

        assert!(!batcher.push(item(), &closed));

        let mut batcher = Batcher::new(10, NEVER);
        batcher.push(item(), &|_| true);
        assert!(!batcher.flush(&closed));
    }

    #[test]
    fn a_flush_restarts_the_interval() {
        let sizes = RefCell::new(Vec::new());
        let send = recorder(&sizes);
        let interval = Duration::from_secs(1);
        let mut batcher = Batcher::new(1_000, interval);
        // Backdated so the first push is due, keeping the test off the wall clock.
        batcher.last_flush = Instant::now()
            .checked_sub(interval)
            .expect("the monotonic clock should be past the interval");

        assert!(batcher.push(item(), &send));
        assert_eq!(vec![1], *sizes.borrow());

        assert!(batcher.push(item(), &send));
        assert_eq!(vec![1], *sizes.borrow());
    }
}

use std::{
    sync::mpsc::Sender,
    time::{Duration, Instant},
};

use super::path_info::PathInfo;
use crate::command::Command;

/// Default interval after which a partial batch is flushed even if not full, so
/// results still stream visibly when items arrive sparsely. Each flush redraws
/// the screen, and 100 ms is the shortest interval at which redrawing buys any
/// perceived responsiveness, so nothing below it is worth the wakeup.
pub(super) const BATCH_FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// The per-batch send closure shared by the streaming producers: builds a
/// `ListingBatch` stamped with `generation` and reports whether the channel
/// is still open.
pub(super) fn batch_sender(
    tx: &Sender<Command>,
    generation: u64,
) -> impl Fn(Vec<PathInfo>) -> bool {
    move |items| tx.send(Command::ListingBatch { items, generation }).is_ok()
}

/// Accumulates `PathInfo`s and flushes them in batches through a caller-supplied
/// sender, once one reaches `max_size` or `interval` elapses. The directory
/// loader and the recursive search stream this way rather than sending one
/// command per item, which would sit ahead of terminal input in the single FIFO
/// channel and make the UI unresponsive.
///
/// The `send` closure builds and sends the per-batch command, returning `false`
/// once the channel is closed, at which point the producer stops.
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

    /// Add an item, flushing first if the batch is now full or the flush
    /// interval has elapsed. Returns `false` if the channel is closed.
    pub(super) fn push<F: Fn(Vec<PathInfo>) -> bool>(&mut self, item: PathInfo, send: &F) -> bool {
        self.batch.push(item);
        if self.batch.len() >= self.max_size {
            self.flush(send)
        } else {
            self.flush_if_due(send)
        }
    }

    /// Flush the pending batch if the flush interval has elapsed. Producers that
    /// add items sparsely call this between items so results still stream.
    /// Returns `false` if the channel is closed.
    pub(super) fn flush_if_due<F: Fn(Vec<PathInfo>) -> bool>(&mut self, send: &F) -> bool {
        if self.last_flush.elapsed() >= self.interval {
            self.flush(send)
        } else {
            true
        }
    }

    /// Send the pending batch (if any) and reset the interval timer. Returns
    /// `false` if the channel is closed.
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
    const NEVER: Duration = Duration::from_secs(86_400);

    fn item() -> PathInfo {
        PathInfo::try_from(Path::new(".")).unwrap()
    }

    /// Records the size of each flushed batch and reports the channel as open.
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
        // Batching exists to keep a flood of per-item commands from sitting
        // ahead of terminal input in the single command channel.
        assert!(sizes.borrow().is_empty());

        assert!(batcher.push(item(), &send));
        assert_eq!(vec![3], *sizes.borrow());
    }

    #[test]
    fn an_elapsed_interval_flushes_a_partial_batch() {
        let sizes = RefCell::new(Vec::new());
        let send = recorder(&sizes);
        // A zero interval is always due, standing in for a producer whose
        // matches arrive more sparsely than the flush interval.
        let mut batcher = Batcher::new(1_000, Duration::ZERO);

        assert!(batcher.push(item(), &send));
        assert_eq!(vec![1], *sizes.borrow());
    }

    #[test]
    fn flushing_an_empty_batch_sends_nothing() {
        let sizes = RefCell::new(Vec::new());
        let send = recorder(&sizes);
        let mut batcher = Batcher::new(3, Duration::ZERO);

        // An empty batch must not send a ListingBatch: consumers would count
        // it as a completed batch of zero items.
        assert!(batcher.flush(&send));
        assert!(batcher.flush_if_due(&send));
        assert!(sizes.borrow().is_empty());
    }

    #[test]
    fn a_closed_channel_is_reported_so_the_producer_stops() {
        let closed = |_: Vec<PathInfo>| false;
        let mut batcher = Batcher::new(1, NEVER);

        // Every producer treats `false` as "stop walking": the receiver is
        // gone, so any further work is wasted.
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
        // Backdate the timer so the first push is due. Reaching into the field
        // keeps the test off the wall clock: the two pushes below are
        // microseconds apart, so only the reset inside `flush` can decide
        // whether the second one flushes.
        batcher.last_flush = Instant::now()
            .checked_sub(interval)
            .expect("the monotonic clock should be past the interval");

        assert!(batcher.push(item(), &send));
        assert_eq!(vec![1], *sizes.borrow());

        // Without the reset the timer would still read as elapsed, and a
        // producer adding items sparsely would send each one as its own batch.
        assert!(batcher.push(item(), &send));
        assert_eq!(vec![1], *sizes.borrow());
    }
}

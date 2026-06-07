use std::time::{Duration, Instant};

use super::path_info::PathInfo;

/// Default interval after which a partial batch is flushed even if not full, so
/// results still stream visibly when items arrive sparsely.
pub(super) const BATCH_FLUSH_INTERVAL: Duration = Duration::from_millis(80);

/// Accumulates `PathInfo`s and flushes them as batches through a caller-supplied
/// sender. Background producers (the directory loader and the recursive search)
/// stream results this way rather than sending one command per item: a flood of
/// individual commands would sit ahead of terminal input in the single FIFO
/// command channel and make the UI unresponsive. A batch is sent once it reaches
/// `max_size` or `interval` has elapsed, whichever comes first.
///
/// The `send` closure builds and sends the per-batch command, returning `false`
/// if the channel is closed (the producer should then stop).
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

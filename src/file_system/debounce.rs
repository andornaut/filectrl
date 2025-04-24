use std::time::{Duration, Instant};

/// Debounces progress updates based on bytes processed
///
/// This debouncer ensures we get at least one progress update for any file size by using the `has_triggered` flag.
/// For 0KB files, the threshold will be 0, and for non-zero files, updates are sent when:
/// 1. The first chunk of data is processed (thanks to `has_triggered`)
/// 2. Subsequent chunks that exceed the percentage-based threshold
pub struct BytesDebouncer {
    current_bytes: u64,
    has_triggered: bool,
    threshold: u64,
}

impl BytesDebouncer {
    pub fn new(debounce_threshold_percentage: u64, total_size: u64) -> Self {
        Self {
            current_bytes: 0,
            has_triggered: false,
            threshold: (total_size * debounce_threshold_percentage) / 100,
        }
    }

    pub fn should_trigger(&mut self, additional_bytes: u64) -> bool {
        self.current_bytes += additional_bytes;
        if !self.has_triggered || self.current_bytes >= self.threshold {
            self.current_bytes = 0; // Reset for next threshold
            self.has_triggered = true;
            true
        } else {
            false
        }
    }
}

/// Debounces events based on time intervals
///
/// This debouncer ensures we don't process events too frequently by enforcing a minimum time interval
/// between triggers. When an event arrives:
/// 1. If enough time has passed since the last trigger, it triggers immediately
/// 2. If not enough time has passed, the event is delayed and will trigger after the debounce period
/// 3. Multiple events within the debounce period will only result in one delayed trigger
pub struct TimeDebouncer {
    last_triggered: Option<Instant>,
    threshold: Duration,
    has_delayed_event: bool,
}

impl TimeDebouncer {
    pub fn new(debounce_threshold: Duration) -> Self {
        Self {
            last_triggered: None,
            threshold: debounce_threshold,
            has_delayed_event: false,
        }
    }

    pub fn should_trigger(&mut self, at: Instant) -> bool {
        let time_since_last_trigger = self
            .last_triggered
            .map(|last_triggered| at.duration_since(last_triggered));

        // If we've never triggered or enough time has passed
        if time_since_last_trigger.is_none() || time_since_last_trigger.unwrap() >= self.threshold {
            self.last_triggered = Some(at);
            self.has_delayed_event = false;
            true
        } else {
            false
        }
    }

    pub fn has_delayed_event(&self) -> bool {
        self.has_delayed_event
    }

    pub fn set_delayed_event(&mut self) {
        self.has_delayed_event = true;
    }
}

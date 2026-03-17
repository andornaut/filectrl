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
        if time_since_last_trigger.is_none_or(|d| d >= self.threshold) {
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

#[cfg(test)]
mod tests {
    use super::*;

    mod bytes_debouncer {
        use super::*;

        #[test]
        fn first_call_always_triggers() {
            let mut d = BytesDebouncer::new(5, 1_000_000);
            assert!(d.should_trigger(1));
        }

        #[test]
        fn second_call_below_threshold_does_not_trigger() {
            let mut d = BytesDebouncer::new(5, 1_000_000); // threshold = 50_000 bytes
            d.should_trigger(1); // first call always triggers
            assert!(!d.should_trigger(1_000)); // well below threshold
        }

        #[test]
        fn call_at_threshold_triggers() {
            let mut d = BytesDebouncer::new(5, 1_000_000); // threshold = 50_000 bytes
            d.should_trigger(1); // first call
            assert!(d.should_trigger(50_000));
        }

        #[test]
        fn zero_byte_file_always_triggers() {
            // threshold = 0, so every call should trigger
            let mut d = BytesDebouncer::new(5, 0);
            assert!(d.should_trigger(0));
            assert!(d.should_trigger(0));
        }
    }

    mod time_debouncer {
        use super::*;

        #[test]
        fn first_call_always_triggers() {
            let mut d = TimeDebouncer::new(Duration::from_millis(100));
            assert!(d.should_trigger(Instant::now()));
        }

        #[test]
        fn call_within_threshold_does_not_trigger() {
            let mut d = TimeDebouncer::new(Duration::from_millis(100));
            let now = Instant::now();
            d.should_trigger(now);
            assert!(!d.should_trigger(now + Duration::from_millis(50)));
        }

        #[test]
        fn call_at_threshold_triggers() {
            let mut d = TimeDebouncer::new(Duration::from_millis(100));
            let now = Instant::now();
            d.should_trigger(now);
            assert!(d.should_trigger(now + Duration::from_millis(100)));
        }

        #[test]
        fn delayed_event_roundtrip() {
            let mut d = TimeDebouncer::new(Duration::from_millis(100));
            assert!(!d.has_delayed_event());
            d.set_delayed_event();
            assert!(d.has_delayed_event());
        }

        #[test]
        fn triggering_clears_delayed_event() {
            let mut d = TimeDebouncer::new(Duration::from_millis(100));
            let now = Instant::now();
            d.should_trigger(now);
            d.set_delayed_event();
            assert!(d.has_delayed_event());
            d.should_trigger(now + Duration::from_millis(100));
            assert!(!d.has_delayed_event());
        }
    }
}

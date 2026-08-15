use std::time::{Duration, Instant};

/// Debounces progress updates on units processed and on elapsed time, a unit
/// being whatever the caller counts: bytes copied, or entries removed.
///
/// Count alone bounds updates per unit of work, so a copy fast enough to finish
/// in a second sends its whole percentage ladder inside that second, each one a
/// redraw. The floor bounds the rate; the count keeps an operation making no
/// progress from sending at all.
///
/// The first call always triggers, so any total reports at least once. For an
/// empty total the threshold is 0 and every call past the floor triggers.
pub struct ProgressDebouncer {
    current_count: u64,
    has_triggered: bool,
    last_triggered: Option<Instant>,
    min_interval: Duration,
    threshold: u64,
}

impl ProgressDebouncer {
    pub fn new(debounce_threshold_percentage: u64, min_interval: Duration, total: u64) -> Self {
        Self {
            current_count: 0,
            has_triggered: false,
            last_triggered: None,
            min_interval,
            // saturating_mul guards against overflow for very large totals;
            // the product is divided down to the percentage threshold.
            threshold: total.saturating_mul(debounce_threshold_percentage) / 100,
        }
    }

    pub fn should_trigger(&mut self, at: Instant, additional: u64) -> bool {
        self.current_count += additional;
        if self.has_triggered {
            if self.current_count < self.threshold {
                return false;
            }
            // The count is due but the floor has not elapsed. Hold the count
            // rather than resetting it, so a suppressed update does not cost
            // another whole threshold's worth of work before the next one.
            if self
                .last_triggered
                .is_some_and(|last| at.duration_since(last) < self.min_interval)
            {
                return false;
            }
        }
        self.current_count = 0;
        self.has_triggered = true;
        self.last_triggered = Some(at);
        true
    }
}

/// Enforces a minimum interval between triggers. An event arriving after the
/// window triggers at once; one arriving inside it is delayed to the end of the
/// window, and several inside it produce that one delayed trigger.
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

        if time_since_last_trigger.is_none_or(|d| d >= self.threshold) {
            self.last_triggered = Some(at);
            self.has_delayed_event = false;
            true
        } else {
            false
        }
    }

    /// Time left until the debounce window ends: zero if nothing has
    /// triggered yet or the window has already elapsed.
    pub fn remaining(&self, at: Instant) -> Duration {
        self.last_triggered.map_or(Duration::ZERO, |last| {
            self.threshold.saturating_sub(at.duration_since(last))
        })
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

    mod progress_debouncer {
        use super::*;

        const FLOOR: Duration = Duration::from_millis(100);
        /// Long enough that the time floor never suppresses anything, so a test
        /// exercises the count rule alone.
        const LATER: Duration = Duration::from_secs(1);

        #[test]
        fn first_call_always_triggers() {
            let mut d = ProgressDebouncer::new(5, FLOOR, 1_000_000);
            assert!(d.should_trigger(Instant::now(), 1));
        }

        #[test]
        fn second_call_below_threshold_does_not_trigger() {
            let mut d = ProgressDebouncer::new(5, FLOOR, 1_000_000); // threshold = 50_000 bytes
            let now = Instant::now();
            d.should_trigger(now, 1); // first call always triggers
            assert!(!d.should_trigger(now + LATER, 1_000)); // well below threshold
        }

        #[test]
        fn call_at_threshold_triggers() {
            let mut d = ProgressDebouncer::new(5, FLOOR, 1_000_000); // threshold = 50_000 bytes
            let now = Instant::now();
            d.should_trigger(now, 1); // first call
            assert!(d.should_trigger(now + LATER, 50_000));
        }

        #[test]
        fn zero_total_always_triggers_once_the_floor_elapses() {
            // threshold = 0, so only the time floor can suppress a call
            let mut d = ProgressDebouncer::new(5, FLOOR, 0);
            let now = Instant::now();
            assert!(d.should_trigger(now, 0));
            assert!(d.should_trigger(now + LATER, 0));
        }

        #[test]
        fn very_large_total_does_not_overflow() {
            // total_size * percentage would overflow u64; saturating_mul keeps
            // the threshold finite instead of panicking (debug) or wrapping.
            let mut d = ProgressDebouncer::new(50, FLOOR, u64::MAX);
            let now = Instant::now();
            assert!(d.should_trigger(now, 1)); // first call always triggers
            assert!(!d.should_trigger(now + LATER, 1)); // below the (huge) threshold
        }

        #[test]
        fn a_due_count_within_the_floor_does_not_trigger() {
            let mut d = ProgressDebouncer::new(1, FLOOR, 1_000); // threshold = 10
            let now = Instant::now();
            d.should_trigger(now, 1); // first call
            assert!(!d.should_trigger(now + Duration::from_millis(99), 10));
        }

        #[test]
        fn a_count_held_through_the_floor_triggers_at_once() {
            let mut d = ProgressDebouncer::new(1, FLOOR, 1_000); // threshold = 10
            let now = Instant::now();
            d.should_trigger(now, 1); // first call
            d.should_trigger(now + Duration::from_millis(50), 10); // suppressed

            // The suppressed count is held rather than discarded, so the next
            // call past the floor triggers without re-earning the threshold.
            assert!(d.should_trigger(now + FLOOR, 0));
        }

        #[test]
        fn the_floor_is_measured_from_the_last_trigger() {
            let mut d = ProgressDebouncer::new(1, FLOOR, 1_000); // threshold = 10
            let now = Instant::now();
            d.should_trigger(now, 1); // first call
            assert!(d.should_trigger(now + FLOOR, 10));
            assert!(!d.should_trigger(now + FLOOR + Duration::from_millis(99), 10));
            assert!(d.should_trigger(now + FLOOR + FLOOR, 0));
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
        fn remaining_counts_down_from_last_trigger() {
            let mut d = TimeDebouncer::new(Duration::from_millis(100));
            let now = Instant::now();
            assert_eq!(Duration::ZERO, d.remaining(now)); // never triggered
            d.should_trigger(now);
            assert_eq!(
                Duration::from_millis(60),
                d.remaining(now + Duration::from_millis(40))
            );
            assert_eq!(
                Duration::ZERO,
                d.remaining(now + Duration::from_millis(150))
            );
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

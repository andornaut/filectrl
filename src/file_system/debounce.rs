use std::time::{Duration, Instant};

/// Debounces progress updates on units processed (bytes copied, entries
/// removed) and on elapsed time. The count alone would let a fast copy send its
/// whole percentage ladder inside one second; the time floor bounds the rate.
/// The first call always triggers.
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
            threshold: total.saturating_mul(debounce_threshold_percentage) / 100,
        }
    }

    pub fn should_trigger(&mut self, at: Instant, additional: u64) -> bool {
        self.current_count += additional;
        if self.has_triggered {
            if self.current_count < self.threshold {
                return false;
            }
            // Due but inside the floor: hold the count, so the next update does not cost
            // another whole threshold.
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

    #[cfg(test)]
    pub fn threshold(&self) -> u64 {
        self.threshold
    }
}

/// Enforces a minimum interval between triggers. Events inside the window
/// produce one trigger delayed to its end.
pub struct TimeDebouncer {
    last_triggered: Option<Instant>,
    threshold: Duration,
}

impl TimeDebouncer {
    pub fn new(debounce_threshold: Duration) -> Self {
        Self {
            last_triggered: None,
            threshold: debounce_threshold,
        }
    }

    pub fn should_trigger(&mut self, at: Instant) -> bool {
        let time_since_last_trigger = self
            .last_triggered
            .map(|last_triggered| at.duration_since(last_triggered));

        if time_since_last_trigger.is_none_or(|d| d >= self.threshold) {
            self.last_triggered = Some(at);
            true
        } else {
            false
        }
    }

    /// Time left in the debounce window: zero if nothing has triggered yet or the
    /// window has elapsed.
    pub fn remaining(&self, at: Instant) -> Duration {
        self.last_triggered.map_or(Duration::ZERO, |last| {
            self.threshold.saturating_sub(at.duration_since(last))
        })
    }

    pub fn set_threshold(&mut self, threshold: Duration) {
        self.threshold = threshold;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod progress_debouncer {
        use super::*;

        const FLOOR: Duration = Duration::from_millis(100);
        /// Long enough that the time floor never suppresses anything.
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
            // A trigger restarts the count, so the 49_999 does not carry over.
            d.should_trigger(now, 49_999);
            assert!(!d.should_trigger(now + LATER, 1_000));
        }

        #[test]
        fn call_at_threshold_triggers() {
            let mut d = ProgressDebouncer::new(5, FLOOR, 1_000_000); // threshold = 50_000 bytes
            let now = Instant::now();
            d.should_trigger(now, 1); // first call
            assert!(d.should_trigger(now + LATER, 50_000));
        }

        #[test]
        fn one_percent_of_the_total_is_due_and_less_is_not() {
            let mut d = ProgressDebouncer::new(1, FLOOR, 1_000); // threshold = 10
            let now = Instant::now();
            d.should_trigger(now, 1); // first call
            assert!(!d.should_trigger(now + LATER, 9));
            assert!(d.should_trigger(now + LATER, 1));
        }

        #[test]
        fn zero_total_always_triggers_once_the_floor_elapses() {
            let mut d = ProgressDebouncer::new(5, FLOOR, 0);
            let now = Instant::now();
            assert!(d.should_trigger(now, 0));
            assert!(d.should_trigger(now + LATER, 0));
        }

        #[test]
        fn very_large_total_does_not_overflow() {
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
    }
}

use std::time::{Duration, Instant};

/// Debounces progress updates based on bytes processed
pub struct BytesDebouncer {
    current_bytes: u64,
    threshold: u64,
}

impl BytesDebouncer {
    pub fn new(debounce_threshold_percentage: u64, total_size: u64) -> Self {
        Self {
            current_bytes: 0,
            threshold: (total_size * debounce_threshold_percentage) / 100,
        }
    }

    pub fn should_trigger(&mut self, bytes: u64) -> bool {
        self.current_bytes += bytes;
        if self.current_bytes >= self.threshold {
            self.current_bytes = 0; // Reset for next threshold
            true
        } else {
            false
        }
    }
}

/// Debounces events based on time intervals
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

    pub fn should_trigger(&mut self) -> bool {
        let now = Instant::now();
        let time_since_last_trigger = self.last_triggered.map(|last| now.duration_since(last));

        // If we've never triggered or enough time has passed
        if time_since_last_trigger.is_none() || time_since_last_trigger.unwrap() >= self.threshold {
            self.last_triggered = Some(now); // Reset for the next threshold
            true
        } else {
            false
        }
    }
}

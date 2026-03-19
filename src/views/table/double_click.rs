use std::time::{Duration, Instant};

use crate::{app::config::Config, file_system::path_info::PathInfo};

#[derive(Default)]
pub(super) struct DoubleClick {
    last_path: Option<PathInfo>,
    start: Option<Instant>,
    threshold: Duration,
}

impl DoubleClick {
    pub(super) fn new(config: &Config) -> Self {
        let threshold =
            Duration::from_millis(config.ui.double_click_interval_milliseconds as u64);
        Self {
            threshold,
            ..Default::default()
        }
    }

    pub(super) fn click_and_is_double_click(&mut self, path: &PathInfo) -> bool {
        let item = Some(path.clone());
        if let Some(start) = self.start
            && start.elapsed() <= self.threshold
            && self.last_path == item
        {
            self.start = None;
            self.last_path = None;
            return true;
        }
        self.start = Some(Instant::now());
        self.last_path = item;
        false
    }
}

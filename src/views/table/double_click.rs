use std::time::{Duration, Instant};

use crate::{app::config::Config, file_system::path_info::PathInfo};

pub(super) struct DoubleClick {
    last_path: Option<PathInfo>,
    start: Option<Instant>,
    threshold: Duration,
}

impl Default for DoubleClick {
    fn default() -> Self {
        let ms = Config::global().ui.double_click_interval_milliseconds;
        Self {
            last_path: None,
            start: None,
            threshold: Duration::from_millis(ms as u64),
        }
    }
}

impl DoubleClick {
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

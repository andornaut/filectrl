use std::time::Instant;

use crate::{app::config::Config, file_system::path_info::PathInfo};

#[derive(Default)]
pub(super) struct DoubleClick {
    last_path: Option<PathInfo>,
    start: Option<Instant>,
    threshold_milliseconds: u16,
}

impl DoubleClick {
    pub(super) fn new(config: &Config) -> Self {
        let threshold_milliseconds = config.ui.double_click_interval_milliseconds;
        Self {
            threshold_milliseconds,
            ..Default::default()
        }
    }

    pub(super) fn click_and_is_double_click(&mut self, path: &PathInfo) -> bool {
        let item = Some(path.clone());
        if let Some(start) = self.start
            && start.elapsed().as_millis() <= self.threshold_milliseconds as u128
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

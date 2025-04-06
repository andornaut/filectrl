use std::time::Instant;

use crate::file_system::path_info::PathInfo;

const DEFAULT_THRESHOLD_MILLISECONDS: u16 = 300;

#[derive(Default)]
pub struct DoubleClick {
    last_path: Option<PathInfo>,
    start: Option<Instant>,
    threshold_milliseconds: u16,
}

impl DoubleClick {
    pub fn new(threshold_milliseconds: Option<u16>) -> Self {
        let threshold_milliseconds =
            threshold_milliseconds.unwrap_or(DEFAULT_THRESHOLD_MILLISECONDS);
        Self {
            threshold_milliseconds,
            ..Default::default()
        }
    }

    pub fn click_and_check_for_double_click(&mut self, path: &PathInfo) -> bool {
        let item = Some(path.clone());
        if let Some(start) = self.start {
            if start.elapsed().as_millis() <= self.threshold_milliseconds as u128
                && self.last_path == item
            {
                self.start = None;
                self.last_path = None;
                return true;
            }
        }
        self.start = Some(Instant::now());
        self.last_path = item;
        false
    }
}

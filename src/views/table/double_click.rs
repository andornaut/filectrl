use std::time::Instant;

use crate::{app::config::Config, file_system::path_info::PathInfo};
use smart_default::SmartDefault;

const DEFAULT_THRESHOLD_MILLISECONDS: u16 = 300;

#[derive(SmartDefault)]
pub struct DoubleClick {
    last_path: Option<PathInfo>,
    start: Option<Instant>,
    #[default(DEFAULT_THRESHOLD_MILLISECONDS)]
    threshold_milliseconds: u16,
}

impl DoubleClick {
    pub fn new(config: &Config) -> Self {
        let threshold_milliseconds = config
            .double_click_threshold_milliseconds
            .expect("double_click_threshold_milliseconds is a required configuration setting");
        Self {
            threshold_milliseconds,
            ..Default::default()
        }
    }

    pub fn click_and_is_double_click(&mut self, path: &PathInfo) -> bool {
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

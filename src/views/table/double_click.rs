use std::time::{Duration, Instant};

use crate::file_system::path_info::PathInfo;

pub(super) struct DoubleClick {
    last_path: Option<PathInfo>,
    start: Option<Instant>,
    threshold: Duration,
}

impl DoubleClick {
    pub(super) fn new(interval_milliseconds: u16) -> Self {
        Self {
            last_path: None,
            start: None,
            threshold: Duration::from_millis(u64::from(interval_milliseconds)),
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn path(name: &str) -> PathInfo {
        let mut info = PathInfo::try_from(Path::new("/")).unwrap();
        info.display_name = name.to_string();
        info.path = Path::new("/").join(name);
        info
    }

    /// Two calls in a row are microseconds apart, so every case here is inside
    /// the threshold whatever it is configured to. The elapsed-time branch
    /// needs a real wait and is not covered.
    fn clicker() -> DoubleClick {
        DoubleClick::new(500)
    }

    #[test]
    fn a_second_click_on_the_same_entry_is_a_double_click() {
        let mut clicks = clicker();
        assert!(!clicks.click_and_is_double_click(&path("a")));
        assert!(clicks.click_and_is_double_click(&path("a")));
    }

    #[test]
    fn a_click_on_another_entry_starts_over() {
        let mut clicks = clicker();
        clicks.click_and_is_double_click(&path("a"));

        // Clicking away and back is two first clicks, not a double click on
        // whichever entry the cursor happens to land on.
        assert!(!clicks.click_and_is_double_click(&path("b")));
        assert!(!clicks.click_and_is_double_click(&path("a")));
    }

    #[test]
    fn a_third_click_does_not_fire_a_second_time() {
        let mut clicks = clicker();
        clicks.click_and_is_double_click(&path("a"));
        assert!(clicks.click_and_is_double_click(&path("a")));

        // A double click opens the entry, so leaving the state armed would
        // open it again on the next click.
        assert!(!clicks.click_and_is_double_click(&path("a")));
        assert!(clicks.click_and_is_double_click(&path("a")));
    }
}

use std::{
    hash::{Hash, Hasher},
    sync::atomic::{AtomicUsize, Ordering},
};

// Progress (x,y) means "x progress" of "y total"
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Progress(pub u64, pub u64);

impl Progress {
    pub fn percentage(&self) -> u32 {
        if self.is_done() {
            return 100;
        }
        ((self.0 as f64 / self.1 as f64) * 100.0).round() as u32
    }

    pub fn scaled(&self, factor: u16) -> u16 {
        if self.is_done() {
            return factor;
        }
        (self.0 * factor as u64 / self.1) as u16
    }

    fn combine(&self, progress: &Progress) -> Self {
        Progress(self.0 + progress.0, self.1 + progress.1)
    }

    fn done(&mut self) {
        self.0 = self.1;
    }

    fn is_done(&self) -> bool {
        // `Progress(n, 0)` is considered done
        self.1 == 0 || self.0 == self.1
    }

    fn increment(&mut self, additional: u64) {
        self.0 += additional;
    }
}

#[derive(Clone, Debug, Eq)]
pub struct Task {
    id: Id,
    progress: Progress,
    status: TaskStatus,
}

impl PartialEq for Task {
    fn eq(&self, other: &Task) -> bool {
        self.id == other.id
    }
}

impl Hash for Task {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl Task {
    pub fn new(total: u64) -> Self {
        Self {
            id: next_id(),
            progress: Progress(0, total),
            status: TaskStatus::default(),
        }
    }

    pub fn combine_progress(&self, progress: &Progress) -> Progress {
        self.progress.combine(progress)
    }

    pub fn done(&mut self) {
        // Calling .increment() may also set the status to done.
        self.progress.done();
        self.status = TaskStatus::Done;
    }

    pub fn error(&mut self, message: String) {
        assert!(!self.is_done_or_error());

        self.status = TaskStatus::Error(message)
    }

    pub fn error_message(&self) -> Option<String> {
        match &self.status {
            TaskStatus::Error(message) => Some(message.clone()),
            _ => None,
        }
    }

    pub fn increment(&mut self, additional: u64) {
        assert!(!self.is_done_or_error());

        self.progress.increment(additional);
        self.status = if self.progress.is_done() {
            TaskStatus::Done
        } else {
            TaskStatus::InProgress
        };
    }

    pub fn is_done_or_error(&self) -> bool {
        matches!(self.status, TaskStatus::Done | TaskStatus::Error(_))
    }

    pub fn is_new(&self) -> bool {
        self.status == TaskStatus::New
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Id(usize);

fn next_id() -> Id {
    // Ref. https://users.rust-lang.org/t/idiomatic-rust-way-to-generate-unique-id/33805/6
    static COUNTER: AtomicUsize = AtomicUsize::new(1);
    Id(COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
enum TaskStatus {
    Done,
    Error(String),
    InProgress,
    #[default]
    New,
}

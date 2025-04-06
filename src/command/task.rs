use std::{
    hash::{Hash, Hasher},
    sync::atomic::{AtomicUsize, Ordering},
};

// Progress (x,y) means "x progress" of "y total"
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Progress(pub u64, pub u64);

impl Progress {
    pub fn done(&mut self) {
        self.0 = self.1;
    }

    pub fn is_done(&self) -> bool {
        // `Progress(n, 0)` is considered done
        self.1 == 0 || self.0 == self.1
    }

    pub fn scaled(&self, factor: u16) -> u16 {
        if self.is_done() {
            return factor;
        }
        let result = self.0 * factor as u64 / self.1;
        result as u16
    }

    fn combine(&self, progress: &Progress) -> Self {
        Progress(self.0 + progress.0, self.1 + progress.1)
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
        self.progress.done();
        self.status = TaskStatus::Done;
    }

    pub fn error(&mut self, message: String) {
        self.status = TaskStatus::Error(message)
    }

    pub fn error_message(&self) -> Option<String> {
        if let TaskStatus::Error(message) = &self.status {
            Some(message.clone())
        } else {
            None
        }
    }

    pub fn increment(&mut self, additional: u64) {
        self.progress.increment(additional);
        self.status = if self.progress.is_done() {
            TaskStatus::Done
        } else {
            TaskStatus::InProgress
        };
    }

    pub fn is_done(&self) -> bool {
        self.status == TaskStatus::Done
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

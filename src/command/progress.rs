use std::{
    hash::{Hash, Hasher},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::Sender,
    },
};

use super::Command;

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

/// A handle to an in-progress task.
///
/// Finalization methods (`done`, `error`) consume `self`, making it a compile-time
/// error to finalize a task more than once or to report an error after it has already
/// been marked done. This prevents the class of bugs where an async operation attempts
/// to update a task that has already been removed from the UI.
pub struct ActiveTask {
    task: Task,
    tx: Sender<Command>,
}

impl ActiveTask {
    /// Creates a new active task and an initial snapshot suitable for `Command::Progress`.
    pub fn new(total: u64, tx: Sender<Command>) -> (Self, Task) {
        let task = Task::new(total);
        let initial = task.clone();
        (Self { task, tx }, initial)
    }

    pub fn total_size(&self) -> u64 {
        self.task.progress.1
    }

    pub fn increment(&mut self, additional: u64) {
        self.task.increment(additional);
    }

    pub fn send_progress(&self) {
        // Err means the receiver was dropped (app is shutting down); silently ignore.
        let _ = self.tx.send(Command::Progress(self.task.clone()));
    }

    /// Marks the task as successfully completed. Consumes `self`.
    pub fn done(mut self) {
        self.task.done();
        // Err means the receiver was dropped (app is shutting down); silently ignore.
        let _ = self.tx.send(Command::Progress(self.task));
    }

    /// Marks the task as failed with an error message. Consumes `self`.
    pub fn error(mut self, message: String) {
        self.task.error(message);
        // Err means the receiver was dropped (app is shutting down); silently ignore.
        let _ = self.tx.send(Command::Progress(self.task));
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
    fn new(total: u64) -> Self {
        Self {
            id: next_id(),
            progress: Progress(0, total),
            status: TaskStatus::default(),
        }
    }

    pub fn combine_progress(&self, progress: &Progress) -> Progress {
        self.progress.combine(progress)
    }

    fn done(&mut self) {
        // Calling .increment() may also set the status to done.
        self.progress.done();
        self.status = TaskStatus::Done;
    }

    fn error(&mut self, message: String) {
        self.status = TaskStatus::Error(message)
    }

    pub fn error_message(&self) -> Option<String> {
        match &self.status {
            TaskStatus::Error(message) => Some(message.clone()),
            _ => None,
        }
    }

    fn increment(&mut self, additional: u64) {
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

use std::{
    hash::{Hash, Hasher},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::Sender,
    },
};

use super::Command;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Progress {
    pub completed: u64,
    pub total: u64,
}

impl Progress {
    pub fn percentage(&self) -> u32 {
        if self.is_done() {
            return 100;
        }
        ((self.completed as f64 / self.total as f64) * 100.0).round() as u32
    }

    pub fn scaled(&self, factor: u16) -> u16 {
        if self.is_done() {
            return factor;
        }
        ((self.completed as f64 / self.total as f64 * factor as f64).round() as u16).min(factor)
    }

    fn done(&mut self) {
        self.completed = self.total;
    }

    fn is_done(&self) -> bool {
        // `Progress { total: 0, .. }` is considered done
        self.total == 0 || self.completed == self.total
    }

    fn increment(&mut self, additional: u64) {
        self.completed = (self.completed + additional).min(self.total);
    }
}

/// A handle to an in-progress task.
///
/// Finalization methods (`done`, `error`) consume `self`, making it a compile-time
/// error to finalize a task more than once or to report an error after it has already
/// been marked done. This prevents the class of bugs where an async operation attempts
/// to update a task that has already been removed from the UI.
///
/// If dropped without calling `done` or `error` (e.g. via an early `?` return), the
/// `Drop` impl sends a final progress update marking the task as done, clearing any
/// phantom progress bar from the UI.
pub struct ActiveTask {
    task: Option<Task>,
    tx: Sender<Command>,
}

impl Drop for ActiveTask {
    fn drop(&mut self) {
        if let Some(mut task) = self.task.take() {
            // Dropped without finalization — the operation exited early
            // Report as an error so the UI clears the progress bar and alerts the user.
            task.error("Task interrupted");
            let _ = self.tx.send(Command::Progress(task));
        }
    }
}

impl ActiveTask {
    /// Creates a new active task and an initial snapshot suitable for `Command::Progress`.
    pub fn new(total: u64, tx: Sender<Command>) -> (Self, Task) {
        let task = Task::new(total);
        let initial = task.clone();
        (
            Self {
                task: Some(task),
                tx,
            },
            initial,
        )
    }

    pub fn total_size(&self) -> u64 {
        self.task.as_ref().map_or(0, |t| t.progress.total)
    }

    pub fn increment(&mut self, additional: u64) {
        if let Some(task) = &mut self.task {
            task.increment(additional);
        }
    }

    pub fn send_progress(&self) {
        // Err means the receiver was dropped (app is shutting down); silently ignore.
        if let Some(task) = &self.task {
            let _ = self.tx.send(Command::Progress(task.clone()));
        }
    }

    /// Marks the task as successfully completed. Consumes `self`.
    pub fn done(mut self) {
        if let Some(mut task) = self.task.take() {
            task.done();
            // Err means the receiver was dropped (app is shutting down); silently ignore.
            let _ = self.tx.send(Command::Progress(task));
        }
    }

    /// Marks the task as failed with an error message. Consumes `self`.
    pub fn error(mut self, message: String) {
        if let Some(mut task) = self.task.take() {
            task.error(message);
            // Err means the receiver was dropped (app is shutting down); silently ignore.
            let _ = self.tx.send(Command::Progress(task));
        }
    }
}

#[derive(Clone, Debug, Eq)]
pub struct Task {
    id: Id,
    progress: Progress,
    status: TaskStatus,
}

/// Identity-based equality: two `Task` values are the same task if they share the same `id`,
/// regardless of their current progress or status. This allows tasks to be looked up and
/// deduplicated by identity (e.g. in the notices `HashSet`) as progress snapshots arrive.
impl PartialEq for Task {
    fn eq(&self, other: &Task) -> bool {
        self.id == other.id
    }
}

/// Hashed by `id` only, consistent with the identity-based `PartialEq` above.
impl Hash for Task {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl Task {
    fn new(total: u64) -> Self {
        Self {
            id: next_id(),
            progress: Progress {
                completed: 0,
                total,
            },
            status: TaskStatus::default(),
        }
    }

    pub fn combine_progress(&self, progress: &Progress) -> Progress {
        Progress {
            completed: self.progress.completed + progress.completed,
            total: self.progress.total + progress.total,
        }
    }

    fn done(&mut self) {
        self.progress.done();
        self.status = TaskStatus::Done;
    }

    fn error(&mut self, message: impl Into<String>) {
        self.status = TaskStatus::Error(message.into())
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

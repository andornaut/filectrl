use std::{
    hash::{Hash, Hasher},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::Sender,
    },
};

use super::Command;

/// Describes what a task is doing, for display in the notices view and the
/// cancel alert.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TaskKind {
    Copy { source: String, destination: String },
    Move { source: String, destination: String },
    Delete { path: String },
}

impl TaskKind {
    /// The verb prefix, always shown in full by the operations notice (it is
    /// not truncated, only the `detail` is).
    pub fn prefix(&self) -> &'static str {
        match self {
            TaskKind::Copy { .. } => "Copying ",
            TaskKind::Move { .. } => "Moving ",
            TaskKind::Delete { .. } => "Deleting ",
        }
    }

    /// The source path, for operations that have one (copy/move). The
    /// operations notice truncates only this part to fit the width.
    pub fn source(&self) -> Option<&str> {
        match self {
            TaskKind::Copy { source, .. } | TaskKind::Move { source, .. } => Some(source),
            TaskKind::Delete { .. } => None,
        }
    }

    /// The source's basename (file/dir name), for copy/move.
    pub fn source_basename(&self) -> Option<&str> {
        self.source().map(basename)
    }

    /// The full destination path (including the basename) for copy/move.
    /// Used by the operations notice as a fallback when the source cannot be
    /// shown at all.
    pub fn destination(&self) -> Option<&str> {
        match self {
            TaskKind::Copy { destination, .. } | TaskKind::Move { destination, .. } => {
                Some(destination)
            }
            TaskKind::Delete { .. } => None,
        }
    }

    /// The target path shown in full by the operations notice: the
    /// destination directory for copy/move, or the path being deleted.
    pub fn target(&self) -> String {
        match self {
            TaskKind::Copy {
                source,
                destination,
            }
            | TaskKind::Move {
                source,
                destination,
            } => dest_display(source, destination),
            TaskKind::Delete { path } => path.clone(),
        }
    }

    /// The path portion (source + target).
    pub fn detail(&self) -> String {
        match self.source() {
            Some(source) => format!("{source} to {}", self.target()),
            None => self.target(),
        }
    }

    /// The shared human phrasing used by both the operations notice and the
    /// cancel alert. Callers add their own decoration (e.g. a trailing
    /// ellipsis or a `Cancelled:` prefix).
    pub fn message(&self) -> String {
        format!("{}{}", self.prefix(), self.detail())
    }
}

fn basename(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
}

/// If the source and destination share a basename (the common "into a
/// directory" case), show the destination's parent directory (with a
/// trailing slash to denote a directory) instead of repeating the filename;
/// otherwise show the full destination path.
fn dest_display(source: &str, destination: &str) -> String {
    if basename(source) == basename(destination) {
        let parent = Path::new(destination)
            .parent()
            .and_then(|p| p.to_str())
            .filter(|p| !p.is_empty())
            .unwrap_or(destination);
        if parent.ends_with('/') {
            parent.to_string()
        } else {
            format!("{parent}/")
        }
    } else {
        destination.to_string()
    }
}

#[derive(Clone, Debug)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

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
/// Finalization methods (`done`, `cancelled`, `error`) consume `self`, making it a compile-time
/// error to finalize a task more than once or to report an error after it has already
/// been marked done. This prevents the class of bugs where an async operation attempts
/// to update a task that has already been removed from the UI.
///
/// If dropped without calling `done` or `error` (e.g. via an early `?` return), the
/// `Drop` impl sends a final progress update marking the task as done, clearing any
/// phantom progress bar from the UI.
pub struct ActiveTask {
    cancel_token: CancellationToken,
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
    /// Returns the active task handle, an initial task snapshot, and a cancellation token.
    pub fn new(tx: Sender<Command>, kind: TaskKind, total: u64) -> (Self, Task, CancellationToken) {
        let cancel_token = CancellationToken::new();
        let task = Task::new(kind, total);
        let initial = task.clone();
        (
            Self {
                cancel_token: cancel_token.clone(),
                task: Some(task),
                tx,
            },
            initial,
            cancel_token,
        )
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
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

    /// Marks the task as cancelled by the user. Consumes `self`.
    pub fn cancelled(mut self) {
        if let Some(mut task) = self.task.take() {
            task.cancelled();
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
    kind: TaskKind,
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
    fn new(kind: TaskKind, total: u64) -> Self {
        Self {
            id: next_id(),
            kind,
            progress: Progress {
                completed: 0,
                total,
            },
            status: TaskStatus::default(),
        }
    }

    pub fn id(&self) -> usize {
        self.id.0
    }

    pub fn kind(&self) -> &TaskKind {
        &self.kind
    }

    pub fn combine_progress(&self, progress: &Progress) -> Progress {
        Progress {
            completed: self.progress.completed + progress.completed,
            total: self.progress.total + progress.total,
        }
    }

    fn cancelled(&mut self) {
        self.status = TaskStatus::Cancelled;
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

    pub fn is_cancelled(&self) -> bool {
        matches!(self.status, TaskStatus::Cancelled)
    }

    /// True once the task has reached a terminal state (done, errored, or
    /// cancelled) and should be removed from the notices view.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            TaskStatus::Cancelled | TaskStatus::Done | TaskStatus::Error(_)
        )
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
    Cancelled,
    Done,
    Error(String),
    InProgress,
    #[default]
    New,
}

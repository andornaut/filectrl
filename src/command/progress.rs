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

    /// Sets the task's total size once it becomes known (e.g. after scanning a
    /// directory) and sends a progress update so the notice reflects it.
    pub fn set_total(&mut self, total: u64) {
        if let Some(task) = &mut self.task {
            task.progress.total = total;
        }
        self.send_progress();
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn progress(completed: u64, total: u64) -> Progress {
        Progress { completed, total }
    }

    #[test]
    fn progress_percentage() {
        assert_eq!(100, progress(0, 0).percentage()); // total == 0 is "done"
        assert_eq!(100, progress(50, 0).percentage());
        assert_eq!(0, progress(0, 100).percentage());
        assert_eq!(50, progress(50, 100).percentage());
        assert_eq!(33, progress(1, 3).percentage()); // 33.33 rounds down
        assert_eq!(67, progress(2, 3).percentage()); // 66.67 rounds up
        assert_eq!(100, progress(100, 100).percentage());
    }

    #[test]
    fn progress_scaled() {
        assert_eq!(10, progress(0, 0).scaled(10)); // done -> full factor
        assert_eq!(10, progress(100, 100).scaled(10)); // done -> full factor
        assert_eq!(0, progress(0, 100).scaled(10));
        assert_eq!(5, progress(50, 100).scaled(10));
        assert_eq!(3, progress(1, 3).scaled(10)); // 3.33 rounds down
        assert_eq!(10, progress(200, 100).scaled(10)); // clamped to factor
    }

    #[test]
    fn progress_increment_clamps_at_total() {
        let mut p = progress(0, 100);
        p.increment(40);
        assert_eq!(progress(40, 100), p);
        assert!(!p.is_done());
        p.increment(1_000);
        assert_eq!(progress(100, 100), p);
        assert!(p.is_done());
    }

    #[test]
    fn progress_is_done() {
        assert!(progress(0, 0).is_done()); // zero total is done
        assert!(progress(100, 100).is_done());
        assert!(!progress(0, 100).is_done());
        assert!(!progress(99, 100).is_done());
    }

    fn copy(source: &str, destination: &str) -> TaskKind {
        TaskKind::Copy {
            source: source.to_string(),
            destination: destination.to_string(),
        }
    }

    #[test]
    fn task_kind_prefix() {
        assert_eq!("Copying ", copy("a", "b").prefix());
        assert_eq!(
            "Moving ",
            TaskKind::Move {
                source: "a".into(),
                destination: "b".into()
            }
            .prefix()
        );
        assert_eq!("Deleting ", TaskKind::Delete { path: "a".into() }.prefix());
    }

    #[test]
    fn task_kind_source_and_basename() {
        let k = copy("/a/b/file.txt", "/c/d/file.txt");
        assert_eq!(Some("/a/b/file.txt"), k.source());
        assert_eq!(Some("file.txt"), k.source_basename());

        let del = TaskKind::Delete {
            path: "/x/y".into(),
        };
        assert_eq!(None, del.source());
        assert_eq!(None, del.source_basename());
    }

    #[test]
    fn task_kind_copy_into_directory_shows_parent() {
        // Same basename: show the destination's parent directory with a trailing slash.
        let k = copy("/a/b/file.txt", "/c/d/file.txt");
        assert_eq!("/c/d/", k.target());
        assert_eq!("/a/b/file.txt to /c/d/", k.detail());
        assert_eq!("Copying /a/b/file.txt to /c/d/", k.message());
    }

    #[test]
    fn task_kind_copy_with_rename_shows_full_destination() {
        // Different basename: show the full destination path.
        let k = copy("/a/b/old.txt", "/c/d/new.txt");
        assert_eq!("/c/d/new.txt", k.target());
        assert_eq!("/a/b/old.txt to /c/d/new.txt", k.detail());
    }

    #[test]
    fn task_kind_dest_display_edge_parents() {
        // Destination at filesystem root: parent is "/".
        assert_eq!("/", copy("z/file.txt", "/file.txt").target());
        // Destination has no directory component: empty parent falls back to the
        // destination itself, with a trailing slash appended.
        assert_eq!("file.txt/", copy("z/file.txt", "file.txt").target());
    }

    #[test]
    fn task_kind_delete_detail_and_message() {
        let k = TaskKind::Delete {
            path: "/x/y".into(),
        };
        assert_eq!("/x/y", k.target());
        assert_eq!("/x/y", k.detail());
        assert_eq!("Deleting /x/y", k.message());
    }

    fn delete_task() -> Task {
        Task::new(TaskKind::Delete { path: "/x".into() }, 100)
    }

    #[test]
    fn task_starts_new_with_unique_id() {
        let a = delete_task();
        let b = delete_task();
        assert!(a.is_new());
        assert!(!a.is_terminal());
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn task_identity_equality_and_hash() {
        let mut a = delete_task();
        let snapshot = a.clone();
        a.increment(50); // progress/status diverge from the snapshot
        // Identity-based equality: still the same task.
        assert_eq!(a, snapshot);

        let mut set = HashSet::new();
        set.insert(snapshot);
        // Re-inserting a later snapshot of the same task does not grow the set.
        assert!(!set.insert(a));
        assert_eq!(1, set.len());

        // A different task is distinct.
        assert!(set.insert(delete_task()));
        assert_eq!(2, set.len());
    }

    #[test]
    fn task_increment_transitions_status() {
        let mut t = delete_task();
        t.increment(40);
        assert!(!t.is_new());
        assert!(!t.is_terminal());
        t.increment(60);
        assert!(t.is_terminal()); // reached total -> Done
    }

    #[test]
    fn task_done_marks_progress_complete_and_terminal() {
        let mut t = delete_task();
        t.done();
        assert!(t.is_terminal());
        assert_eq!(100, t.progress.percentage());
    }

    #[test]
    fn task_cancelled_is_terminal() {
        let mut t = delete_task();
        t.cancelled();
        assert!(t.is_cancelled());
        assert!(t.is_terminal());
    }

    #[test]
    fn task_error_records_message_and_is_terminal() {
        let mut t = delete_task();
        t.error("disk full");
        assert_eq!(Some("disk full".to_string()), t.error_message());
        assert!(t.is_terminal());
        assert!(!t.is_cancelled());
    }

    #[test]
    fn task_combine_progress_sums_fields() {
        let t = Task::new(TaskKind::Delete { path: "/x".into() }, 100);
        let combined = t.combine_progress(&progress(10, 50));
        assert_eq!(progress(10, 150), combined);
    }

    fn recv_task(rx: &std::sync::mpsc::Receiver<Command>) -> Task {
        match rx.recv().expect("a Progress command should have been sent") {
            Command::Progress(task) => task,
            other => panic!("expected Command::Progress, got {other:?}"),
        }
    }

    #[test]
    fn active_task_drop_without_finalize_reports_error() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (active, _initial, _token) =
            ActiveTask::new(tx, TaskKind::Delete { path: "/x".into() }, 100);
        drop(active);

        let task = recv_task(&rx);
        assert_eq!(Some("Task interrupted".to_string()), task.error_message());
        assert!(task.is_terminal());
        // Drop sends exactly one message.
        assert!(rx.recv().is_err());
    }

    #[test]
    fn active_task_done_sends_done_and_drop_is_noop() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (active, _initial, _token) =
            ActiveTask::new(tx, TaskKind::Delete { path: "/x".into() }, 100);
        active.done();

        let task = recv_task(&rx);
        assert!(task.is_terminal());
        assert_eq!(None, task.error_message());
        assert_eq!(100, task.progress.percentage());
        // Finalization consumed the task, so Drop must not send a second update.
        assert!(rx.recv().is_err());
    }

    #[test]
    fn active_task_cancelled_sends_cancelled_status() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (active, _initial, _token) =
            ActiveTask::new(tx, TaskKind::Delete { path: "/x".into() }, 100);
        active.cancelled();

        let task = recv_task(&rx);
        assert!(task.is_cancelled());
        assert!(task.is_terminal());
        assert!(rx.recv().is_err());
    }

    #[test]
    fn active_task_error_sends_error_message() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (active, _initial, _token) =
            ActiveTask::new(tx, TaskKind::Delete { path: "/x".into() }, 100);
        active.error("disk full".to_string());

        let task = recv_task(&rx);
        assert_eq!(Some("disk full".to_string()), task.error_message());
        assert!(task.is_terminal());
        assert!(!task.is_cancelled());
        assert!(rx.recv().is_err());
    }

    #[test]
    fn active_task_send_progress_then_done_emits_two_updates() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (mut active, _initial, _token) =
            ActiveTask::new(tx, TaskKind::Delete { path: "/x".into() }, 100);
        active.increment(40);
        active.send_progress();

        let mid = recv_task(&rx);
        assert!(!mid.is_terminal());
        assert_eq!(40, mid.progress.percentage());

        active.done();
        let done = recv_task(&rx);
        assert!(done.is_terminal());
        assert!(rx.recv().is_err());
    }
}

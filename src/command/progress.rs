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

/// Shared fields for copy and move operations.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Transfer {
    pub source: String,
    pub destination: String,
}

/// What a task is doing, for the notices view and the cancel alert.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TaskKind {
    Copy(Transfer),
    Move(Transfer),
    Delete { path: String },
}

impl TaskKind {
    /// The verb prefix, never truncated.
    pub fn prefix(&self) -> &'static str {
        match self {
            TaskKind::Copy(_) => "Copying ",
            TaskKind::Move(_) => "Moving ",
            TaskKind::Delete { .. } => "Deleting ",
        }
    }

    fn transfer(&self) -> Option<&Transfer> {
        match self {
            TaskKind::Copy(t) | TaskKind::Move(t) => Some(t),
            TaskKind::Delete { .. } => None,
        }
    }

    /// The source path of a copy or move.
    pub fn source(&self) -> Option<&str> {
        self.transfer().map(|t| t.source.as_str())
    }

    pub fn source_basename(&self) -> Option<&str> {
        self.source().map(basename)
    }

    /// The full destination path of a copy or move.
    pub fn destination(&self) -> Option<&str> {
        self.transfer().map(|t| t.destination.as_str())
    }

    /// The destination directory of a copy or move, or the path being deleted.
    pub fn target(&self) -> String {
        match self {
            TaskKind::Copy(t) | TaskKind::Move(t) => dest_display(&t.source, &t.destination),
            TaskKind::Delete { path } => path.clone(),
        }
    }

    pub fn detail(&self) -> String {
        match self.source() {
            Some(source) => format!("{source} to {}", self.target()),
            None => self.target(),
        }
    }

    /// The phrasing shared by the operations notice and the cancel alert.
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

/// The destination's parent with a trailing slash when the basenames match,
/// otherwise the full destination.
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

    /// Whether both handles control the same task.
    #[cfg(test)]
    pub fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
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
    // f64 for half-away-from-zero rounding; `as` saturates. An unfinished task
    // stops one short of `factor`, even past a lowered total.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    #[allow(clippy::cast_sign_loss)]
    pub fn scaled(&self, factor: u16) -> u16 {
        if self.is_done() {
            return factor;
        }
        if self.total == 0 {
            return 0;
        }
        ((self.completed as f64 / self.total as f64 * f64::from(factor)).round() as u16)
            .min(factor.saturating_sub(1))
    }

    pub fn percentage(&self) -> u32 {
        u32::from(self.scaled(100))
    }

    fn done(&mut self) {
        self.completed = self.total;
    }

    /// A zero total means nothing sized to count, not finished.
    fn is_done(&self) -> bool {
        self.total != 0 && self.completed == self.total
    }

    fn increment(&mut self, additional: u64) {
        self.completed = (self.completed + additional).min(self.total);
    }
}

/// A handle to an in-progress task. Finalization consumes `self`; dropped
/// without it, the task reports "Task interrupted".
pub struct ActiveTask {
    cancel_token: CancellationToken,
    /// Set once the task ended or entered a stage that cannot be interrupted.
    /// Shared with the cancel stack.
    uncancellable: Arc<AtomicBool>,
    task: Option<Task>,
    tx: Sender<Command>,
}

impl Drop for ActiveTask {
    fn drop(&mut self) {
        self.finalize(|task| task.error("Task interrupted"));
    }
}

impl ActiveTask {
    /// Returns the handle, an initial snapshot for `Command::Progress`, and a
    /// cancellation token.
    pub fn new(tx: Sender<Command>, kind: TaskKind, total: u64) -> (Self, Task, CancellationToken) {
        let cancel_token = CancellationToken::new();
        let task = Task::new(kind, total);
        let initial = task.clone();
        (
            Self {
                cancel_token: cancel_token.clone(),
                uncancellable: Arc::new(AtomicBool::new(false)),
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

    /// A shared flag set when the task can no longer be cancelled.
    pub fn uncancellable_handle(&self) -> Arc<AtomicBool> {
        self.uncancellable.clone()
    }

    /// Marks the task as no longer cancellable without finalizing it.
    pub fn set_uncancellable(&self) {
        self.uncancellable.store(true, Ordering::Relaxed);
    }

    /// Applies the terminal transition and sends the final snapshot, once.
    fn finalize(&mut self, transition: impl FnOnce(&mut Task)) {
        if let Some(mut task) = self.task.take() {
            self.uncancellable.store(true, Ordering::Relaxed);
            transition(&mut task);
            let _ = self.tx.send(Command::Progress(task));
        }
    }

    pub fn total_size(&self) -> u64 {
        self.task.as_ref().map_or(0, |t| t.progress.total)
    }

    /// Sets the total once known and sends a progress update.
    pub fn set_total(&mut self, total: u64) {
        if let Some(task) = &mut self.task {
            task.progress.total = total;
            // A `New` snapshot would re-add a task cleared from the notices view.
            task.start();
        }
        self.send_progress();
    }

    pub fn increment(&mut self, additional: u64) {
        if let Some(task) = &mut self.task {
            task.increment(additional);
        }
    }

    pub fn send_progress(&self) {
        if let Some(task) = &self.task {
            let _ = self.tx.send(Command::Progress(task.clone()));
        }
    }

    pub fn done(mut self) {
        self.finalize(Task::done);
    }

    pub fn cancelled(mut self) {
        self.finalize(Task::cancelled);
    }

    pub fn error(mut self, message: String) {
        self.finalize(|task| task.error(message));
    }
}

#[derive(Clone, Debug, Eq)]
pub struct Task {
    id: Id,
    kind: TaskKind,
    progress: Progress,
    status: TaskStatus,
}

/// Equal by `id`, whatever the progress or status.
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

    pub fn progress(&self) -> &Progress {
        &self.progress
    }

    fn cancelled(&mut self) {
        self.status = TaskStatus::Cancelled;
    }

    fn done(&mut self) {
        self.progress.done();
        self.status = TaskStatus::Done;
    }

    fn error(&mut self, message: impl Into<String>) {
        self.status = TaskStatus::Error(message.into());
    }

    fn start(&mut self) {
        self.status = TaskStatus::InProgress;
    }

    pub fn error_message(&self) -> Option<String> {
        match &self.status {
            TaskStatus::Error(message) => Some(message.clone()),
            _ => None,
        }
    }

    /// Advances byte progress. Never terminal: the total can be stale or work
    /// can remain.
    fn increment(&mut self, additional: u64) {
        self.progress.increment(additional);
        self.status = TaskStatus::InProgress;
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self.status, TaskStatus::Cancelled)
    }

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

    use test_case::test_case;

    use super::*;

    fn progress(completed: u64, total: u64) -> Progress {
        Progress { completed, total }
    }

    #[test]
    fn a_percentage_rounds_and_counts_a_zero_total_as_not_started() {
        assert_eq!(0, progress(0, 0).percentage());
        assert_eq!(0, progress(50, 0).percentage());
        assert_eq!(0, progress(0, 100).percentage());
        assert_eq!(50, progress(50, 100).percentage());
        assert_eq!(33, progress(1, 3).percentage());
        assert_eq!(67, progress(2, 3).percentage()); // 66.67 rounds up
        assert_eq!(99, progress(995, 1000).percentage()); // unfinished stays below 100
        assert_eq!(100, progress(100, 100).percentage());
    }

    #[test]
    fn a_scaled_position_clamps_to_the_factor_and_is_full_when_done() {
        assert_eq!(0, progress(0, 0).scaled(10));
        assert_eq!(10, progress(100, 100).scaled(10));
        assert_eq!(0, progress(0, 100).scaled(10));
        assert_eq!(5, progress(50, 100).scaled(10));
        assert_eq!(3, progress(1, 3).scaled(10));
        assert_eq!(9, progress(99, 100).scaled(10));
        assert_eq!(9, progress(200, 100).scaled(10)); // past a lowered total, still unfinished
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

    fn copy(source: &str, destination: &str) -> TaskKind {
        TaskKind::Copy(Transfer {
            source: source.to_string(),
            destination: destination.to_string(),
        })
    }

    fn r#move(source: &str, destination: &str) -> TaskKind {
        TaskKind::Move(Transfer {
            source: source.to_string(),
            destination: destination.to_string(),
        })
    }

    #[test]
    fn each_task_kind_names_its_own_verb() {
        assert_eq!("Copying ", copy("a", "b").prefix());
        assert_eq!("Moving ", r#move("a", "b").prefix());
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

    /// An empty `destination` builds a delete.
    #[test_case("/a/b/file.txt", "/c/d/file.txt", "/c/d/" ; "copy into a directory")]
    #[test_case("/a/b/old.txt", "/c/d/new.txt", "/c/d/new.txt" ; "copy with a rename")]
    #[test_case("z/file.txt", "/file.txt", "/" ; "destination at the filesystem root")]
    #[test_case("z/file.txt", "file.txt", "file.txt/" ; "destination with no directory")]
    #[test_case("/x/y", "", "/x/y" ; "delete")]
    fn task_kind_renders(source: &str, destination: &str, target: &str) {
        let kind = if destination.is_empty() {
            TaskKind::Delete {
                path: source.to_string(),
            }
        } else {
            copy(source, destination)
        };

        assert_eq!(target, kind.target());
        let detail = match kind.source() {
            Some(source) => format!("{source} to {target}"),
            None => target.to_string(),
        };
        assert_eq!(detail, kind.detail());
        assert_eq!(format!("{}{detail}", kind.prefix()), kind.message());
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
        a.increment(50);
        assert_eq!(a, snapshot);

        let mut set = HashSet::new();
        set.insert(snapshot);
        assert!(!set.insert(a));
        assert_eq!(1, set.len());

        assert!(set.insert(delete_task()));
        assert_eq!(2, set.len());
    }

    #[test]
    fn task_increment_to_total_stays_in_progress_until_done() {
        let mut t = delete_task();
        t.increment(40);
        assert!(!t.is_new());
        assert!(!t.is_terminal());
        t.increment(60);
        assert!(!t.is_terminal());
        assert_eq!(100, t.progress.percentage());
        assert_eq!(10, t.progress.scaled(10));
        t.done();
        assert!(t.is_terminal());
    }

    fn recv_task(rx: &std::sync::mpsc::Receiver<Command>) -> Task {
        match rx.recv().expect("a Progress command should have been sent") {
            Command::Progress(task) => task,
            other => panic!("expected Command::Progress, got {other:?}"),
        }
    }

    fn active_task() -> (ActiveTask, std::sync::mpsc::Receiver<Command>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let (active, _initial, _token) =
            ActiveTask::new(tx, TaskKind::Delete { path: "/x".into() }, 100);
        (active, rx)
    }

    /// The terminal snapshot, asserting that finalizing sent exactly one.
    fn only_terminal_task(rx: &std::sync::mpsc::Receiver<Command>) -> Task {
        let task = recv_task(rx);
        assert!(task.is_terminal());
        assert!(rx.recv().is_err(), "expected exactly one update");
        task
    }

    #[test]
    fn active_task_drop_without_finalize_reports_error() {
        let (active, rx) = active_task();
        drop(active);

        let task = only_terminal_task(&rx);
        assert_eq!(Some("Task interrupted".to_string()), task.error_message());
    }

    #[test]
    fn active_task_done_fills_the_progress_bar() {
        let (active, rx) = active_task();
        // Finished with nothing counted.
        active.done();

        let task = only_terminal_task(&rx);
        assert_eq!(None, task.error_message());
        assert_eq!(100, task.progress.percentage());
    }

    #[test]
    fn active_task_cancelled_sends_cancelled_status() {
        let (active, rx) = active_task();
        active.cancelled();

        assert!(only_terminal_task(&rx).is_cancelled());
    }

    #[test]
    fn active_task_error_sends_error_message() {
        let (active, rx) = active_task();
        active.error("disk full".to_string());

        let task = only_terminal_task(&rx);
        assert_eq!(Some("disk full".to_string()), task.error_message());
        assert!(!task.is_cancelled());
    }

    #[test]
    fn a_finalized_task_can_no_longer_be_cancelled() {
        let (active, _rx) = active_task();
        let uncancellable = active.uncancellable_handle();
        assert!(!uncancellable.load(Ordering::Relaxed));

        active.done();

        assert!(uncancellable.load(Ordering::Relaxed));
    }

    #[test]
    fn learning_the_total_stops_the_task_reporting_itself_as_new() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (mut active, _initial, _token) =
            ActiveTask::new(tx, TaskKind::Delete { path: "/x".into() }, 0);

        active.set_total(500);

        let update = recv_task(&rx);
        assert!(!update.is_new());
        assert_eq!(500, update.progress.total);
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

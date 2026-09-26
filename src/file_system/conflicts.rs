//! A paste's standing answer as its workers see it, and its refusal messages.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

use super::path_info::compact;
use crate::command::ConflictChoice;

/// A paste's standing `*All` answer, shared by the queue and the workers running
/// its sources. Only the queue decides to replace anything; a worker reads the
/// answer only to skip. A name taken after the queue saw it free is never
/// replaced or asked about: it is skipped under "skip all" and otherwise
/// recorded (`raced_refusal`).
#[derive(Clone, Debug, Default)]
pub(super) struct Conflicts {
    /// `SKIP_ALL`, `OVERWRITE_ALL`, or zero for no standing answer.
    standing: Arc<AtomicU8>,
}

const SKIP_ALL: u8 = 1;
const OVERWRITE_ALL: u8 = 2;

impl Conflicts {
    /// Records `choice` as the standing answer if it is an `*All`, including for
    /// workers already running. Any other answer leaves the standing one.
    pub(super) fn stand(&self, choice: ConflictChoice) {
        let standing = match choice {
            ConflictChoice::SkipAll => SKIP_ALL,
            ConflictChoice::OverwriteAll => OVERWRITE_ALL,
            ConflictChoice::Skip | ConflictChoice::Overwrite => return,
        };
        self.standing.store(standing, Ordering::SeqCst);
    }

    pub(super) fn standing(&self) -> Option<ConflictChoice> {
        match self.standing.load(Ordering::SeqCst) {
            SKIP_ALL => Some(ConflictChoice::SkipAll),
            OVERWRITE_ALL => Some(ConflictChoice::OverwriteAll),
            _ => None,
        }
    }

    /// Whether a worker skips a name taken after the queue saw it free: only under
    /// a standing "skip all".
    pub(super) fn skips_raced(&self) -> bool {
        self.standing() == Some(ConflictChoice::SkipAll)
    }
}

/// Refusal for a destination taken after the queue saw it free.
pub(super) fn raced_refusal(is_move: bool, source: &Path, destination: &Path) -> String {
    format!(
        "Cannot {} {} to {}: another entry took that name after the paste checked it",
        verb(is_move),
        compact(source),
        compact(destination)
    )
}

/// Refusal for an entry inside a copied tree whose destination name is taken,
/// by another program or by a name the destination folds onto another.
pub(super) fn raced_in_copy_refusal(is_move: bool, source: &Path, destination: &Path) -> String {
    format!(
        "Cannot {} {} to {}: the name is already taken there, by another entry or one the \
         destination treats as the same",
        verb(is_move),
        compact(source),
        compact(destination),
    )
}

/// Refusal for a destination holding another entry than the one to replace.
pub(super) fn changed_refusal(is_move: bool, source: &Path, destination: &Path) -> String {
    format!(
        "Cannot {} {} to {}: the entry there changed after the paste checked it",
        verb(is_move),
        compact(source),
        compact(destination)
    )
}

/// Refusal for a name an earlier source of the same paste took.
pub(super) fn same_name_refusal(is_move: bool, source: &Path, dest_dir: &Path) -> String {
    format!(
        "Cannot {} {} into {}: another source in this paste already takes that name there",
        verb(is_move),
        compact(source),
        compact(dest_dir)
    )
}

/// Refusal for a name holding an entry made since the paste began.
pub(super) fn made_since_refusal(is_move: bool, source: &Path, dest_dir: &Path) -> String {
    format!(
        "Cannot {} {} into {}: an entry made since this paste began holds that name",
        verb(is_move),
        compact(source),
        compact(dest_dir)
    )
}

/// Refusal for a destination that is another name of the same file.
pub(super) fn same_file_refusal(is_move: bool, source: &Path, destination: &Path) -> String {
    format!(
        "Cannot {} {} to {}: they are the same file",
        verb(is_move),
        compact(source),
        compact(destination)
    )
}

/// Suffix saying the entry at `destination` was not replaced.
pub(super) fn not_replaced(destination: &Path) -> String {
    format!("; {} was not replaced", compact(destination))
}

/// A copy or move failure reported by the system.
pub(super) fn failed_transfer(
    is_move: bool,
    source: &Path,
    destination: &Path,
    error: &dyn std::fmt::Display,
) -> String {
    format!(
        "Failed to {} {} to {}: {error}",
        verb(is_move),
        compact(source),
        compact(destination)
    )
}

/// `failed_transfer`, adding `not_replaced` when `kept`.
pub(super) fn failed_replacement(
    kept: bool,
    is_move: bool,
    source: &Path,
    destination: &Path,
    error: &dyn std::fmt::Display,
) -> String {
    let failed = failed_transfer(is_move, source, destination, error);
    if kept {
        failed + &not_replaced(destination)
    } else {
        failed
    }
}

/// The message for a failed rename of `source` onto `destination`, or `None` to
/// skip. `AlreadyExists` means a name raced (skipped under "skip all"); any other
/// error is a failure, noting the entry was kept when the rename `replaces` it.
pub(super) fn rename_failure(
    conflicts: &Conflicts,
    is_move: bool,
    replaces: bool,
    source: &Path,
    destination: &Path,
    error: &std::io::Error,
) -> Option<String> {
    if error.kind() != std::io::ErrorKind::AlreadyExists {
        Some(failed_replacement(
            replaces,
            is_move,
            source,
            destination,
            error,
        ))
    } else if conflicts.skips_raced() {
        None
    } else {
        Some(raced_refusal(is_move, source, destination))
    }
}

pub(super) fn verb(is_move: bool) -> &'static str {
    if is_move { "move" } else { "copy" }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test]
    fn nothing_skips_until_an_answer_stands() {
        assert!(!Conflicts::default().skips_raced());
    }

    #[test_case(&[] => None ; "nothing answered")]
    #[test_case(&[ConflictChoice::Skip, ConflictChoice::Overwrite] => None ; "single answers")]
    #[test_case(&[ConflictChoice::SkipAll, ConflictChoice::Overwrite] => Some(ConflictChoice::SkipAll) ; "skip all then overwrite")]
    #[test_case(&[ConflictChoice::OverwriteAll, ConflictChoice::Skip] => Some(ConflictChoice::OverwriteAll) ; "overwrite all then skip")]
    #[test_case(&[ConflictChoice::OverwriteAll, ConflictChoice::SkipAll] => Some(ConflictChoice::SkipAll) ; "a later all answer replaces an earlier one")]
    fn what_stands(answers: &[ConflictChoice]) -> Option<ConflictChoice> {
        let conflicts = Conflicts::default();
        for &choice in answers {
            conflicts.stand(choice);
        }
        conflicts.standing()
    }

    /// A worker holds a clone, and must see an answer given after it started.
    #[test]
    fn a_standing_answer_reaches_every_clone() {
        let conflicts = Conflicts::default();
        let worker = conflicts.clone();

        conflicts.stand(ConflictChoice::SkipAll);

        assert!(worker.skips_raced());
    }

    #[test_case(&[] => false ; "no standing answer")]
    #[test_case(&[ConflictChoice::SkipAll] => true ; "skip all")]
    #[test_case(&[ConflictChoice::OverwriteAll] => false ; "overwrite all")]
    #[test_case(&[ConflictChoice::SkipAll, ConflictChoice::OverwriteAll] => false ; "overwrite all after skip all")]
    fn what_skips_a_raced_name(answers: &[ConflictChoice]) -> bool {
        let conflicts = Conflicts::default();
        for &choice in answers {
            conflicts.stand(choice);
        }
        conflicts.skips_raced()
    }
}

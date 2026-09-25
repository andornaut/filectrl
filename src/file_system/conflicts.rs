//! What the workers of one paste need of its answers, and how its refusals
//! read.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

use super::path_info::compact;
use crate::command::ConflictChoice;

/// A paste's standing `*All` answer, shared between the queue that records
/// and applies it and the workers running its sources.
///
/// Only the queue decides to replace anything: an entry it found at a
/// source's destination, answered for with "overwrite" or covered by a
/// standing "overwrite all", and only while that entry still holds the name
/// when the source's work runs (`changed_refusal`). A worker reads the
/// standing answer only to skip. A name taken after the queue saw it free (by
/// another program, another paste, or a name the filesystem folds onto one
/// this paste wrote) is never replaced, since nobody decided to replace what
/// now holds it, and a worker never asks about it either: it runs long after
/// the queue handed out its source, about a state the user never saw. Under a
/// standing "skip all" such a name is skipped, and otherwise it is recorded
/// like any other entry that could not be written (`raced_refusal`).
#[derive(Clone, Debug, Default)]
pub(super) struct Conflicts {
    /// `SKIP_ALL`, `OVERWRITE_ALL`, or zero for no standing answer.
    standing: Arc<AtomicU8>,
}

const SKIP_ALL: u8 = 1;
const OVERWRITE_ALL: u8 = 2;

impl Conflicts {
    /// Records `choice` as the paste's standing answer when it is an `*All`,
    /// for the queue and for the workers, including those already handed a
    /// source whose name they have not reached yet. Any other answer is for
    /// one collision and leaves the standing one as it was.
    pub(super) fn stand(&self, choice: ConflictChoice) {
        let standing = match choice {
            ConflictChoice::SkipAll => SKIP_ALL,
            ConflictChoice::OverwriteAll => OVERWRITE_ALL,
            ConflictChoice::Skip | ConflictChoice::Overwrite => return,
        };
        self.standing.store(standing, Ordering::SeqCst);
    }

    /// The standing `*All` answer, which the queue applies to each collision
    /// it meets from then on.
    pub(super) fn standing(&self) -> Option<ConflictChoice> {
        match self.standing.load(Ordering::SeqCst) {
            SKIP_ALL => Some(ConflictChoice::SkipAll),
            OVERWRITE_ALL => Some(ConflictChoice::OverwriteAll),
            _ => None,
        }
    }

    /// Whether a worker skips a name taken at a destination after the queue
    /// saw it free, rather than recording it: only under a standing "skip
    /// all". Such a name is never replaced.
    pub(super) fn skips_raced(&self) -> bool {
        self.standing() == Some(ConflictChoice::SkipAll)
    }
}

/// The refusal of `source`, whose destination `destination` was taken after
/// the queue saw it free.
pub(super) fn raced_refusal(is_move: bool, source: &Path, destination: &Path) -> String {
    format!(
        "Cannot {} {} to {}: another entry took that name after the paste checked it",
        verb(is_move),
        compact(source),
        compact(destination)
    )
}

/// The refusal of `source`, an entry inside a directory the copy created,
/// whose destination `destination` is taken: by another program writing into
/// the tree, or by an entry of the same tree whose name the destination treats
/// as the same (`Notes` and `notes` on a case-insensitive filesystem).
pub(super) fn raced_in_copy_refusal(is_move: bool, source: &Path, destination: &Path) -> String {
    format!(
        "Cannot {} {} to {}: the name is already taken there, by another entry or one the \
         destination treats as the same",
        verb(is_move),
        compact(source),
        compact(destination),
    )
}

/// The refusal of `source`, whose destination `destination` holds another
/// entry than the one the queue was told to replace.
pub(super) fn changed_refusal(is_move: bool, source: &Path, destination: &Path) -> String {
    format!(
        "Cannot {} {} to {}: the entry there changed after the paste checked it",
        verb(is_move),
        compact(source),
        compact(destination)
    )
}

/// The refusal of `source`, whose name an earlier source of the same paste
/// took into `dest_dir`.
pub(super) fn same_name_refusal(is_move: bool, source: &Path, dest_dir: &Path) -> String {
    format!(
        "Cannot {} {} into {}: another source in this paste already takes that name there",
        verb(is_move),
        compact(source),
        compact(dest_dir)
    )
}

/// The refusal of `source`, whose name in `dest_dir` holds an entry made since
/// the paste began (`PendingPaste::meet`).
pub(super) fn made_since_refusal(is_move: bool, source: &Path, dest_dir: &Path) -> String {
    format!(
        "Cannot {} {} into {}: an entry made since this paste began holds that name",
        verb(is_move),
        compact(source),
        compact(dest_dir)
    )
}

/// The refusal of `source`, whose destination `destination` is another name of
/// the same file, which replacing would delete or leave as it was.
pub(super) fn same_file_refusal(is_move: bool, source: &Path, destination: &Path) -> String {
    format!(
        "Cannot {} {} to {}: they are the same file",
        verb(is_move),
        compact(source),
        compact(destination)
    )
}

/// What a failed replacement adds to its message: the entry it was to replace
/// is still at `destination`.
pub(super) fn not_replaced(destination: &Path) -> String {
    format!("; {} was not replaced", compact(destination))
}

/// The failure of a copy or move of `source` to `destination` that a call
/// outside filectrl reported as `error`.
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

/// `failed_transfer` for a replacement: when `kept`, the entry the paste was
/// to replace is still at `destination`, which the message then says
/// (`not_replaced`).
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

/// What a failed rename of `source` onto `destination` means, for a move and
/// for landing a staged replacement alike: a name taken since the queue saw
/// it free (`AlreadyExists`, which only a rename that replaces nothing can
/// find) is skipped under a standing "skip all" (`None`) and refused as raced
/// otherwise; any other failure is one, and says the entry granted was left
/// when the rename was the one that `replaces` it.
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

/// The verb a copy or a move names itself by in its messages.
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

    /// Only an `*All` stands; an answer for one collision leaves the standing
    /// one as it was.
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

    /// The answer reaches every clone: a worker holds one, and a copy already
    /// running has to see an answer given after it started.
    #[test]
    fn a_standing_answer_reaches_every_clone() {
        let conflicts = Conflicts::default();
        let worker = conflicts.clone();

        conflicts.stand(ConflictChoice::SkipAll);

        assert!(worker.skips_raced());
    }

    /// Only a standing "skip all" skips a raced name; "overwrite all" does
    /// not reach it, and replaces a "skip all" given before it.
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

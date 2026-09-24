//! The conflict decisions for one paste.

use std::{
    collections::HashSet,
    path::Path,
    sync::{Arc, Mutex},
};

use nix::sys::stat::{FileStat, lstat};

use super::entry_id::EntryId;
use crate::command::ConflictChoice;

/// The standing `*All` answer for one paste, shared between the thread running
/// its queue and the workers copying its sources, so a name another process
/// takes deep inside a tree is settled the same way as one at the top level.
///
/// Only a standing answer crosses that boundary: the queue writes it, workers
/// read it. A worker never asks. It runs long after the queue has handed out
/// its last source, on the one thread every operation is serialized onto, and
/// the collision it finds is a race against another program, about a state the
/// user never saw. Without a standing answer that covers it, the entry is
/// recorded like any other that could not be written and the walk carries on.
///
/// It also holds what the paste has put into its destination, by device and
/// inode, so that no worker replaces an entry an earlier source of the same
/// paste wrote. The queue refuses a second source of a name it has already
/// handed out, but it compares names byte for byte, and a filesystem that folds
/// case or normalizes Unicode gives two different names one entry.
/// `was_pasted_stat` is the one place that decides what such an entry is.
#[derive(Clone, Default)]
pub(super) struct Conflicts {
    apply_to_all: Arc<Mutex<Option<ConflictChoice>>>,
    pasted: Arc<Mutex<HashSet<EntryId>>>,
}

impl Conflicts {
    /// The standing `*All` answer, if one has been given.
    pub(super) fn standing(&self) -> Option<ConflictChoice> {
        *self.lock()
    }

    /// Records an answer from the user. An `*All` stands for the rest of the
    /// paste, including the parts already handed to a worker; anything else
    /// answers only the collision in front of the user.
    pub(super) fn answer(&self, choice: ConflictChoice) {
        if matches!(
            choice,
            ConflictChoice::OverwriteAll | ConflictChoice::SkipAll
        ) {
            *self.lock() = Some(choice);
        }
    }

    /// Records the entry `id` as one this paste wrote.
    pub(super) fn record_pasted(&self, id: EntryId) {
        self.pasted
            .lock()
            .expect("the pasted entries are not poisoned")
            .insert(id);
    }

    /// Whether the entry `path` names is one replacing would take from this
    /// paste (`was_pasted_stat`).
    pub(super) fn was_pasted(&self, path: &Path) -> bool {
        lstat(path).is_ok_and(|stat| self.was_pasted_stat(&stat))
    }

    /// Whether the entry `stat` describes is one this paste wrote, under its
    /// only name. An entry with another link survives a replacement under
    /// that one, so replacing a name of it loses nothing, and a hard link
    /// elsewhere to what a move brought in is not mistaken for it.
    pub(super) fn was_pasted_stat(&self, stat: &FileStat) -> bool {
        stat.st_nlink == 1
            && self
                .pasted
                .lock()
                .expect("the pasted entries are not poisoned")
                .contains(&EntryId::of_stat(stat))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<ConflictChoice>> {
        // Nothing blocks while the lock is held, so a panic while it is held
        // would have to come from the two lines above.
        self.apply_to_all
            .lock()
            .expect("the conflict state is not poisoned")
    }
}

/// Whether `path` names an entry the paste behind `conflicts` wrote, which no
/// worker of that paste may replace. `false` outside a paste.
pub(super) fn pasted_here(conflicts: Option<&Conflicts>, path: &Path) -> bool {
    conflicts.is_some_and(|conflicts| conflicts.was_pasted(path))
}

/// The refusal of a source whose destination name holds what an earlier source
/// of the same paste wrote, wherever the paste finds it.
pub(super) fn same_name_refusal(is_move: bool, source: &Path, dest_dir: &Path) -> String {
    let verb = if is_move { "move" } else { "copy" };
    format!(
        "Cannot {verb} {} into {}: another source in this paste has the same name",
        super::path_info::compact(source),
        super::path_info::compact(dest_dir)
    )
}

/// True when `choice` means the entry it answered should be replaced.
pub(super) fn replaces(choice: ConflictChoice) -> bool {
    matches!(
        choice,
        ConflictChoice::Overwrite | ConflictChoice::OverwriteAll
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_stands_until_an_all_is_answered() {
        let conflicts = Conflicts::default();

        assert_eq!(None, conflicts.standing());
    }

    #[test]
    fn a_one_off_answer_does_not_stand() {
        let conflicts = Conflicts::default();

        conflicts.answer(ConflictChoice::Overwrite);
        assert_eq!(None, conflicts.standing());

        conflicts.answer(ConflictChoice::Skip);
        assert_eq!(None, conflicts.standing());
    }

    #[test]
    fn an_all_answer_stands_for_every_clone() {
        let conflicts = Conflicts::default();
        // What a worker holds: the answer has to reach a copy already running.
        let worker = conflicts.clone();

        conflicts.answer(ConflictChoice::SkipAll);

        assert_eq!(Some(ConflictChoice::SkipAll), worker.standing());
    }

    /// Aliasing, where a filesystem that folds case gives two names one entry,
    /// is reproduced by giving the entry a second name and removing its first,
    /// which leaves it one link, as an aliased entry has.
    #[test]
    fn an_entry_is_pasted_by_identity_rather_than_by_name() {
        let fx = crate::test_support::TempDir::new("conflicts_pasted");
        let pasted = fx.join("one");
        let alias = fx.join("alias");
        let other = fx.join("other");
        std::fs::write(&pasted, b"first").unwrap();
        std::fs::write(&other, b"other").unwrap();
        let conflicts = Conflicts::default();
        // What a worker holds: another task of the paste has to see it.
        let worker = conflicts.clone();

        conflicts.record_pasted(EntryId::of_path(&pasted).unwrap());
        std::fs::hard_link(&pasted, &alias).unwrap();
        std::fs::remove_file(&pasted).unwrap();

        assert!(worker.was_pasted(&alias));
        assert!(!worker.was_pasted(&other));
        assert!(!pasted_here(None, &alias));
    }

    /// Replacing one name of an entry with another link loses nothing: what
    /// the paste wrote survives under the other.
    #[test]
    fn a_pasted_entry_with_another_link_is_not_guarded() {
        let fx = crate::test_support::TempDir::new("conflicts_linked");
        let pasted = fx.join("one");
        let link = fx.join("link");
        std::fs::write(&pasted, b"first").unwrap();
        let conflicts = Conflicts::default();
        conflicts.record_pasted(EntryId::of_path(&pasted).unwrap());

        std::fs::hard_link(&pasted, &link).unwrap();

        assert!(!conflicts.was_pasted(&link));
        assert!(!conflicts.was_pasted(&pasted));
    }
}

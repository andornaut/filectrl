//! The conflict decisions for one paste.

use std::{
    collections::HashSet,
    os::unix::fs::MetadataExt,
    path::Path,
    sync::{Arc, Mutex},
};

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
#[derive(Clone, Default)]
pub(super) struct Conflicts {
    apply_to_all: Arc<Mutex<Option<ConflictChoice>>>,
    pasted: Arc<Mutex<HashSet<(u64, u64)>>>,
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

    /// Records the entry `path` names as one this paste wrote.
    pub(super) fn record_pasted(&self, path: &Path) {
        if let Ok(metadata) = path.symlink_metadata() {
            self.record_pasted_id(metadata.dev(), metadata.ino());
        }
    }

    /// Records the entry with this device and inode as one this paste wrote.
    pub(super) fn record_pasted_id(&self, dev: u64, ino: u64) {
        self.pasted
            .lock()
            .expect("the pasted entries are not poisoned")
            .insert((dev, ino));
    }

    /// Whether the entry `path` names is one this paste wrote.
    pub(super) fn was_pasted(&self, path: &Path) -> bool {
        path.symlink_metadata().is_ok_and(|metadata| {
            self.pasted
                .lock()
                .expect("the pasted entries are not poisoned")
                .contains(&(metadata.dev(), metadata.ino()))
        })
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

    #[test]
    fn an_entry_is_pasted_by_identity_rather_than_by_name() {
        let fx = crate::test_support::TempDir::new("conflicts_pasted");
        let pasted = fx.join("Foo");
        let alias = fx.join("foo");
        let other = fx.join("other");
        std::fs::write(&pasted, b"first").unwrap();
        std::fs::hard_link(&pasted, &alias).unwrap();
        std::fs::write(&other, b"other").unwrap();
        let conflicts = Conflicts::default();
        // What a worker holds: another task of the paste has to see it.
        let worker = conflicts.clone();

        conflicts.record_pasted(&pasted);

        assert!(worker.was_pasted(&alias));
        assert!(!worker.was_pasted(&other));
        assert!(!pasted_here(None, &alias));
    }
}

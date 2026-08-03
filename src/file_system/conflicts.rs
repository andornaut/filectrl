//! The conflict decisions for one paste.

use std::sync::{Arc, Mutex};

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
#[derive(Clone, Default)]
pub(super) struct Conflicts {
    apply_to_all: Arc<Mutex<Option<ConflictChoice>>>,
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

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<ConflictChoice>> {
        // Nothing blocks while the lock is held, so a panic while it is held
        // would have to come from the two lines above.
        self.apply_to_all
            .lock()
            .expect("the conflict state is not poisoned")
    }
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
}

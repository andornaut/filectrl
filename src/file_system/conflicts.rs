//! The conflict decisions for one paste.

use std::sync::{
    Arc, Mutex,
    mpsc::{self, Sender},
};

use crate::command::{Command, ConflictChoice, PromptAction};

/// Shared between the thread running a paste's queue and the workers copying
/// its sources, so that a name already taken deep inside a tree is answered the
/// same way as one at the top level, and an `*All` answer settles both.
///
/// A top-level collision is found before the task starts, so the queue can ask
/// about it and wait without blocking anything. One inside a directory being
/// merged is only found by the worker, so it asks from there and blocks until
/// the answer arrives. Operations are serialized onto one worker, so nothing
/// else is waiting on that thread, and the alternative is failing an entry the
/// user could have resolved.
#[derive(Clone)]
pub(super) struct Conflicts {
    state: Arc<Mutex<State>>,
    tx: Sender<Command>,
}

#[derive(Default)]
struct State {
    /// The standing answer from an `*All`, which resolves every later collision
    /// without asking again.
    apply_to_all: Option<ConflictChoice>,
    /// The reply channel of a worker blocked on an answer.
    waiting: Option<Sender<ConflictChoice>>,
}

impl Conflicts {
    pub(super) fn new(tx: Sender<Command>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            tx,
        }
    }

    /// The standing `*All` answer, if one has been given.
    pub(super) fn standing(&self) -> Option<ConflictChoice> {
        self.lock().apply_to_all
    }

    /// Records an answer from the user. Returns `true` when it was delivered to
    /// a worker that was blocked waiting for it, which means the queue has
    /// nothing to do with it.
    pub(super) fn answer(&self, choice: ConflictChoice) -> bool {
        let mut state = self.lock();
        if matches!(
            choice,
            ConflictChoice::OverwriteAll | ConflictChoice::SkipAll
        ) {
            state.apply_to_all = Some(choice);
        }
        match state.waiting.take() {
            Some(reply) => {
                let _ = reply.send(choice);
                true
            }
            None => false,
        }
    }

    /// Abandons the paste: every later collision skips, and a worker already
    /// blocked is released rather than left waiting for an answer that is no
    /// longer coming.
    /// Returns whether a worker was released, which means the dismissal was
    /// answering that worker rather than the queue.
    pub(super) fn abandon(&self) -> bool {
        let mut state = self.lock();
        state.apply_to_all = Some(ConflictChoice::SkipAll);
        match state.waiting.take() {
            Some(reply) => {
                let _ = reply.send(ConflictChoice::Skip);
                true
            }
            None => false,
        }
    }

    /// What to do about `name`, already taken at a destination inside the tree
    /// being copied. Applies the standing answer when there is one, and
    /// otherwise asks and blocks until the user answers.
    ///
    /// Called from the worker, never from the thread that draws the UI.
    pub(super) fn resolve(&self, name: &str, can_overwrite: bool) -> ConflictChoice {
        let reply = {
            let mut state = self.lock();
            match state.apply_to_all {
                Some(ConflictChoice::SkipAll) => return ConflictChoice::Skip,
                // "Overwrite all" cannot answer for a directory, so that
                // collision is still asked about.
                Some(ConflictChoice::OverwriteAll) if can_overwrite => {
                    return ConflictChoice::Overwrite;
                }
                _ => {}
            }
            let (reply_tx, reply_rx) = mpsc::channel();
            state.waiting = Some(reply_tx);
            reply_rx
        };
        let _ = self.tx.send(Command::OpenPrompt(PromptAction::Conflict {
            name: name.to_string(),
            can_overwrite,
        }));
        // A closed channel means the app is shutting down, so stop rather than
        // block a worker that nothing will ever answer.
        reply.recv().unwrap_or(ConflictChoice::Skip)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // The lock is never held across a blocking wait, so a panic while it is
        // held would have to come from the few lines above.
        self.state
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
    use std::{sync::mpsc, thread, time::Duration};

    use super::*;

    fn conflicts() -> (Conflicts, mpsc::Receiver<Command>) {
        let (tx, rx) = mpsc::channel();
        (Conflicts::new(tx), rx)
    }

    #[test]
    fn a_standing_skip_all_answers_without_asking() {
        let (conflicts, rx) = conflicts();
        conflicts.answer(ConflictChoice::SkipAll);

        assert_eq!(ConflictChoice::Skip, conflicts.resolve("a.txt", true));
        // Nothing was asked, so no prompt was opened.
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_standing_overwrite_all_answers_without_asking() {
        let (conflicts, rx) = conflicts();
        conflicts.answer(ConflictChoice::OverwriteAll);

        assert_eq!(ConflictChoice::Overwrite, conflicts.resolve("a.txt", true));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_standing_overwrite_all_still_asks_about_a_directory() {
        let (conflicts, rx) = conflicts();
        conflicts.answer(ConflictChoice::OverwriteAll);
        let asking = conflicts.clone();
        let worker = thread::spawn(move || asking.resolve("docs", false));

        // It could not be answered by the standing choice, so a prompt opened.
        let opened = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            opened,
            Command::OpenPrompt(PromptAction::Conflict {
                can_overwrite: false,
                ..
            })
        ));
        assert!(conflicts.answer(ConflictChoice::Skip));
        assert_eq!(ConflictChoice::Skip, worker.join().unwrap());
    }

    #[test]
    fn a_worker_blocks_until_the_answer_arrives() {
        let (conflicts, rx) = conflicts();
        let asking = conflicts.clone();
        let worker = thread::spawn(move || asking.resolve("a.txt", true));

        rx.recv_timeout(Duration::from_secs(5))
            .expect("a prompt should have opened");
        // Nothing else can answer it, so the worker is still waiting.
        assert!(conflicts.answer(ConflictChoice::Overwrite));
        assert_eq!(ConflictChoice::Overwrite, worker.join().unwrap());
    }

    #[test]
    fn an_answer_with_nobody_waiting_is_reported_as_unclaimed() {
        let (conflicts, _rx) = conflicts();

        // The queue asked this one, so it has to handle the answer itself.
        assert!(!conflicts.answer(ConflictChoice::Skip));
    }

    #[test]
    fn abandoning_releases_a_blocked_worker_and_skips_the_rest() {
        let (conflicts, rx) = conflicts();
        let asking = conflicts.clone();
        let worker = thread::spawn(move || asking.resolve("a.txt", true));
        rx.recv_timeout(Duration::from_secs(5))
            .expect("a prompt should have opened");

        assert!(conflicts.abandon());

        assert_eq!(ConflictChoice::Skip, worker.join().unwrap());
        // Everything still to come skips too, rather than reopening the prompt
        // the user just dismissed.
        assert_eq!(ConflictChoice::Skip, conflicts.resolve("b.txt", true));
    }
}

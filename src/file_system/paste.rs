//! The paste state machine: what each source of a paste meets at its
//! destination, and what to do about it.

use std::{
    collections::{HashSet, VecDeque},
    ffi::OsString,
};

use super::{conflicts::Conflicts, entry_id::Seen, path_info::PathInfo, tasks};
use crate::{
    app::clipboard::ClipboardEntry,
    command::{Command, ConflictChoice},
};

/// A paste running one source at a time, so a name that is already taken in the
/// destination can be answered for before the next source starts. Held only
/// while the conflict prompt is open: `advance_paste` takes it, and puts it back
/// only when it needs an answer.
pub(super) struct PendingPaste {
    /// `Move` when true, `Copy` when false. Decides both the task kind and
    /// which clipboard entry an unfinished paste leaves behind.
    pub(super) is_move: bool,
    pub(super) dest: PathInfo,
    /// Sources not yet processed. The one being asked about stays at the front
    /// until the answer pops it.
    pub(super) remaining: VecDeque<PathInfo>,
    /// Sources that could not be started, kept so a retry carries only them.
    pub(super) failed: Vec<PathInfo>,
    /// How many tasks actually started, which decides whether the clipboard is
    /// cleared, reduced, or left alone.
    pub(super) started: usize,
    /// The standing `*All` answer, shared with the workers running this
    /// paste's sources (`Conflicts`). "Overwrite all" still asks about a
    /// directory, which is never replaced.
    pub(super) conflicts: Conflicts,
    /// The names of the sources started earlier in this paste. Two marked
    /// sources can share a basename (search results make it easy), and the
    /// second is refused rather than asked about, like `mv a/x b/x d/`:
    /// whatever the answer, replacing the name would destroy what the first
    /// source put there, and for a cut that is the only copy.
    pub(super) claimed: HashSet<OsString>,
    /// When the paste began, by this machine's clock. An entry born since
    /// (`Seen::birth`) is taken for one this paste wrote, under a name the
    /// destination treats as another's (`meet`).
    began: (i64, i64),
    /// The entry the open conflict prompt is asking about (`meet`), which an
    /// "overwrite" answer allows the source's work to replace and nothing
    /// else.
    asked: Option<Seen>,
}

/// What already holds a source's name in the destination directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Occupant {
    /// A directory, or anything where a directory source goes, neither of
    /// which is ever replaced. Removing a directory would take its contents
    /// with it, and it is never merged into; a directory never replaces a
    /// non-directory either, as `cp -R` and `mv` refuse to.
    Irreplaceable,
    /// A file, symlink, or other non-directory where a non-directory goes,
    /// which the user may replace.
    Replaceable,
}

/// What a paste should do with the source at the front of its queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PasteStep {
    /// Stop and ask. `can_overwrite` is false for an irreplaceable occupant,
    /// whose replacement is never offered.
    Ask { can_overwrite: bool },
    /// Drop the source without running anything.
    Skip,
    /// Run the source, replacing `replace`, the entry found at its
    /// destination, when that is given.
    Run { replace: Option<Seen> },
    /// Refuse the source: an entry made since the paste began holds its name.
    Taken,
}

impl PendingPaste {
    /// A paste of `sources` into `dest`, a move when `is_move`, with nothing
    /// answered yet.
    pub(super) fn new(is_move: bool, dest: &PathInfo, sources: &[PathInfo]) -> Self {
        Self {
            is_move,
            dest: dest.clone(),
            remaining: sources.iter().cloned().collect(),
            failed: Vec::new(),
            started: 0,
            conflicts: Conflicts::default(),
            claimed: HashSet::new(),
            began: now(),
            asked: None,
        }
    }

    /// What to do with `src`, given what holds its destination name on disk
    /// and the paste's standing answer. When it asks, the entry found there
    /// is recorded as the only one an "overwrite" answer may replace.
    ///
    /// An entry born since the paste began is refused rather than offered,
    /// whatever the answer: on a destination that treats two names as one
    /// (case, Unicode normalization), it is most likely what an earlier source
    /// of this paste wrote under the other name, and replacing it would
    /// destroy that source's only copy for a cut. Another program's entry
    /// made in the meantime is refused too. Only a birth time the filesystem
    /// recorded counts; where there is none, only the names are compared
    /// (`is_claimed`).
    pub(super) fn meet(&mut self, src: &PathInfo) -> PasteStep {
        let found = existing_destination(&self.dest, src);
        if found.is_some_and(|(_, seen)| seen.birth().is_some_and(|birth| birth >= self.began)) {
            return PasteStep::Taken;
        }
        let step = step(self.conflicts.standing(), found);
        if let PasteStep::Ask { .. } = step {
            self.asked = found.map(|(_, seen)| seen);
        }
        step
    }

    /// Whether an earlier source of this paste already took `src`'s
    /// destination name. Decided from the names alone, never the disk, which
    /// may not show the earlier source yet (its work is only queued).
    pub(super) fn is_claimed(&self, src: &PathInfo) -> bool {
        src.path
            .file_name()
            .is_some_and(|name| self.claimed.contains(name))
    }

    /// Records that `src`'s destination name is taken, once its work is
    /// actually running.
    pub(super) fn claim(&mut self, src: &PathInfo) {
        if let Some(name) = src.path.file_name() {
            self.claimed.insert(name.to_os_string());
        }
    }

    /// Records the answer to the collision in front of the user. Returns the
    /// entry the answered source may replace, the one the prompt asked about,
    /// or `None` when the source does not run. A "skip all" also reaches the
    /// sources already handed to a worker (`Conflicts::skips_raced`).
    pub(super) fn answer(&mut self, choice: ConflictChoice) -> Option<Seen> {
        self.conflicts.stand(choice);
        let asked = self.asked.take();
        if matches!(
            choice,
            ConflictChoice::Overwrite | ConflictChoice::OverwriteAll
        ) {
            asked
        } else {
            None
        }
    }

    /// The clipboard follow-up once the paste is finished or abandoned. Nothing
    /// started leaves the clipboard untouched so the paste can be retried
    /// as-is; a clean run clears it; a partial run reduces it to what was not
    /// pasted, because a full retry would collide with the destinations just
    /// created.
    pub(super) fn clipboard_follow_up(self) -> Option<Command> {
        if self.started == 0 {
            return None;
        }
        if self.failed.is_empty() {
            return Some(Command::SetClipboardEntry(None));
        }
        let entry = if self.is_move {
            ClipboardEntry::Move(self.failed)
        } else {
            ClipboardEntry::Copy(self.failed)
        };
        Some(Command::SetClipboardEntry(Some(entry)))
    }
}

/// This machine's clock now, in the shape of a `Seen` time.
fn now() -> (i64, i64) {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (
        i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        i64::from(since.subsec_nanos()),
    )
}

/// What to do with a source, given the paste's standing answer and what already
/// holds its destination name, if anything, with the entry it is. Pure, so the
/// whole answer matrix can be exercised without a worker.
fn step(standing: Option<ConflictChoice>, found: Option<(Occupant, Seen)>) -> PasteStep {
    let Some((occupant, entry)) = found else {
        return PasteStep::Run { replace: None };
    };
    let can_overwrite = occupant == Occupant::Replaceable;
    match standing {
        Some(ConflictChoice::SkipAll) => PasteStep::Skip,
        // "Overwrite all" cannot answer for what is never replaced, so that
        // collision is still asked about.
        Some(ConflictChoice::OverwriteAll) if can_overwrite => PasteStep::Run {
            replace: Some(entry),
        },
        _ => PasteStep::Ask { can_overwrite },
    }
}

/// What already holds `src`'s name in `dest`, and which entry it is, or `None`
/// when the name is free. Links are not followed, so a symlink to a directory
/// reports `Replaceable` to a non-directory source and is replaced as a link
/// rather than treated as the directory it points at.
fn existing_destination(dest: &PathInfo, src: &PathInfo) -> Option<(Occupant, Seen)> {
    let name = src.path.file_name()?;
    let destination = dest.path.join(name);
    let seen = Seen::of_path(&destination).ok()??;
    // The name holds the source itself or another link to it, or the source
    // is a symlink to what holds it: the paste is refused, so offering to
    // replace it would promise what cannot happen and let an "overwrite all"
    // stand on a collision that was never real.
    if tasks::onto_itself(&src.path, &destination).is_some() {
        return None;
    }
    let occupant = if src.is_directory() || seen.is_directory() {
        Occupant::Irreplaceable
    } else {
        Occupant::Replaceable
    };
    // From the same look that classified it, so the entry an "overwrite"
    // answer allows replacing is the one this found.
    Some((occupant, seen))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use test_case::test_case;

    use super::*;
    use crate::file_system::tests::CopyFixture;

    /// A source that is only a path, for what is decided from names alone.
    fn source(path: impl Into<PathBuf>) -> PathInfo {
        let mut info = PathInfo::try_from(Path::new("/")).unwrap();
        info.path = path.into();
        info
    }

    fn pending(standing: Option<ConflictChoice>) -> PendingPaste {
        let mut pending =
            PendingPaste::new(false, &PathInfo::try_from(Path::new("/")).unwrap(), &[]);
        if let Some(standing) = standing {
            pending.answer(standing);
        }
        pending
    }

    #[test_case(None, None => "run" ; "a free name just runs")]
    #[test_case(None, Some(Occupant::Replaceable) => "ask, offering overwrite" ; "a file asks, offering overwrite")]
    #[test_case(None, Some(Occupant::Irreplaceable) => "ask, withholding overwrite" ; "a directory asks, withholding overwrite")]
    #[test_case(Some(ConflictChoice::SkipAll), None => "run" ; "skip all does not skip a free name")]
    #[test_case(Some(ConflictChoice::SkipAll), Some(Occupant::Replaceable) => "skip" ; "skip all skips a file")]
    #[test_case(Some(ConflictChoice::SkipAll), Some(Occupant::Irreplaceable) => "skip" ; "skip all skips a directory")]
    #[test_case(Some(ConflictChoice::OverwriteAll), None => "run" ; "overwrite all does not force a free name")]
    #[test_case(Some(ConflictChoice::OverwriteAll), Some(Occupant::Replaceable) => "run, replacing the entry found" ; "overwrite all replaces the file found")]
    #[test_case(Some(ConflictChoice::OverwriteAll), Some(Occupant::Irreplaceable) => "ask, withholding overwrite" ; "overwrite all still asks about a directory")]
    fn the_paste_step_matrix(
        standing: Option<ConflictChoice>,
        occupant: Option<Occupant>,
    ) -> &'static str {
        let fx = crate::test_support::TempDir::new("paste_step");
        fs::write(fx.join("found"), b"found").unwrap();
        let found = crate::file_system::entry_id::seen(&fx.join("found")).unwrap();

        match step(standing, occupant.map(|occupant| (occupant, found))) {
            PasteStep::Ask {
                can_overwrite: true,
            } => "ask, offering overwrite",
            PasteStep::Ask {
                can_overwrite: false,
            } => "ask, withholding overwrite",
            PasteStep::Skip => "skip",
            PasteStep::Taken => "taken",
            PasteStep::Run { replace: None } => "run",
            PasteStep::Run {
                replace: Some(entry),
            } => {
                assert_eq!(found, entry);
                "run, replacing the entry found"
            }
        }
    }

    #[test]
    fn a_name_claimed_earlier_in_the_paste_is_taken_whatever_the_disk_shows() {
        let fx = CopyFixture::new("fs_claimed");
        let mut pending = pending(None);
        pending.dest = fx.dest.clone();
        // Two marked sources can share a basename when the marks span
        // directories, which search results make easy.
        let mut twin = fx.src.clone();
        twin.path = fx
            .dest
            .path
            .parent()
            .unwrap()
            .join("elsewhere")
            .join("a.txt");

        assert!(!pending.is_claimed(&twin));
        pending.claim(&fx.src);

        // The first source's work is only queued, so the disk still shows the
        // name as free.
        assert_eq!(PasteStep::Run { replace: None }, pending.meet(&twin));
        assert!(pending.is_claimed(&twin));
        assert!(!pending.is_claimed(&fx.other));
    }

    /// A claim is of the name exactly: a name the destination might treat as
    /// the same is left to `meet`, which sees what the earlier source wrote.
    #[test]
    fn a_claim_takes_only_its_own_name() {
        let mut pending = pending(None);

        pending.claim(&source("/nowhere/one/Notes.txt"));

        assert!(pending.is_claimed(&source("/nowhere/two/Notes.txt")));
        assert!(!pending.is_claimed(&source("/nowhere/two/notes.txt")));
    }

    /// An entry born since the paste began is refused without a prompt,
    /// whatever the standing answer: it is taken for what an earlier source
    /// wrote under a name the destination treats as the same. One that was
    /// there before is asked about as usual. Needs a filesystem that records
    /// birth times; skipped otherwise.
    #[test_case(None ; "with no standing answer")]
    #[test_case(Some(ConflictChoice::OverwriteAll) ; "under overwrite all")]
    #[test_case(Some(ConflictChoice::SkipAll) ; "under skip all")]
    fn an_entry_born_since_the_paste_began_is_taken(standing: Option<ConflictChoice>) {
        let fx = CopyFixture::new("fs_born_since");
        if !crate::file_system::entry_id::records_birth_time(&fx.dest.path) {
            eprintln!("skipped: this filesystem records no birth time");
            return;
        }
        fx.occupy("b.txt");
        crate::test_support::tick();
        let mut pending = pending(standing);
        pending.dest = fx.dest.clone();
        crate::test_support::tick();
        fx.occupy("a.txt");

        assert_eq!(PasteStep::Taken, pending.meet(&fx.src));
        assert_ne!(PasteStep::Taken, pending.meet(&fx.other));
    }

    #[test]
    fn a_later_all_answer_replaces_the_standing_one() {
        let mut pending = pending(Some(ConflictChoice::OverwriteAll));

        pending.answer(ConflictChoice::SkipAll);

        // Deliberate: "skip all" answers for the whole batch, so it supersedes
        // an earlier "overwrite all". A directory collision is how this comes
        // up, reopening the prompt with only the skip choices; the single-entry
        // `s` leaves the standing answer alone.
        assert_eq!(Some(ConflictChoice::SkipAll), pending.conflicts.standing());
        assert!(pending.conflicts.skips_raced());
        pending.answer(ConflictChoice::Skip);
        assert_eq!(Some(ConflictChoice::SkipAll), pending.conflicts.standing());
    }

    /// Only an overwrite runs the answered source, and it may replace only
    /// the entry the prompt asked about.
    #[test_case(ConflictChoice::Skip => (false, None) ; "skip runs nothing and does not stand")]
    #[test_case(ConflictChoice::Overwrite => (true, None) ; "overwrite runs and does not stand")]
    #[test_case(ConflictChoice::SkipAll => (false, Some(ConflictChoice::SkipAll)) ; "skip all runs nothing and stands")]
    #[test_case(ConflictChoice::OverwriteAll => (true, Some(ConflictChoice::OverwriteAll)) ; "overwrite all runs and stands")]
    fn an_answer_decides_the_source_and_whether_it_stands(
        choice: ConflictChoice,
    ) -> (bool, Option<ConflictChoice>) {
        let fx = CopyFixture::new("fs_answered");
        fx.occupy("a.txt");
        let mut pending = pending(None);
        pending.dest = fx.dest.clone();
        assert_eq!(
            PasteStep::Ask {
                can_overwrite: true
            },
            pending.meet(&fx.src)
        );
        let asked = pending.asked;
        assert!(asked.is_some(), "the prompt records what it asks about");

        let granted = pending.answer(choice);

        assert!(granted.is_none() || granted == asked);
        assert_eq!(None, pending.asked, "an answer is for one prompt");
        (granted.is_some(), pending.conflicts.standing())
    }

    /// A paste with `started` tasks started and `failed` sources that could not.
    fn finished(is_move: bool, started: usize, failed: Vec<PathInfo>) -> Option<Command> {
        let mut pending =
            PendingPaste::new(is_move, &PathInfo::try_from(Path::new("/")).unwrap(), &[]);
        pending.started = started;
        pending.failed = failed;
        pending.clipboard_follow_up()
    }

    /// The nothing-started, clean and partial outcomes are all covered against
    /// the real handler in `file_system.rs`. What only this reaches is the move: a partial
    /// one has to keep the operation, or the retry would copy what it cut.
    #[test]
    fn a_partial_move_keeps_the_clipboard_under_the_move_operation() {
        let src = PathInfo::try_from(Path::new("/")).unwrap();
        assert_eq!(
            Some(Command::SetClipboardEntry(Some(ClipboardEntry::Move(
                vec![src.clone()]
            )))),
            finished(true, 1, vec![src])
        );
    }

    #[test]
    fn a_destination_is_classified_by_what_holds_the_name() {
        let fx = CopyFixture::new("fs_occupant");
        assert_eq!(
            None,
            existing_destination(&fx.dest, &fx.src).map(|(occupant, _)| occupant)
        );

        fx.occupy("a.txt");
        assert_eq!(
            Some(Occupant::Replaceable),
            existing_destination(&fx.dest, &fx.src).map(|(occupant, _)| occupant)
        );

        fx.occupy_with_directory("b.txt");
        assert_eq!(
            Some(Occupant::Irreplaceable),
            existing_destination(&fx.dest, &fx.other).map(|(occupant, _)| occupant)
        );

        // The file at `a.txt` is replaceable by a file, never by a directory.
        let dir_source = fx.src.path.parent().unwrap().join("dirs").join("a.txt");
        fs::create_dir_all(&dir_source).unwrap();
        let dir_source = PathInfo::try_from(dir_source.as_path()).unwrap();
        assert_eq!(
            Some(Occupant::Irreplaceable),
            existing_destination(&fx.dest, &dir_source).map(|(occupant, _)| occupant)
        );
    }

    /// The entry an "overwrite" answer may replace is the one found, from the
    /// same look at the name.
    #[test]
    fn a_destination_is_found_with_its_identity() {
        let fx = CopyFixture::new("fs_occupant_id");
        fx.occupy("a.txt");

        assert_eq!(
            Some((
                Occupant::Replaceable,
                Seen::of_path(&fx.dest.path.join("a.txt")).unwrap().unwrap()
            )),
            existing_destination(&fx.dest, &fx.src)
        );
    }

    #[test]
    fn pasting_into_the_source_directory_is_not_a_collision() {
        let fx = CopyFixture::new("fs_occupant_self");
        let src_dir = PathInfo::try_from(fx.src.path.parent().unwrap()).unwrap();

        // The destination name resolves to the source itself, which the
        // operation refuses outright. A reported collision would offer to
        // replace the file being pasted, and an "overwrite all" would then stand
        // for the rest of the batch on the strength of it.
        assert_eq!(
            None,
            existing_destination(&src_dir, &fx.src).map(|(occupant, _)| occupant)
        );
    }

    #[test]
    fn pasting_into_a_symlink_to_the_source_directory_is_not_a_collision() {
        let fx = CopyFixture::new("fs_occupant_self_link");
        let src_dir = fx.src.path.parent().unwrap();
        let link = fx.dest.path.parent().unwrap().join("link");
        std::os::unix::fs::symlink(src_dir, &link).unwrap();
        let aliased = PathInfo::try_from(link.as_path()).unwrap();

        // Same entry, reached through a symlinked parent.
        assert_eq!(
            None,
            existing_destination(&aliased, &fx.src).map(|(occupant, _)| occupant)
        );
    }

    #[test]
    fn a_symlink_in_the_way_is_a_collision_even_when_it_points_at_the_source() {
        let fx = CopyFixture::new("fs_occupant_link_to_source");
        std::os::unix::fs::symlink(&fx.src.path, fx.dest.path.join("a.txt")).unwrap();

        // The link is its own entry: pasting would replace the link, not the
        // file it points at, so it is a collision to ask about.
        assert_eq!(
            Some(Occupant::Replaceable),
            existing_destination(&fx.dest, &fx.src).map(|(occupant, _)| occupant)
        );
    }

    #[test]
    fn a_symlink_source_whose_target_holds_the_name_is_not_a_collision() {
        let fx = CopyFixture::new("fs_occupant_link_over_target");
        fs::write(fx.dest.path.join("a.txt"), b"data").unwrap();
        let link_dir = fx.src.path.parent().unwrap().join("links");
        fs::create_dir(&link_dir).unwrap();
        std::os::unix::fs::symlink(fx.dest.path.join("a.txt"), link_dir.join("a.txt")).unwrap();
        let link = PathInfo::try_from(link_dir.join("a.txt").as_path()).unwrap();

        // Offering to replace it would promise a paste that validation then
        // refuses, since it would replace the file the link points at.
        assert_eq!(
            None,
            existing_destination(&fx.dest, &link).map(|(occupant, _)| occupant)
        );
    }

    #[test]
    fn a_hard_link_of_the_source_is_not_a_collision() {
        let fx = CopyFixture::new("fs_occupant_hard_link");
        fs::hard_link(&fx.src.path, fx.dest.path.join("a.txt")).unwrap();

        assert_eq!(
            None,
            existing_destination(&fx.dest, &fx.src).map(|(occupant, _)| occupant)
        );
    }

    #[test]
    fn a_symlinked_directory_in_the_way_is_replaceable() {
        let fx = CopyFixture::new("fs_occupant_symlink");
        fx.occupy_with_directory("target");
        std::os::unix::fs::symlink(fx.dest.path.join("target"), fx.dest.path.join("a.txt"))
            .unwrap();

        // Replacing it unlinks the link, which does not touch the directory it
        // points at, so the overwrite choices stay available.
        assert_eq!(
            Some(Occupant::Replaceable),
            existing_destination(&fx.dest, &fx.src).map(|(occupant, _)| occupant)
        );
    }
}

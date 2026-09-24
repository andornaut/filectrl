//! The paste state machine: what each source of a paste meets at its
//! destination, and what to do about it.

use std::{
    collections::{HashSet, VecDeque},
    ffi::OsString,
};

use super::{
    conflicts::{self, Conflicts},
    path_info::PathInfo,
    tasks,
};
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
    /// The paste's standing `*All` answer, shared with the workers so one given
    /// here also settles a name another process takes inside a tree already
    /// being copied.
    pub(super) conflicts: Conflicts,
    /// Destination names already taken by sources started earlier in this
    /// paste. Two marked sources can share a basename (search results make it
    /// easy), and the second is refused rather than asked about, like `mv a/x
    /// b/x d/`: whatever the answer, replacing the name would destroy what the
    /// first source put there, and for a cut that is the only copy.
    pub(super) claimed: HashSet<OsString>,
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

impl Occupant {
    /// The occupant a source meets, from whether each of the two is a
    /// directory.
    pub(super) fn of(source_is_directory: bool, is_directory: bool) -> Self {
        if source_is_directory || is_directory {
            Self::Irreplaceable
        } else {
            Self::Replaceable
        }
    }
}

/// What a paste should do with the source at the front of its queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PasteStep {
    /// Stop and ask. `can_overwrite` is false for an irreplaceable occupant,
    /// whose replacement is never offered.
    Ask { can_overwrite: bool },
    /// Drop the source without running anything.
    Skip,
    /// Run the source, replacing what is at its destination when `overwrite`.
    Run { overwrite: bool },
}

impl PendingPaste {
    /// What holds `src`'s destination name on disk.
    pub(super) fn occupant(&self, src: &PathInfo) -> Option<Occupant> {
        existing_destination(&self.dest, src)
    }

    /// Whether an earlier source of this paste already took `src`'s
    /// destination name: the same name, checked before the disk, which may not
    /// show it yet (the earlier work is only queued) or may show what the
    /// earlier source replaced it with; or a name the disk shows holding what
    /// an earlier source wrote, as two names are one entry on a filesystem
    /// that folds case or normalization. Refused rather than asked about,
    /// since a worker refuses to replace that entry whatever the answer.
    pub(super) fn is_claimed(&self, src: &PathInfo) -> bool {
        src.path.file_name().is_some_and(|name| {
            self.claimed.contains(name) || self.conflicts.was_pasted(&self.dest.path.join(name))
        })
    }

    /// Records that `src`'s destination name is taken, once its work is
    /// actually running.
    pub(super) fn claim(&mut self, src: &PathInfo) {
        if let Some(name) = src.path.file_name() {
            self.claimed.insert(name.to_os_string());
        }
    }

    /// What to do with the source at the front of the queue, given what is
    /// already at its destination.
    pub(super) fn step(&self, occupant: Option<Occupant>) -> PasteStep {
        step(self.conflicts.standing(), occupant)
    }

    /// Records the answer to the collision in front of the user. Returns
    /// whether the answered source runs, replacing its destination. An `*All`
    /// also reaches the sources already handed to a worker.
    pub(super) fn answer(&mut self, choice: ConflictChoice) -> bool {
        self.conflicts.answer(choice);
        conflicts::replaces(choice)
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

/// What to do with a source, given the paste's standing answer and what already
/// holds its destination name. Pure, so the whole answer matrix can be
/// exercised without a filesystem or a worker.
///
/// Shared with the workers, which apply it to a name another process takes
/// inside a tree they are already copying. `Ask` is the one outcome a worker
/// cannot act on, so it records the collision instead.
pub(super) fn step(standing: Option<ConflictChoice>, occupant: Option<Occupant>) -> PasteStep {
    let Some(occupant) = occupant else {
        return PasteStep::Run { overwrite: false };
    };
    let can_overwrite = occupant == Occupant::Replaceable;
    match standing {
        Some(ConflictChoice::SkipAll) => PasteStep::Skip,
        // "Overwrite all" cannot answer for what is never replaced, so that
        // collision is still asked about.
        Some(ConflictChoice::OverwriteAll) if can_overwrite => PasteStep::Run { overwrite: true },
        _ => PasteStep::Ask { can_overwrite },
    }
}

/// What already holds `src`'s name in `dest`, or `None` when the name is free.
/// Links are not followed, so a symlink to a directory reports `Replaceable`
/// to a non-directory source and is replaced as a link rather than treated as
/// the directory it points at.
pub(super) fn existing_destination(dest: &PathInfo, src: &PathInfo) -> Option<Occupant> {
    let name = src.path.file_name()?;
    let destination = dest.path.join(name);
    let metadata = destination.symlink_metadata().ok()?;
    // The source itself, another name of it, or a symlink to it holds the
    // name: the paste is refused, so offering to replace it would promise what
    // cannot happen and let an "overwrite all" stand on a collision that was
    // never real.
    if tasks::onto_itself(&src.path, &destination).is_some() {
        return None;
    }
    Some(Occupant::of(src.is_directory(), metadata.is_dir()))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use test_case::test_case;

    use super::*;
    use crate::file_system::tests::CopyFixture;

    fn pending(standing: Option<ConflictChoice>) -> PendingPaste {
        let conflicts = Conflicts::default();
        if let Some(standing) = standing {
            conflicts.answer(standing);
        }
        PendingPaste {
            is_move: false,
            dest: PathInfo::try_from(Path::new("/")).unwrap(),
            remaining: VecDeque::new(),
            failed: Vec::new(),
            started: 0,
            conflicts,
            claimed: HashSet::new(),
        }
    }

    #[test_case(None, None => PasteStep::Run { overwrite: false } ; "a free name just runs")]
    #[test_case(None, Some(Occupant::Replaceable) => PasteStep::Ask { can_overwrite: true } ; "a file asks, offering overwrite")]
    #[test_case(None, Some(Occupant::Irreplaceable) => PasteStep::Ask { can_overwrite: false } ; "a directory asks, withholding overwrite")]
    #[test_case(Some(ConflictChoice::SkipAll), None => PasteStep::Run { overwrite: false } ; "skip all does not skip a free name")]
    #[test_case(Some(ConflictChoice::SkipAll), Some(Occupant::Replaceable) => PasteStep::Skip ; "skip all skips a file")]
    #[test_case(Some(ConflictChoice::SkipAll), Some(Occupant::Irreplaceable) => PasteStep::Skip ; "skip all skips a directory")]
    #[test_case(Some(ConflictChoice::OverwriteAll), None => PasteStep::Run { overwrite: false } ; "overwrite all does not force a free name")]
    #[test_case(Some(ConflictChoice::OverwriteAll), Some(Occupant::Replaceable) => PasteStep::Run { overwrite: true } ; "overwrite all replaces a file")]
    #[test_case(Some(ConflictChoice::OverwriteAll), Some(Occupant::Irreplaceable) => PasteStep::Ask { can_overwrite: false } ; "overwrite all still asks about a directory")]
    fn the_paste_step_matrix(
        standing: Option<ConflictChoice>,
        occupant: Option<Occupant>,
    ) -> PasteStep {
        step(standing, occupant)
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
        assert_eq!(None, pending.occupant(&twin));
        assert!(pending.is_claimed(&twin));
        assert!(!pending.is_claimed(&fx.other));
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
        pending.answer(ConflictChoice::Skip);
        assert_eq!(Some(ConflictChoice::SkipAll), pending.conflicts.standing());
    }

    #[test_case(ConflictChoice::Skip => (false, None) ; "skip runs nothing and does not stand")]
    #[test_case(ConflictChoice::Overwrite => (true, None) ; "overwrite runs and does not stand")]
    #[test_case(ConflictChoice::SkipAll => (false, Some(ConflictChoice::SkipAll)) ; "skip all runs nothing and stands")]
    #[test_case(ConflictChoice::OverwriteAll => (true, Some(ConflictChoice::OverwriteAll)) ; "overwrite all runs and stands")]
    fn an_answer_decides_the_source_and_whether_it_stands(
        choice: ConflictChoice,
    ) -> (bool, Option<ConflictChoice>) {
        let mut pending = pending(None);
        let runs = pending.answer(choice);
        (runs, pending.conflicts.standing())
    }

    /// A paste with `started` tasks started and `failed` sources that could not.
    fn finished(is_move: bool, started: usize, failed: Vec<PathInfo>) -> Option<Command> {
        PendingPaste {
            is_move,
            dest: PathInfo::try_from(Path::new("/")).unwrap(),
            remaining: VecDeque::new(),
            failed,
            started,
            conflicts: Conflicts::default(),
            claimed: HashSet::new(),
        }
        .clipboard_follow_up()
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
        assert_eq!(None, existing_destination(&fx.dest, &fx.src));

        fx.occupy("a.txt");
        assert_eq!(
            Some(Occupant::Replaceable),
            existing_destination(&fx.dest, &fx.src)
        );

        fx.occupy_with_directory("b.txt");
        assert_eq!(
            Some(Occupant::Irreplaceable),
            existing_destination(&fx.dest, &fx.other)
        );

        // The file at `a.txt` is replaceable by a file, never by a directory.
        let dir_source = fx.src.path.parent().unwrap().join("dirs").join("a.txt");
        fs::create_dir_all(&dir_source).unwrap();
        let dir_source = PathInfo::try_from(dir_source.as_path()).unwrap();
        assert_eq!(
            Some(Occupant::Irreplaceable),
            existing_destination(&fx.dest, &dir_source)
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
        assert_eq!(None, existing_destination(&src_dir, &fx.src));
    }

    #[test]
    fn pasting_into_a_symlink_to_the_source_directory_is_not_a_collision() {
        let fx = CopyFixture::new("fs_occupant_self_link");
        let src_dir = fx.src.path.parent().unwrap();
        let link = fx.dest.path.parent().unwrap().join("link");
        std::os::unix::fs::symlink(src_dir, &link).unwrap();
        let aliased = PathInfo::try_from(link.as_path()).unwrap();

        // Same entry, reached through a symlinked parent.
        assert_eq!(None, existing_destination(&aliased, &fx.src));
    }

    #[test]
    fn a_symlink_in_the_way_is_a_collision_even_when_it_points_at_the_source() {
        let fx = CopyFixture::new("fs_occupant_link_to_source");
        std::os::unix::fs::symlink(&fx.src.path, fx.dest.path.join("a.txt")).unwrap();

        // The link is its own entry: pasting would replace the link, not the
        // file it points at, so it is a collision to ask about.
        assert_eq!(
            Some(Occupant::Replaceable),
            existing_destination(&fx.dest, &fx.src)
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
        assert_eq!(None, existing_destination(&fx.dest, &link));
    }

    #[test]
    fn a_hard_link_of_the_source_is_not_a_collision() {
        let fx = CopyFixture::new("fs_occupant_hard_link");
        fs::hard_link(&fx.src.path, fx.dest.path.join("a.txt")).unwrap();

        assert_eq!(None, existing_destination(&fx.dest, &fx.src));
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
            existing_destination(&fx.dest, &fx.src)
        );
    }
}

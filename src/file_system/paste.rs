//! The paste state machine: what each source of a paste meets at its
//! destination, and what to do about it.

use std::{
    collections::{HashSet, VecDeque},
    ffi::OsString,
};

use super::{path_info::PathInfo, tasks};
use crate::{
    app::clipboard::ClipboardEntry,
    command::{Command, ConflictChoice},
};

/// A paste running one source at a time, so a taken name can be answered for
/// before the next source starts. Held only while the conflict prompt is open.
pub(super) struct PendingPaste {
    /// `Move` when true, `Copy` when false.
    pub(super) is_move: bool,
    pub(super) dest: PathInfo,
    /// Sources not yet processed; the one being asked about stays at the front.
    pub(super) remaining: VecDeque<PathInfo>,
    /// Sources that could not be started, for the clipboard follow-up.
    pub(super) failed: Vec<PathInfo>,
    /// How many tasks started, which decides the clipboard follow-up.
    pub(super) started: usize,
    /// The standing `*All` answer, which answers every later collision.
    pub(super) standing: Option<ConflictChoice>,
    /// Names of the sources started earlier in this paste. A second source of the
    /// same name is refused rather than asked about, like `mv a/x b/x d/`.
    pub(super) claimed: HashSet<OsString>,
}

/// What already holds a source's name in the destination directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Occupant {
    /// A directory, or anything where a directory source goes: never replaced or
    /// merged into, like `cp -R` and `mv`.
    Irreplaceable,
    /// A non-directory where a non-directory goes, which the user may replace.
    Replaceable,
}

/// What a paste should do with the source at the front of its queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PasteStep {
    /// Stop and ask. `can_overwrite` is false for an irreplaceable occupant.
    Ask {
        can_overwrite: bool,
    },
    Skip,
    /// Run the source, replacing what holds its name when `overwrite`.
    Run {
        overwrite: bool,
    },
}

impl PendingPaste {
    pub(super) fn new(is_move: bool, dest: &PathInfo, sources: &[PathInfo]) -> Self {
        Self {
            is_move,
            dest: dest.clone(),
            remaining: sources.iter().cloned().collect(),
            failed: Vec::new(),
            started: 0,
            standing: None,
            claimed: HashSet::new(),
        }
    }

    /// What to do with `src`, given its destination name on disk and the standing
    /// answer.
    pub(super) fn meet(&self, src: &PathInfo) -> PasteStep {
        step(self.standing, existing_destination(&self.dest, src))
    }

    /// Whether an earlier source of this paste took `src`'s name. Decided from
    /// names alone, since the earlier source's work may only be queued.
    pub(super) fn is_claimed(&self, src: &PathInfo) -> bool {
        src.path
            .file_name()
            .is_some_and(|name| self.claimed.contains(name))
    }

    /// Records `src`'s destination name as taken, once its work is running.
    pub(super) fn claim(&mut self, src: &PathInfo) {
        if let Some(name) = src.path.file_name() {
            self.claimed.insert(name.to_os_string());
        }
    }

    /// Records the answer to the open prompt. Returns whether the source runs,
    /// replacing what holds its name.
    pub(super) fn answer(&mut self, choice: ConflictChoice) -> bool {
        match choice {
            ConflictChoice::SkipAll | ConflictChoice::OverwriteAll => self.standing = Some(choice),
            ConflictChoice::Skip | ConflictChoice::Overwrite => {}
        }
        matches!(
            choice,
            ConflictChoice::Overwrite | ConflictChoice::OverwriteAll
        )
    }

    /// The clipboard follow-up once the paste ends: untouched if nothing started,
    /// cleared after a clean run, otherwise reduced to what was not pasted.
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

/// What to do with a source, given the standing answer and what holds its
/// destination name.
fn step(standing: Option<ConflictChoice>, found: Option<Occupant>) -> PasteStep {
    let Some(occupant) = found else {
        return PasteStep::Run { overwrite: false };
    };
    let can_overwrite = occupant == Occupant::Replaceable;
    match standing {
        Some(ConflictChoice::SkipAll) => PasteStep::Skip,
        // "Overwrite all" cannot answer for what is never replaced.
        Some(ConflictChoice::OverwriteAll) if can_overwrite => PasteStep::Run { overwrite: true },
        _ => PasteStep::Ask { can_overwrite },
    }
}

/// What holds `src`'s name in `dest`, or `None` when the name is free. Links are
/// not followed: a symlink to a directory is replaced as a link.
fn existing_destination(dest: &PathInfo, src: &PathInfo) -> Option<Occupant> {
    let name = src.path.file_name()?;
    let destination = dest.path.join(name);
    let metadata = destination.symlink_metadata().ok()?;
    // The paste would be refused as onto itself, so it is not a collision.
    if tasks::onto_itself(&src.path, &destination) {
        return None;
    }
    Some(if src.is_directory() || metadata.is_dir() {
        Occupant::Irreplaceable
    } else {
        Occupant::Replaceable
    })
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
        match step(standing, occupant) {
            PasteStep::Ask {
                can_overwrite: true,
            } => "ask, offering overwrite",
            PasteStep::Ask {
                can_overwrite: false,
            } => "ask, withholding overwrite",
            PasteStep::Skip => "skip",
            PasteStep::Run { overwrite: false } => "run",
            PasteStep::Run { overwrite: true } => "run, replacing the entry found",
        }
    }

    #[test]
    fn a_name_claimed_earlier_in_the_paste_is_taken_whatever_the_disk_shows() {
        let fx = CopyFixture::new("fs_claimed");
        let mut pending = pending(None);
        pending.dest = fx.dest.clone();
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

        // The first source's work is only queued, so the disk shows the name free.
        assert_eq!(PasteStep::Run { overwrite: false }, pending.meet(&twin));
        assert!(pending.is_claimed(&twin));
        assert!(!pending.is_claimed(&fx.other));
    }

    /// A name the destination might treat as the same is left to `meet`.
    #[test]
    fn a_claim_takes_only_its_own_name() {
        let mut pending = pending(None);

        pending.claim(&source("/nowhere/one/Notes.txt"));

        assert!(pending.is_claimed(&source("/nowhere/two/Notes.txt")));
        assert!(!pending.is_claimed(&source("/nowhere/two/notes.txt")));
    }

    #[test]
    fn a_later_all_answer_replaces_the_standing_one() {
        let mut pending = pending(Some(ConflictChoice::OverwriteAll));

        pending.answer(ConflictChoice::SkipAll);

        // Deliberate: "skip all" answers for the whole batch; a single `s` does not stand.
        assert_eq!(Some(ConflictChoice::SkipAll), pending.standing);
        pending.answer(ConflictChoice::Skip);
        assert_eq!(Some(ConflictChoice::SkipAll), pending.standing);
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

        (runs, pending.standing)
    }

    fn finished(is_move: bool, started: usize, failed: Vec<PathInfo>) -> Option<Command> {
        let mut pending =
            PendingPaste::new(is_move, &PathInfo::try_from(Path::new("/")).unwrap(), &[]);
        pending.started = started;
        pending.failed = failed;
        pending.clipboard_follow_up()
    }

    /// The other outcomes are covered in `file_system.rs`; a partial move has to
    /// keep the operation, or the retry would copy what it cut.
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

        assert_eq!(None, existing_destination(&src_dir, &fx.src));
    }

    #[test]
    fn pasting_into_a_symlink_to_the_source_directory_is_not_a_collision() {
        let fx = CopyFixture::new("fs_occupant_self_link");
        let src_dir = fx.src.path.parent().unwrap();
        let link = fx.dest.path.parent().unwrap().join("link");
        std::os::unix::fs::symlink(src_dir, &link).unwrap();
        let aliased = PathInfo::try_from(link.as_path()).unwrap();

        assert_eq!(None, existing_destination(&aliased, &fx.src));
    }

    #[test]
    fn a_symlink_in_the_way_is_a_collision_even_when_it_points_at_the_source() {
        let fx = CopyFixture::new("fs_occupant_link_to_source");
        std::os::unix::fs::symlink(&fx.src.path, fx.dest.path.join("a.txt")).unwrap();

        assert_eq!(
            Some(Occupant::Replaceable),
            existing_destination(&fx.dest, &fx.src)
        );
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

        // Replacing the link does not touch the directory it points at.
        assert_eq!(
            Some(Occupant::Replaceable),
            existing_destination(&fx.dest, &fx.src)
        );
    }
}

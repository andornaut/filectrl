use std::{sync::mpsc, thread, time::Duration};

use nix::{
    fcntl::{FcntlArg, fcntl},
    sys::time::TimeSpec,
};
use rustix::fs::XattrFlags;
use test_case::test_case;

// The module this tests, with every name the copy code imported, as the tests
// used them when they lived beside it.
#[allow(clippy::wildcard_imports)]
use super::{
    super::test_support::{
        Kind, answered, assert_made, copy_task, copy_task_with, destination, every_case,
        finished_task, kind_matrix, make, mode_of, no_paste, paste_after, paste_over, seen,
        src_and_dest, staging_left, within_deadline,
    },
    super::{
        sys::{CWD, *},
        walk::*,
    },
    *,
};
#[allow(clippy::wildcard_imports)]
use super::{attributes::*, entry::*, replace::*, tree::*};
use crate::{
    command::{
        Command, ConflictChoice,
        progress::{ActiveTask, Progress},
    },
    file_system::{
        entry_id::{EntryId, Seen},
        path_info::{PathInfo, compact},
    },
    test_support::TempDir,
};
#[allow(unused_imports)]
use std::{
    ffi::{CStr, CString, OsStr},
    fs::{self, File},
    io::{ErrorKind, Read, Write},
    os::{
        fd::AsFd,
        unix::{ffi::OsStrExt, fs::PermissionsExt},
    },
    path::{Path, PathBuf},
    time::Instant,
};

/// The settings of a copy, or of the copy a move across devices makes
/// when `is_move`, in a paste nobody answered anything for.
fn settings(is_move: bool) -> CopySettings<'static> {
    CopySettings {
        is_move,
        conflicts: no_paste(),
    }
}

/// A copy context for a test, with no standing answer, so a raced name
/// is recorded as an error. `is_move` makes it the copy a move makes.
fn context<'a>(is_move: bool, active: &'a mut ActiveTask, buffer: &'a mut [u8]) -> CopyContext<'a> {
    CopyContext::new(settings(is_move), active, buffer, None, 0)
}

/// A copy task for a test that reads nothing it reports.
fn idle_task() -> ActiveTask {
    let (tx, _rx) = mpsc::channel();
    copy_task(tx)
}

/// The source at `path` as a task is started for it.
fn listed(path: impl AsRef<Path>) -> PathInfo {
    PathInfo::try_from(path.as_ref()).unwrap()
}

/// Copies `src` to `dst` as a copy, or as a move's copy stage when
/// `is_move`, returning the errors recorded.
fn copy_one(is_move: bool, src: &Path, dst: &Path) -> Vec<String> {
    let (tx, _rx) = mpsc::channel();
    let mut active = copy_task(tx);
    let mut buffer = [0u8; 64];
    let mut context = context(is_move, &mut active, &mut buffer);
    assert!(copy_path(&mut context, None, &listed(src), src, dst));
    let errors = context.into_outcome().errors;
    active.done();
    errors
}

// Linux only: recreating a socket goes through mknod, which macOS refuses
// to an unprivileged process with EPERM.
#[cfg(target_os = "linux")]
#[test]
fn copy_path_recreates_socket() {
    let fx = TempDir::new("tasks");
    let src = fx.join("sock");
    let _listener = std::os::unix::net::UnixListener::bind(&src).unwrap();
    let dst = fx.join("sock_copy");

    let errors = copy_one(false, &src, &dst);
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    assert!(unix_mode::is_socket(mode_of(&dst)));
    assert!(src.exists());
}

#[test]
fn copy_path_recreates_fifo() {
    let fx = TempDir::new("tasks");
    let src = fx.join("fifo");
    nix::unistd::mkfifo(&src, nix::sys::stat::Mode::from_bits_truncate(0o644)).unwrap();
    let dst = fx.join("fifo_copy");

    let errors = copy_one(false, &src, &dst);
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    let dst_mode = mode_of(&dst);
    assert!(unix_mode::is_fifo(dst_mode));
    assert_eq!(0o644, dst_mode & 0o7777);
}

#[test]
fn copy_path_continues_past_unreadable_entries() {
    let fx = TempDir::new("tasks");
    let src = fx.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.txt"), b"a").unwrap();
    std::fs::write(src.join("bad"), b"x").unwrap();
    std::fs::write(src.join("c.txt"), b"c").unwrap();
    let mut permissions = std::fs::metadata(src.join("bad")).unwrap().permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(src.join("bad"), permissions).unwrap();
    // A chmod-000 file is still readable by root (CAP_DAC_OVERRIDE) and on
    // mounts that ignore permissions. Probe what this filesystem actually
    // does rather than inspecting the euid, so the assertions below match
    // the environment instead of being skipped in it.
    let is_unreadable = std::fs::File::open(src.join("bad")).is_err();

    let dst = fx.join("dst");

    let errors = copy_one(false, &src, &dst);

    if is_unreadable {
        // Like cp -R: the unreadable entry is recorded, not fatal.
        // Named relative to both trees: `compact` keeps only the last
        // components, which would read the same on either side.
        assert_eq!(
            vec![format!(
                "Failed to copy {} from {} to {}: Permission denied (os error 13)",
                compact(Path::new("bad")),
                compact(&src),
                compact(&dst)
            )],
            errors
        );
        assert!(!dst.join("bad").exists());
    } else {
        // Nothing was unreadable here, so this is a plain full copy.
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert!(dst.join("bad").exists());
    }
    // The walk must reach the entries on both sides of "bad" either way: a
    // failed entry must not abort the siblings.
    assert!(dst.join("a.txt").exists());
    assert!(dst.join("c.txt").exists());
}

#[test]
fn copy_path_reuses_one_buffer_without_leaking_bytes_between_files() {
    let fx = TempDir::new("tasks");
    let src = fx.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a_long.txt"), b"aaaaaaaaaaaaaaaa").unwrap();
    std::fs::write(src.join("b_short.txt"), b"b").unwrap();
    let dst = fx.join("dst");

    // One buffer serves the whole tree, so a short file copied after a
    // longer one must not pick up the previous file's trailing bytes.
    let errors = copy_one(false, &src, &dst);
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");

    assert_eq!(
        b"aaaaaaaaaaaaaaaa".to_vec(),
        std::fs::read(dst.join("a_long.txt")).unwrap()
    );
    assert_eq!(
        b"b".to_vec(),
        std::fs::read(dst.join("b_short.txt")).unwrap()
    );
}

/// The modification times of `path` and everything under it, by relative
/// name, so a tree can be compared against its copy.
fn modified_times(root: &Path) -> Vec<(PathBuf, std::time::SystemTime)> {
    let mut times = vec![(
        PathBuf::from("."),
        fs::metadata(root).unwrap().modified().unwrap(),
    )];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let metadata = fs::metadata(&path).unwrap();
            if metadata.is_dir() {
                stack.push(path.clone());
            }
            times.push((
                path.strip_prefix(root).unwrap().to_path_buf(),
                metadata.modified().unwrap(),
            ));
        }
    }
    times.sort();
    times
}

#[test]
fn a_preserving_copy_keeps_the_modification_times_of_the_whole_tree() {
    let fx = TempDir::new("tasks_times");
    let src = fx.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("a.txt"), b"a").unwrap();
    fs::write(src.join("sub").join("b.txt"), b"b").unwrap();
    // Backdate everything, deepest first so writing a child does not move
    // the parent's time again.
    let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    for relative in ["sub/b.txt", "sub", "a.txt", "."] {
        let file = File::options().read(true).open(src.join(relative)).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(old))
            .unwrap();
    }
    let before = modified_times(&src);
    let dst = fx.join("dst");

    // A same-device move is a rename, which keeps the timestamps. The
    // cross-device fallback copies, so it has to put them back or the
    // result depends on which mount the destination is on.
    let errors = copy_one(true, &src, &dst);
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");

    assert_eq!(before, modified_times(&dst));
}

/// The outcome a name taken since the copy started must have: skipped
/// under a standing "skip all", otherwise recorded, and never replaced.
/// `nested` when it is inside a directory the copy created.
fn assert_raced(
    case: &str,
    standing: Option<ConflictChoice>,
    (is_move, nested): (bool, bool),
    (old, new): (&Path, &Path),
    (errors, skipped): (&[String], usize),
) {
    if standing == Some(ConflictChoice::SkipAll) {
        assert!(errors.is_empty(), "{case}: {errors:?}");
        assert_eq!(1, skipped, "{case}");
    } else {
        let verb = if is_move { "move" } else { "copy" };
        let reason = if nested {
            "the name is already taken there, by another entry or one the destination \
             treats as the same"
        } else {
            "another entry took that name after the paste checked it"
        };
        let refusal = format!(
            "Cannot {verb} {} to {}: {reason}",
            compact(old),
            compact(new)
        );
        assert_eq!(vec![refusal], errors, "{case}");
        assert_eq!(0, skipped, "{case}");
    }
}

/// Every kind of source against every kind of occupant, under every
/// standing answer, for a copy and for a move's copy stage.
fn raced_cases() -> Vec<(Kind, Kind, Option<ConflictChoice>, bool)> {
    kind_matrix()
        .flat_map(|(kind, occupant, standing)| {
            [false, true]
                .into_iter()
                .map(move |is_move| (kind, occupant, standing, is_move))
        })
        .collect()
}

/// A name another process took at the top-level destination after the task
/// started is never replaced, whatever the standing answer: nothing decided
/// to replace what holds it now. "Skip all" skips it, and says so for the
/// top-level entry; otherwise it is recorded.
#[test]
fn a_name_taken_at_the_top_level_since_the_copy_started_is_never_replaced() {
    every_case(raced_cases(), |&raced| {
        within_deadline(move || top_raced(raced));
    });
}

fn top_raced((kind, occupant, standing, is_move): (Kind, Kind, Option<ConflictChoice>, bool)) {
    let case = format!("{kind:?} onto {occupant:?}, {standing:?}, move {is_move}");
    let fx = TempDir::new("tasks_raced_top");
    let (src, dst) = (fx.join("src"), fx.join("dst"));
    fs::create_dir(&src).unwrap();
    fs::create_dir(&dst).unwrap();
    let (old, new) = (src.join("entry"), dst.join("entry"));
    make(kind, "src", &old);
    make(occupant, "raced", &new);
    let id = EntryId::of_path(&new);
    let conflicts = answered(standing);
    let (tx, _rx) = mpsc::channel();
    let mut active = copy_task(tx);
    let mut buffer = [0u8; 64];
    let mut context = CopyContext::new(
        CopySettings {
            is_move,
            conflicts: &conflicts,
        },
        &mut active,
        &mut buffer,
        None,
        0,
    );

    assert!(copy_path(&mut context, None, &listed(&old), &old, &new));
    let outcome = context.into_outcome();
    active.done();

    assert_raced(
        &case,
        standing,
        (is_move, false),
        (&old, &new),
        (&outcome.errors, outcome.skipped),
    );
    assert_eq!(
        standing == Some(ConflictChoice::SkipAll),
        outcome.top_skipped(),
        "{case}"
    );
    assert_made(&case, occupant, "raced", id, &new);
}

/// Copies the tree `src/tree`, with `depth` directories of `sub` below it
/// holding one `entry` of `kind`, into `dst/tree` under `standing`, with
/// `occupant` put at the entry's destination once the copy has created the
/// directory it goes in and before it copies anything into it. Returns
/// what the copy left behind and the entry's two paths.
fn copy_into_a_raced_tree(
    (kind, occupant, standing, is_move): (Kind, Kind, Option<ConflictChoice>, bool),
    depth: usize,
) -> (TempDir, CopyOutcome, PathBuf, PathBuf) {
    let fx = TempDir::new("tasks_raced_tree");
    let mut src = fx.join("src").join("tree");
    let mut dst = fx.join("dst").join("tree");
    let mut dirs = vec![(src.clone(), dst.clone())];
    for _ in 1..depth {
        src.push("sub");
        dst.push("sub");
        dirs.push((src.clone(), dst.clone()));
    }
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(fx.join("dst")).unwrap();
    make(kind, "src", &src.join("entry"));
    let conflicts = answered(standing);
    let (tx, _rx) = mpsc::channel();
    let mut active = copy_task(tx);
    let mut buffer = [0u8; 64];
    let mut context = CopyContext::new(
        CopySettings {
            is_move,
            conflicts: &conflicts,
        },
        &mut active,
        &mut buffer,
        None,
        0,
    );
    // Each directory entered in turn, as the walk enters them, down to the
    // one the entry goes in.
    let mut level = None;
    for (depth, (src_dir, dst_dir)) in dirs.iter().enumerate() {
        let (src_parent, dst_parent) =
            (open_parent(src_dir).unwrap(), open_parent(dst_dir).unwrap());
        let name = c_name(src_dir).unwrap();
        let stat = fstatat(&src_parent, name.as_c_str(), AtFlags::AT_SYMLINK_NOFOLLOW).unwrap();
        let at = At {
            src: &src_parent,
            dst: &dst_parent,
            src_name: &name,
            dst_name: &name,
        };
        let paths = Paths {
            old: src_dir.clone(),
            new: dst_dir.clone(),
            depth,
        };
        level = Some(
            enter_directory(&mut context, &at, &paths, &stat)
                .expect("the directory should be entered"),
        );
    }
    let (old, new) = (src.join("entry"), dst.join("entry"));
    make(occupant, "raced", &new);
    let mut paths = Paths {
        old: src,
        new: dst,
        depth: dirs.len() - 1,
    };

    assert!(copy_tree(
        &mut context,
        level.expect("at least one directory"),
        &mut paths,
    ));
    let outcome = context.into_outcome();
    active.done();
    (fx, outcome, old, new)
}

/// Inside a directory the copy created, at any depth, a name taken is
/// never replaced, whatever the standing answer: it may be the copy's own
/// entry under a name the destination folds together with another. "Skip
/// all" still skips it, and it is not the top-level entry; otherwise it is
/// recorded, so a move keeps its source.
#[test]
fn a_name_taken_inside_a_created_directory_is_never_replaced() {
    let cases: Vec<_> = raced_cases()
        .into_iter()
        .flat_map(|case| [1, 2].into_iter().map(move |depth| (case, depth)))
        .collect();
    every_case(cases, |&case| within_deadline(move || nested_raced(case)));
}

fn nested_raced((raced, depth): ((Kind, Kind, Option<ConflictChoice>, bool), usize)) {
    let (kind, occupant, standing, is_move) = raced;
    let case = format!("{kind:?} onto {occupant:?} at depth {depth}, {standing:?}, move {is_move}");
    let (_fx, outcome, old, new) = copy_into_a_raced_tree(raced, depth);

    assert_raced(
        &case,
        standing,
        (is_move, true),
        (&old, &new),
        (&outcome.errors, outcome.skipped),
    );
    assert!(!outcome.top_skipped(), "{case}");
    assert_made(&case, occupant, "raced", None, &new);
}

/// A move across devices whose copy skipped an entry inside the tree under
/// "skip all" keeps its whole source, from what the copy itself reports.
#[test]
fn a_move_whose_copy_skipped_an_inner_entry_keeps_its_source() {
    let raced = (Kind::File, Kind::File, Some(ConflictChoice::SkipAll), true);
    let (fx, outcome, old, _new) = copy_into_a_raced_tree(raced, 1);
    let tree = fx.join("src").join("tree");
    let (tx, rx) = mpsc::channel();

    super::super::finish_cross_device_move(copy_task(tx), outcome, &tree, &tree, true);

    assert_eq!(
        Some(format!(
            "Skipped 1 entry, so the original {} was kept",
            compact(&tree)
        )),
        finished_task(&rx).error_message()
    );
    assert!(old.exists());
}

// ── prepare_destination: the ordering every queued operation depends on ──

/// A granted overwrite whose entry still holds the name hands it to the
/// copy to replace once its replacement is whole, and removes nothing up
/// front.
#[test]
fn a_granted_overwrite_is_handed_to_the_copy_and_nothing_is_removed() {
    let (_fx, src, dst, active, _token) = destination("tasks_prepare_overwrite");
    let granted = Seen::of_path(&dst).unwrap();

    let (active, prepared) =
        prepare_destination(active, settings(false), granted, &src, &dst, mode_of(&src))
            .expect("the task should continue");

    assert_eq!(granted, prepared.replace);
    assert!(prepared.source.is_some());
    assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
    active.done();
}

/// An entry changed since the overwrite was granted is refused before
/// anything is copied, so no copy is made that could never land.
#[test_case(false ; "a copy")]
#[test_case(true ; "a move")]
fn a_granted_entry_changed_since_is_refused_before_anything_is_copied(is_move: bool) {
    let (fx, src, dst, _active, _token) = destination("tasks_prepare_changed");
    let granted = Seen::of_path(&dst).unwrap();
    // Kept under another name, so the one written next cannot reuse its
    // inode number.
    fs::rename(&dst, fx.join("kept")).unwrap();
    fs::write(&dst, b"since").unwrap();
    let (tx, rx) = mpsc::channel();

    let prepared = prepare_destination(
        copy_task(tx),
        settings(is_move),
        granted,
        &src,
        &dst,
        mode_of(&src),
    );

    assert!(prepared.is_none());
    let verb = if is_move { "move" } else { "copy" };
    assert_eq!(
        Some(format!(
            "Cannot {verb} {} to {}: the entry there changed after the paste checked it",
            compact(&src),
            compact(&dst)
        )),
        finished_task(&rx).error_message()
    );
    assert_eq!(b"since".to_vec(), fs::read(&dst).unwrap());
    assert_eq!(Vec::<String>::new(), staging_left(fx.path()));
}

#[test]
fn a_destination_survives_when_overwrite_was_not_granted() {
    let (_fx, src, dst, active, _token) = destination("tasks_prepare_no_overwrite");

    let (active, prepared) =
        prepare_destination(active, settings(false), None, &src, &dst, mode_of(&src))
            .expect("the task should continue");

    assert_eq!(None, prepared.replace);
    assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
    active.done();
}

/// Something else removed the entry between the prompt and the worker,
/// which leaves the name free for the copy to create.
#[test]
fn a_destination_that_already_vanished_is_not_an_error() {
    let (_fx, src, dst, active, _token) = destination("tasks_prepare_vanished");
    let granted = Seen::of_path(&dst).unwrap();
    fs::remove_file(&dst).unwrap();

    let (active, prepared) =
        prepare_destination(active, settings(false), granted, &src, &dst, mode_of(&src))
            .expect("the task should continue");

    assert_eq!(None, prepared.replace);
    active.done();
}

#[test]
fn an_unreadable_source_leaves_the_destination_it_would_have_replaced() {
    let (_fx, src, dst, active, _token) = destination("tasks_prepare_source_unreadable");
    fs::set_permissions(&src, fs::Permissions::from_mode(0o000)).unwrap();
    // Root reads a mode-000 file anyway (CAP_DAC_OVERRIDE), as do mounts
    // that ignore permissions; probe rather than inspect the euid.
    let is_unreadable = File::open(&src).is_err();

    let granted = Seen::of_path(&dst).unwrap();
    let prepared = prepare_destination(active, settings(false), granted, &src, &dst, mode_of(&src));

    if is_unreadable {
        assert!(prepared.is_none());
    } else {
        let (active, prepared) = prepared.expect("a readable source should continue");
        assert!(prepared.source.is_some());
        active.done();
    }
    assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
}

#[test_case(false, "copy" ; "a copy")]
#[test_case(true, "move" ; "a move")]
fn an_unreadable_source_is_reported_under_the_operation_that_failed(is_move: bool, verb: &str) {
    let fx = TempDir::new("tasks_prepare_verb");
    let src = fx.join("missing.txt");
    let dst = fx.join("dest.txt");
    let (tx, rx) = mpsc::channel();
    // A regular-file mode with no file behind it, so opening fails for
    // root as well.
    let prepared = prepare_destination(
        copy_task(tx),
        settings(is_move),
        None,
        &src,
        &dst,
        0o100_644,
    );

    assert!(prepared.is_none());
    let message = finished_task(&rx)
        .error_message()
        .expect("an error")
        .clone();
    assert!(
        message.starts_with(&format!("Failed to {verb} ")),
        "{message}"
    );
}

/// A move's file is created owner-only and gets the source's other bits
/// once the copy stops; a copy's gets no more than the source has, which
/// the umask trims. Either way a file still being written is readable by
/// no one the source is not.
#[test_case(true => 0o600 ; "a move is created owner only")]
#[test_case(false => 0 ; "a copy is created with no bit the source lacks")]
fn a_copied_file_is_never_more_readable_than_its_source_while_written(is_move: bool) -> u32 {
    let fx = TempDir::new("tasks_create_file_mode");
    let dir = open_parent(&fx.join("dst.txt")).unwrap();

    let _file = create_file_at(&dir, c"dst.txt", 0o100_640, is_move).unwrap();

    let mode = mode_of(&fx.join("dst.txt")) & 0o7777;
    if is_move { mode } else { mode & !0o640 }
}

/// A copy task already cancelled, so the walk stops at its first check.
fn cancelled_copy_task() -> ActiveTask {
    let (tx, rx) = mpsc::channel();
    std::mem::forget(rx);
    let (active, token) = copy_task_with(tx, 1);
    token.cancel();
    active
}

#[test]
fn a_cancelled_file_copy_is_left_with_the_source_mode() {
    let fx = TempDir::new("tasks_cancelled_file_mode");
    let src = fx.join("src.txt");
    fs::write(&src, b"src").unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o640)).unwrap();
    let dst = fx.join("dst.txt");
    let mut active = cancelled_copy_task();

    // Cancelled after the destination is created and before a byte is
    // written. The partial file stays, like an interrupted `cp`, and gets
    // the source's mode: neither the owner-only mode it was created with
    // nor anything broader than the source.
    assert!(!copy_path(
        &mut context(false, &mut active, &mut [0u8; 64]),
        None,
        &listed(&src),
        &src,
        &dst,
    ));
    active.cancelled();

    assert!(dst.exists());
    assert_eq!(0o640, mode_of(&dst) & 0o7777);
}

#[test]
fn a_cancelled_directory_copy_is_left_with_the_source_mode() {
    let fx = TempDir::new("tasks_cancelled_directory_mode");
    let src = fx.join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("a.txt"), b"a").unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o750)).unwrap();
    let dst = fx.join("dst");
    let mut active = cancelled_copy_task();

    // Cancelled at the first entry, after the directory is created.
    assert!(!copy_path(
        &mut context(false, &mut active, &mut [0u8; 64]),
        None,
        &listed(&src),
        &src,
        &dst,
    ));
    active.cancelled();

    assert!(!dst.join("a.txt").exists());
    assert_eq!(0o750, mode_of(&dst) & 0o7777);
}

// ── a replacement lands whole or not at all ──────────────────────────────

/// A replacement that cannot be created leaves the entry it was to
/// replace, and nothing of itself: a device node needs privileges a user
/// does not have, so creating one fails after the copy began.
#[test]
fn a_replacement_that_cannot_be_created_leaves_the_entry_it_would_have_replaced() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipped: root may create a device node");
        return;
    }
    let fx = TempDir::new("tasks_replace_cannot_create");
    let new = fx.join("null");
    fs::write(&new, b"dest").unwrap();

    let (_, task) = paste_over(false, fx.path(), Path::new("/dev/null"));

    assert_eq!(
        Some(format!(
            "Failed to copy \"/dev/null\" to {}: Operation not permitted (os error 1); {} was \
             not replaced",
            compact(&new),
            compact(&new)
        )),
        task.error_message()
    );
    assert_eq!(b"dest".to_vec(), fs::read(&new).unwrap());
    assert_eq!(Vec::<String>::new(), staging_left(fx.path()));
}

/// A replacement cancelled part way leaves the entry it was to replace,
/// and nothing of itself.
#[test]
fn a_cancelled_replacement_leaves_the_entry_it_would_have_replaced() {
    let (fx, src, dst, active, _token) = destination("tasks_replace_cancelled");
    active.done();
    let mut active = cancelled_copy_task();
    let mut buffer = [0u8; 64];
    let mut context = context(false, &mut active, &mut buffer);

    let finished = copy_path(&mut context, seen(&dst), &listed(&src), &src, &dst);
    let errors = context.into_outcome().errors;
    active.cancelled();

    assert!(!finished);
    assert_eq!(Vec::<String>::new(), errors);
    assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
    assert_eq!(Vec::<String>::new(), staging_left(fx.path()));
}

/// The entry granted is checked again just before the replacement takes
/// its name: one changed while the replacement was written is refused
/// and left as it is, the replacement is removed, and nothing counts as
/// written. An entry unchanged is replaced, and that counts.
///
/// An entry removed meanwhile leaves the name free, which the replacement
/// takes without replacing anything.
#[test_case(false, Landed::Unchanged ; "a copy over the entry granted")]
#[test_case(true, Landed::Unchanged ; "a move over the entry granted")]
#[test_case(false, Landed::Changed ; "a copy over an entry changed since")]
#[test_case(true, Landed::Changed ; "a move over an entry changed since")]
#[test_case(false, Landed::Removed ; "a copy onto a name freed since")]
#[test_case(true, Landed::Removed ; "a move onto a name freed since")]
fn a_replacement_lands_only_on_the_entry_granted(is_move: bool, since: Landed) {
    let (fx, src, dst, active, _token) = destination("tasks_replace_lands_granted");
    active.done();
    let granted = seen(&dst);
    let changed = since == Landed::Changed;
    match since {
        Landed::Unchanged => {}
        // Kept under another name, so the one written next cannot reuse
        // its inode number.
        Landed::Changed => {
            fs::rename(&dst, fx.join("kept")).unwrap();
            fs::write(&dst, b"since").unwrap();
        }
        Landed::Removed => fs::remove_file(&dst).unwrap(),
    }
    let (tx, _rx) = mpsc::channel();
    let mut active = copy_task(tx);
    let mut buffer = [0u8; 64];
    let mut context = context(is_move, &mut active, &mut buffer);

    assert!(copy_path(&mut context, granted, &listed(&src), &src, &dst));
    let outcome = context.into_outcome();
    active.done();

    if changed {
        let verb = if is_move { "move" } else { "copy" };
        assert_eq!(
            vec![format!(
                "Cannot {verb} {} to {}: the entry there changed after the paste checked it",
                compact(&src),
                compact(&dst)
            )],
            outcome.errors
        );
        assert_eq!(b"since".to_vec(), fs::read(&dst).unwrap());
    } else {
        assert_eq!(Vec::<String>::new(), outcome.errors);
        assert_eq!(b"src".to_vec(), fs::read(&dst).unwrap());
    }
    assert_eq!(!changed, outcome.wrote);
    assert_eq!(Vec::<String>::new(), staging_left(fx.path()));
}

/// A name taken between the last check and the rename that lands a
/// replacement is refused as raced, never counted as landed, and skipped
/// under a standing "skip all"; any other failure of that rename is one,
/// which says the entry granted was left only when the rename was the one
/// replacing it.
#[test_case(false ; "a copy")]
#[test_case(true ; "a move")]
fn what_the_rename_that_lands_a_replacement_means(is_move: bool) {
    let mut active = idle_task();
    let mut buffer = [0u8; 8];
    let mut context = context(is_move, &mut active, &mut buffer);
    let paths = Paths {
        old: PathBuf::from("/src/one"),
        new: PathBuf::from("/dest/one"),
        depth: 0,
    };
    let verb = if is_move { "move" } else { "copy" };
    let taken = || Err(ErrorKind::AlreadyExists.into());
    let denied = || Err(ErrorKind::PermissionDenied.into());

    assert_eq!(Ok(true), landed(&mut context, &paths, true, Ok(())));
    assert_eq!(
        Err(format!(
            "Cannot {verb} \"/src/one\" to \"/dest/one\": another entry took that name after \
             the paste checked it"
        )),
        landed(&mut context, &paths, false, taken())
    );
    assert_eq!(
        Err(format!(
            "Failed to {verb} \"/src/one\" to \"/dest/one\": permission denied; \
             \"/dest/one\" was not replaced"
        )),
        landed(&mut context, &paths, true, denied())
    );
    assert_eq!(
        Err(format!(
            "Failed to {verb} \"/src/one\" to \"/dest/one\": permission denied"
        )),
        landed(&mut context, &paths, false, denied()),
        "a rename onto a free name leaves nothing granted behind"
    );
    assert_eq!(0, context.skipped);
}

/// Under a standing "skip all" a name taken just before the landing is
/// skipped, as a raced name anywhere else is: counted, not an error.
#[test]
fn a_landing_that_finds_the_name_taken_is_skipped_under_skip_all() {
    let conflicts = answered(Some(ConflictChoice::SkipAll));
    let mut active = idle_task();
    let mut buffer = [0u8; 8];
    let mut context = CopyContext::new(
        CopySettings {
            is_move: false,
            conflicts: &conflicts,
        },
        &mut active,
        &mut buffer,
        None,
        0,
    );
    let paths = Paths {
        old: PathBuf::from("/src/one"),
        new: PathBuf::from("/dest/one"),
        depth: 0,
    };

    let landed = landed(
        &mut context,
        &paths,
        false,
        Err(ErrorKind::AlreadyExists.into()),
    );

    assert_eq!(Ok(false), landed);
    assert_eq!(1, context.skipped);
}

/// What holds a granted name by the time the replacement lands.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Landed {
    Unchanged,
    Changed,
    Removed,
}

/// Landing a staged replacement: over what holds the name only when told
/// to, onto a free name, and never over a name taken since, which is
/// refused with the staged entry and the occupant both left as they are.
#[test_case(true, true => (Ok(()), "staged".to_string(), false) ; "replacing what holds the name")]
#[test_case(false, false => (Ok(()), "staged".to_string(), false) ; "onto a free name")]
#[test_case(false, true => (Err(ErrorKind::AlreadyExists), "taken".to_string(), true) ; "onto a taken name without replacing")]
fn a_staged_replacement_lands(
    replaces: bool,
    taken: bool,
) -> (Result<(), ErrorKind>, String, bool) {
    let fx = TempDir::new("tasks_rename_staged");
    let (staging, dst) = (fx.join("staging"), fx.join("dst"));
    fs::create_dir(&staging).unwrap();
    fs::create_dir(&dst).unwrap();
    fs::write(staging.join("entry"), b"staged").unwrap();
    if taken {
        fs::write(dst.join("entry"), b"taken").unwrap();
    }
    let open = |dir: &Path| File::open(dir).unwrap();

    let landed = rename_staged(replaces, &open(&staging), &open(&dst), c"entry")
        .map_err(|error| error.kind());

    (
        landed,
        String::from_utf8(fs::read(dst.join("entry")).unwrap()).unwrap(),
        staging.join("entry").exists(),
    )
}

/// A replacement that lands leaves nothing of its staging behind, and the
/// destination holds the whole source, for a copy and for a move.
#[test_case(false ; "a copy")]
#[test_case(true ; "a move")]
fn a_replacement_that_lands_leaves_no_staging_behind(is_move: bool) {
    let (_fx, src, dest) = src_and_dest("tasks_replace_lands");
    fs::write(src.join("one"), b"source").unwrap();
    fs::write(dest.join("one"), b"dest").unwrap();

    let (new, task) = paste_over(is_move, &dest, &src.join("one"));

    assert_eq!(None, task.error_message());
    assert_eq!(b"source".to_vec(), fs::read(&new).unwrap());
    assert_eq!(!is_move, src.join("one").exists());
    assert_eq!(Vec::<String>::new(), staging_left(&dest));
}

/// A destination whose modification time is in the future (an archive
/// extracted with such times, a clock stepped back) still takes a
/// replacement: the staging directory is checked against the change
/// time, which nobody can set.
#[test]
fn a_destination_modified_in_the_future_takes_a_replacement() {
    let fx = TempDir::new("tasks_replace_future");
    let (src, dest) = (fx.join("src"), fx.join("dest"));
    fs::create_dir(&dest).unwrap();
    fs::write(&src, b"source").unwrap();
    fs::write(dest.join("src"), b"dest").unwrap();
    let ahead = std::time::SystemTime::now() + Duration::from_hours(1);
    File::open(&dest).unwrap().set_modified(ahead).unwrap();

    let (new, task) = paste_over(false, &dest, &src);

    assert_eq!(None, task.error_message());
    assert_eq!(b"source".to_vec(), fs::read(&new).unwrap());
    assert_eq!(Vec::<String>::new(), staging_left(&dest));
}

/// A staging directory takes the first name offered that is free, and
/// never touches an entry that already holds one.
#[test]
fn a_staging_directory_never_takes_a_name_already_there() {
    let fx = TempDir::new("tasks_staging_names");
    fs::write(fx.join("taken"), b"keep").unwrap();
    let parent = open_parent(&fx.join("x")).unwrap();

    let staging = Staging::create(
        &parent,
        fx.path(),
        [c"taken".to_owned(), c"free".to_owned()],
        c"entry",
    )
    .unwrap();

    assert_eq!(c"free", staging.name.as_c_str());
    assert_eq!(fx.join("free"), staging.path);
    assert!(fx.join("free").is_dir());
    drop(staging);
    assert!(!fx.join("free").exists());
    assert_eq!(b"keep".to_vec(), fs::read(fx.join("taken")).unwrap());
}

/// An entry in the staging directory the copy did not put there is never
/// taken for the copy, landed, or removed, whatever the standing answer:
/// the copy is refused, and the entry granted is left as it was.
#[test_case(None ; "with no standing answer")]
#[test_case(Some(ConflictChoice::SkipAll) ; "under skip all")]
fn an_entry_planted_in_the_staging_directory_is_never_landed_or_removed(
    standing: Option<ConflictChoice>,
) {
    let fx = TempDir::new("tasks_staging_planted");
    let (src, dst) = (fx.join("src"), fx.join("dst"));
    fs::write(&src, b"src").unwrap();
    fs::write(&dst, b"dest").unwrap();
    let granted = seen(&dst).unwrap();
    let parent = File::open(fx.path()).unwrap();
    let staging = Staging::create(&parent, fx.path(), [c"s".to_owned()], c"dst").unwrap();
    fs::write(fx.join("s").join("dst"), b"planted").unwrap();
    let conflicts = answered(standing);
    let (tx, _rx) = mpsc::channel();
    let mut active = copy_task(tx);
    let mut buffer = [0u8; 64];
    let mut context = CopyContext::new(
        CopySettings {
            is_move: false,
            conflicts: &conflicts,
        },
        &mut active,
        &mut buffer,
        None,
        0,
    );
    let stat = fstatat(&parent, c"src", AtFlags::AT_SYMLINK_NOFOLLOW).unwrap();
    let at = At {
        src: &parent,
        dst: &parent,
        src_name: c"src",
        dst_name: c"dst",
    };
    let paths = Paths {
        old: src.clone(),
        new: dst.clone(),
        depth: 0,
    };

    let finished = replace_through(
        &mut context,
        Replacement { granted, staging },
        &at,
        &paths,
        &stat,
    );
    let outcome = context.into_outcome();
    active.done();

    assert!(finished);
    assert_eq!(
        vec![format!(
            "Cannot copy {} to {}: its staging directory was changed; {} was not replaced",
            compact(&src),
            compact(&dst),
            compact(&dst)
        )],
        outcome.errors
    );
    assert_eq!(0, outcome.skipped);
    assert_eq!(
        b"planted".to_vec(),
        fs::read(fx.join("s").join("dst")).unwrap()
    );
    assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
}

/// The entry the copy staged, replaced in the staging directory before it
/// lands, is refused: what took its place is neither landed nor removed.
#[test]
fn a_staged_entry_replaced_before_it_lands_is_refused() {
    let fx = TempDir::new("tasks_staging_replaced_entry");
    let dest = fx.join("d");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("one"), b"granted").unwrap();
    let granted = seen(&dest.join("one")).unwrap();
    let handle = File::open(&dest).unwrap();
    let staging = staged_beside(&handle, &dest, b"staged");
    // Written first under another name, so it cannot reuse the staged
    // entry's inode number.
    fs::write(dest.join("s").join("other"), b"planted").unwrap();
    fs::rename(dest.join("s").join("other"), dest.join("s").join("one")).unwrap();
    let mut active = idle_task();
    let mut buffer = [0u8; 8];
    let mut context = context(false, &mut active, &mut buffer);
    let at = At {
        src: &handle,
        dst: &handle,
        src_name: c"one",
        dst_name: c"one",
    };
    let paths = Paths {
        old: fx.join("src"),
        new: dest.join("one"),
        depth: 0,
    };

    let landed = land(&mut context, &staging, granted, &at, &paths);
    let removed = staging.remove();

    assert_eq!(
        Err(format!(
            "Cannot copy {} to {}: its staging directory was changed; {} was not replaced",
            compact(&fx.join("src")),
            compact(&dest.join("one")),
            compact(&dest.join("one"))
        )),
        landed
    );
    assert_eq!(
        Some(nix::libc::ENOTEMPTY),
        removed.unwrap_err().raw_os_error()
    );
    assert_eq!(
        b"planted".to_vec(),
        fs::read(dest.join("s").join("one")).unwrap()
    );
    assert_eq!(b"granted".to_vec(), fs::read(dest.join("one")).unwrap());
}

/// An empty directory swapped in at the staging name after the staging
/// directory was made is not removed in its place.
#[test]
fn an_empty_directory_at_the_staging_name_since_is_not_removed() {
    let fx = TempDir::new("tasks_staging_empty_swapped");
    let parent = open_parent(&fx.join("x")).unwrap();
    let staging = Staging::create(&parent, fx.path(), [c"s".to_owned()], c"entry").unwrap();
    fs::rename(fx.join("s"), fx.join("moved")).unwrap();
    fs::create_dir(fx.join("s")).unwrap();

    let removed = staging.remove();

    assert_eq!(
        "another entry holds its name, and was left alone",
        removed.unwrap_err().to_string()
    );
    assert!(fx.join("s").is_dir());
}

/// Owner-only, so what is staged is never reachable by others before it
/// lands.
#[test]
fn a_staging_directory_is_owner_only() {
    let fx = TempDir::new("tasks_staging_mode");
    let parent = open_parent(&fx.join("x")).unwrap();

    let staging = Staging::create(&parent, fx.path(), [c"s".to_owned()], c"entry").unwrap();

    assert_eq!(0o700, mode_of(&fx.join("s")) & 0o7777);
    drop(staging);
}

/// A staging directory that is dropped removes the entry staged in it and
/// then itself.
#[test]
fn a_dropped_staging_directory_takes_its_entry_with_it() {
    let fx = TempDir::new("tasks_staging_drop");
    let parent = open_parent(&fx.join("x")).unwrap();
    let mut staging = Staging::create(&parent, fx.path(), [c"s".to_owned()], c"entry").unwrap();
    stage(&mut staging, b"staged");

    drop(staging);

    assert!(!fx.join("s").exists());
}

/// Removal goes through the staging directory's own handle, then by name
/// only for an empty directory: one planted at its name since, holding an
/// entry of the same name, is left alone.
#[test]
fn a_staging_directory_swapped_since_is_not_emptied() {
    let fx = TempDir::new("tasks_staging_swapped");
    let parent = open_parent(&fx.join("x")).unwrap();
    let mut staging = Staging::create(&parent, fx.path(), [c"s".to_owned()], c"entry").unwrap();
    stage(&mut staging, b"staged");
    fs::rename(fx.join("s"), fx.join("moved")).unwrap();
    fs::create_dir(fx.join("s")).unwrap();
    fs::write(fx.join("s").join("entry"), b"planted").unwrap();

    drop(staging);

    assert_eq!(
        b"planted".to_vec(),
        fs::read(fx.join("s").join("entry")).unwrap()
    );
    assert!(!fx.join("moved").join("entry").exists());
}

/// Every name offered is another, within a call and across calls, so no
/// two replacements can meet in one staging directory.
#[test]
fn staging_names_are_never_offered_twice() {
    let first: Vec<CString> = staging_names().collect();
    let second: Vec<CString> = staging_names().collect();
    let all: std::collections::HashSet<&CString> = first.iter().chain(&second).collect();

    assert_eq!(first.len() + second.len(), all.len());
    assert_eq!(STAGING_ATTEMPTS, u64::try_from(first.len()).unwrap());
    let pid = format!(".filectrl-{}-", std::process::id());
    assert!(
        all.iter()
            .all(|name| name.to_str().unwrap().starts_with(&pid)),
        "{all:?}"
    );
}

/// Every name offered taken: nothing is created and nothing is touched.
#[test]
fn a_staging_directory_with_no_free_name_is_refused() {
    let fx = TempDir::new("tasks_staging_none");
    fs::write(fx.join("taken"), b"keep").unwrap();
    let parent = open_parent(&fx.join("x")).unwrap();

    let error = Staging::create(&parent, fx.path(), [c"taken".to_owned()], c"entry")
        .err()
        .expect("no name was free");

    assert_eq!(ErrorKind::AlreadyExists, error.kind());
    // No errno: filectrl's own refusal, which `replace_entry` words so.
    assert_eq!(None, error.raw_os_error());
    assert_eq!("no free name for a staging directory", error.to_string());
    assert_eq!(b"keep".to_vec(), fs::read(fx.join("taken")).unwrap());
}

/// A staging directory is removed from the directory it was made in,
/// through that directory's handle, even once that directory has been
/// renamed and another made at its old name holds one of the same name.
#[test_case(false ; "removed")]
#[test_case(true ; "dropped")]
fn a_staging_directory_is_removed_from_the_directory_it_was_made_in(dropped: bool) {
    let fx = TempDir::new("tasks_staging_parent_moved");
    let made_in = fx.join("p");
    fs::create_dir(&made_in).unwrap();
    let parent = File::open(&made_in).unwrap();
    let staging = Staging::create(&parent, &made_in, [c"s".to_owned()], c"entry").unwrap();
    fs::rename(&made_in, fx.join("q")).unwrap();
    fs::create_dir_all(made_in.join("s")).unwrap();

    if dropped {
        drop(staging);
    } else {
        staging.remove().unwrap();
    }

    assert!(!fx.join("q").join("s").exists());
    assert!(made_in.join("s").is_dir());
}

/// Writes `contents` as the entry of `staging`, recorded as the one the
/// copy made, as `copy_entry` records it.
fn stage(staging: &mut Staging<'_>, contents: &[u8]) {
    let entry = staging
        .path
        .join(OsStr::from_bytes(staging.entry.to_bytes()));
    fs::write(&entry, contents).unwrap();
    staging.staged = EntryId::of_path(&entry);
}

/// A staging directory with `staged` written as its entry, `one`, beside
/// `dir`'s own `one`, for `land`.
fn staged_beside<'a>(dir: &'a File, dir_path: &Path, staged: &[u8]) -> Staging<'a> {
    let mut staging = Staging::create(dir, dir_path, [c"s".to_owned()], c"one").unwrap();
    stage(&mut staging, staged);
    staging
}

/// `land` looks at the name and renames onto it through the directory
/// handle it is given: the directory renamed away and another made at its
/// path is not what it lands in.
#[test]
fn a_landing_goes_through_the_directory_it_was_given() {
    let fx = TempDir::new("tasks_land_handle");
    let dest = fx.join("d");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("one"), b"granted").unwrap();
    let granted = seen(&dest.join("one")).unwrap();
    let handle = File::open(&dest).unwrap();
    let staging = staged_beside(&handle, &dest, b"staged");
    fs::rename(&dest, fx.join("moved")).unwrap();
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("one"), b"elsewhere").unwrap();
    let mut active = idle_task();
    let mut buffer = [0u8; 8];
    let mut context = context(false, &mut active, &mut buffer);
    let at = At {
        src: &handle,
        dst: &handle,
        src_name: c"one",
        dst_name: c"one",
    };
    let paths = Paths {
        old: fx.join("src"),
        new: dest.join("one"),
        depth: 0,
    };

    assert_eq!(Ok(true), land(&mut context, &staging, granted, &at, &paths));
    staging.remove().unwrap();

    assert_eq!(
        b"staged".to_vec(),
        fs::read(fx.join("moved").join("one")).unwrap()
    );
    assert_eq!(b"elsewhere".to_vec(), fs::read(dest.join("one")).unwrap());
}

/// A landing whose rename fails for another reason than a taken name is
/// a failure that leaves the entry granted, and says so. The staging
/// directory can then not be removed either, which `remove` reports.
#[test]
fn a_landing_that_fails_leaves_the_entry_and_says_so() {
    let fx = TempDir::new("tasks_land_fails");
    let dest = fx.join("d");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("one"), b"granted").unwrap();
    let granted = seen(&dest.join("one")).unwrap();
    let handle = File::open(&dest).unwrap();
    let staging = staged_beside(&handle, &dest, b"staged");
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(dest.join("probe"), b"").is_ok() {
        eprintln!("skipped: a directory without write permission can be written here");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let mut active = idle_task();
    let mut buffer = [0u8; 8];
    let mut context = context(false, &mut active, &mut buffer);
    let at = At {
        src: &handle,
        dst: &handle,
        src_name: c"one",
        dst_name: c"one",
    };
    let paths = Paths {
        old: fx.join("src"),
        new: dest.join("one"),
        depth: 0,
    };

    let landed = land(&mut context, &staging, granted, &at, &paths);
    let removed = staging.remove();
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(
        Err(format!(
            "Failed to copy {} to {}: Permission denied (os error 13); {} was not replaced",
            compact(&fx.join("src")),
            compact(&dest.join("one")),
            compact(&dest.join("one"))
        )),
        landed
    );
    assert_eq!(Some(nix::libc::EACCES), removed.unwrap_err().raw_os_error());
    assert_eq!(b"granted".to_vec(), fs::read(dest.join("one")).unwrap());
}

/// A symlink at the name of a directory being given access is never
/// followed: the open is refused, the link's target keeps its mode,
/// whether it is a directory or a file, and so does a mode change by name
/// (which only ever acts on a node the copy just made).
#[test_case(true ; "a link to a directory")]
#[test_case(false ; "a link to a file")]
fn giving_a_directory_access_never_follows_a_symlink(to_directory: bool) {
    let fx = TempDir::new("tasks_access_link");
    let (target, mode) = if to_directory {
        let target = fx.join("dir");
        fs::create_dir(&target).unwrap();
        // Without owner write, so owner access given through the link
        // would show.
        (target, 0o555)
    } else {
        let target = fx.join("file");
        fs::write(&target, b"f").unwrap();
        (target, 0o600)
    };
    fs::set_permissions(&target, fs::Permissions::from_mode(mode)).unwrap();
    std::os::unix::fs::symlink(&target, fx.join("link")).unwrap();
    let parent = File::open(fx.path()).unwrap();

    let opened = open_created(&parent, c"link");
    let set = set_mode_at(&parent, c"link", 0o777);

    assert!(opened.is_err());
    // Linux refuses a mode change on a symlink; macOS changes the link's
    // own mode. Neither reaches the target.
    #[cfg(target_os = "linux")]
    assert_eq!(Some(nix::libc::EOPNOTSUPP), set.unwrap_err().raw_os_error());
    #[cfg(not(target_os = "linux"))]
    let _ = set;
    assert_eq!(mode, mode_of(&target) & 0o7777);
}

/// A new directory the owner cannot read stays unreadable: it is
/// reported, and nothing is changed by name.
#[test]
fn an_unreadable_new_directory_is_reported() {
    let fx = TempDir::new("tasks_access_unheld");
    fs::create_dir(fx.join("dir")).unwrap();
    fs::set_permissions(fx.join("dir"), fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(fx.join("dir")).is_ok() {
        eprintln!("skipped: a directory without permissions can be read here");
        fs::set_permissions(fx.join("dir"), fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }
    let parent = File::open(fx.path()).unwrap();

    let opened = open_created(&parent, c"dir");

    let mode = mode_of(&fx.join("dir")) & 0o7777;
    fs::set_permissions(fx.join("dir"), fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        Some(nix::libc::EACCES),
        opened.err().and_then(|e| e.raw_os_error())
    );
    assert_eq!(0o000, mode);
}

/// A landing that cannot look at the name (a directory without search
/// permission) cannot tell what holds it, so the entry granted may still
/// be there, and the message says it was not replaced.
#[test]
fn a_landing_that_cannot_look_at_the_name_says_the_entry_was_left() {
    let fx = TempDir::new("tasks_land_unsearchable");
    let dest = fx.join("d");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("one"), b"granted").unwrap();
    let granted = seen(&dest.join("one")).unwrap();
    let handle = File::open(&dest).unwrap();
    let staging = staged_beside(&handle, &dest, b"staged");
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o444)).unwrap();
    if fs::symlink_metadata(dest.join("one")).is_ok() {
        eprintln!("skipped: a directory without search permission can be searched here");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let mut active = idle_task();
    let mut buffer = [0u8; 8];
    let mut context = context(true, &mut active, &mut buffer);
    let at = At {
        src: &handle,
        dst: &handle,
        src_name: c"one",
        dst_name: c"one",
    };
    let paths = Paths {
        old: fx.join("src"),
        new: dest.join("one"),
        depth: 0,
    };

    let landed = land(&mut context, &staging, granted, &at, &paths);
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
    staging.remove().unwrap();

    assert_eq!(
        Err(format!(
            "Failed to move {} to {}: Permission denied (os error 13); {} was not replaced",
            compact(&fx.join("src")),
            compact(&dest.join("one")),
            compact(&dest.join("one"))
        )),
        landed
    );
    assert_eq!(b"granted".to_vec(), fs::read(dest.join("one")).unwrap());
}

/// A staging directory that cannot be removed is reported to the user as
/// a warning naming where it was left, not only logged.
#[test]
fn a_staging_directory_that_cannot_be_removed_is_reported() {
    let fx = TempDir::new("tasks_staging_left");
    let dest = fx.join("d");
    fs::create_dir(&dest).unwrap();
    let handle = File::open(&dest).unwrap();
    let staging = staged_beside(&handle, &dest, b"staged");
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(dest.join("probe"), b"").is_ok() {
        eprintln!("skipped: a directory without write permission can be written here");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let (tx, rx) = mpsc::channel();
    let active = copy_task(tx);
    while rx.try_recv().is_ok() {}

    clear_staging(&active, staging);
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
    active.done();

    let warnings: Vec<String> = rx
        .try_iter()
        .filter_map(|command| match command {
            Command::AlertWarn(message) => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(
        vec![format!(
            "Failed to remove the staging directory {}: Permission denied (os error 13)",
            compact(&dest.join("s"))
        )],
        warnings
    );
    assert!(dest.join("s").exists(), "it is where the warning says");
}

#[test]
fn a_vanished_source_leaves_the_destination_it_would_have_replaced() {
    let (_fx, src, dst, active, _token) = destination("tasks_prepare_source_gone");
    fs::remove_file(&src).unwrap();

    // A task can wait behind a long operation on the shared worker, so the
    // source it was going to copy may be gone by the time it runs.
    let granted = Seen::of_path(&dst).unwrap();
    let prepared = prepare_destination(active, settings(false), granted, &src, &dst, 0o100_644);

    assert!(prepared.is_none());
    assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
}

/// A granted destination that cannot be looked at (its directory has no
/// search permission) may still hold the entry granted, so the failure
/// says it was not replaced, and nothing is opened or copied.
#[test]
fn a_granted_destination_that_cannot_be_looked_at_says_it_was_left() {
    let fx = TempDir::new("tasks_prepare_unsearchable");
    let (src, dest) = (fx.join("src.txt"), fx.join("dest"));
    fs::write(&src, b"src").unwrap();
    fs::create_dir(&dest).unwrap();
    let target = dest.join("one");
    fs::write(&target, b"granted").unwrap();
    let granted = seen(&target);
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o444)).unwrap();
    if fs::symlink_metadata(&target).is_ok() {
        eprintln!("skipped: a directory without search permission can be searched here");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let (tx, rx) = mpsc::channel();

    let prepared = prepare_destination(
        copy_task(tx),
        settings(false),
        granted,
        &src,
        &target,
        mode_of(&src),
    );
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(prepared.is_none());
    assert_eq!(
        Some(format!(
            "Failed to copy {} to {}: Permission denied (os error 13); {} was not replaced",
            compact(&src),
            compact(&target),
            compact(&target)
        )),
        finished_task(&rx).error_message()
    );
    assert_eq!(b"granted".to_vec(), fs::read(&target).unwrap());
}

#[test]
fn dir_total_size_sums_the_files_of_the_whole_tree_but_not_its_symlinks() {
    let fx = TempDir::new("tasks");
    let root = fx.join("tree");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("a.txt"), b"abc").unwrap();
    fs::write(root.join("sub").join("b.txt"), b"defgh").unwrap();
    // Recreated as a link, so no bytes are transferred for it.
    std::os::unix::fs::symlink(root.join("a.txt"), root.join("link")).unwrap();
    let (tx, _rx) = mpsc::channel();
    let active = copy_task(tx);

    assert_eq!(Some(8), dir_total_size(&active, &root));
    active.done();
}

#[test]
fn copy_path_advances_progress_from_the_bytes_written() {
    let fx = TempDir::new("tasks_copy_progress");
    let src = fx.join("src.bin");
    fs::write(&src, [7u8; 200]).unwrap();
    let (tx, rx) = mpsc::channel();
    let (mut active, _) = copy_task_with(tx, 200);

    assert!(copy_path(
        &mut context(false, &mut active, &mut [0u8; 64]),
        None,
        &listed(&src),
        &src,
        &fx.join("dst.bin"),
    ));
    active.done();
    let completed: Vec<u64> = rx
        .try_iter()
        .filter_map(|command| match command {
            Command::Progress(task) if !task.is_terminal() => Some(task.progress().completed),
            _ => None,
        })
        .collect();

    // One 64-byte chunk is the first update, before `done` fills the bar,
    // and each later update reports more than the last.
    assert_eq!(Some(&64), completed.first());
    assert!(completed.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn a_tree_of_small_files_sends_progress_per_share_of_the_total_not_per_file() {
    const FILES: usize = 1000;
    let fx = TempDir::new("tasks_copy_tree_progress");
    let src = fx.join("src");
    fs::create_dir(&src).unwrap();
    for i in 0..FILES {
        fs::write(src.join(i.to_string()), [7u8; 10]).unwrap();
    }
    let (tx, rx) = mpsc::channel();
    let active = copy_task(tx);

    let start = Instant::now();
    let (active, outcome) = copy_with_progress(
        settings(false),
        None,
        &PathInfo::try_from(src.as_path()).unwrap(),
        active,
        &src,
        &fx.join("dst"),
    )
    .expect("not cancelled");
    let elapsed = start.elapsed();
    active.done();
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    let updates: Vec<Progress> = rx
        .try_iter()
        .filter_map(|command| match command {
            Command::Progress(task) if !task.is_terminal() => Some(task.progress().clone()),
            _ => None,
        })
        .collect();

    // `set_total` sends one, the first chunk another, and the floor admits
    // one more per interval elapsed. One per file would be a thousand.
    let bound = 2 + elapsed.as_millis() / PROGRESS_MIN_INTERVAL.as_millis();
    assert!(
        (updates.len() as u128) <= bound,
        "{} updates in {elapsed:?} for {FILES} files",
        updates.len()
    );
    // The share is of the tree's bytes, so every update reports that total,
    // and the first chunk counts toward it rather than filling the bar.
    let total = FILES as u64 * 10;
    assert!(
        updates.iter().all(|progress| progress.total == total),
        "{updates:?}"
    );
    let [first, chunk, ..] = updates.as_slice() else {
        panic!("expected the total and the first chunk, got {updates:?}");
    };
    assert_eq!(0, first.completed);
    assert_eq!(10, chunk.completed);
    // The copy finishes inside the floor, so the updates above cannot tell
    // a share of the total from none at all. The threshold can.
    assert_eq!(
        total * PROGRESS_DEBOUNCE_PERCENTAGE / 100,
        outcome.progress_threshold
    );
    assert_ne!(0, outcome.progress_threshold);
}

#[test]
fn copy_path_recreates_a_symlink_without_following_it() {
    let fx = TempDir::new("tasks");
    let target = fx.join("target.txt");
    std::fs::write(&target, b"hello").unwrap();
    std::fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();

    let link = fx.join("link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let dst = fx.join("copied_link.txt");

    assert!(unix_mode::is_symlink(mode_of(&link)));

    let errors = copy_one(false, &link, &dst);
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");

    // The destination must itself be a symlink pointing at the same target,
    // not a regular file containing the target's bytes.
    let dst_meta = fs::symlink_metadata(&dst).unwrap();
    assert!(dst_meta.is_symlink(), "destination must be a symlink");
    assert_eq!(fs::read_link(&dst).unwrap(), target);

    // The link's target must be untouched: copy must not chmod through the
    // link or rewrite its contents.
    let target_mode = fs::symlink_metadata(&target).unwrap().permissions().mode() & 0o7777;
    assert_eq!(target_mode, 0o600, "copy must not chmod the symlink target");
    assert_eq!(std::fs::read(&target).unwrap(), b"hello");
}

/// The permission bits the umask leaves of `0o777`. Probed rather than
/// read, since reading the umask means setting it for every thread.
fn umask_leaves(dir: &Path) -> u32 {
    use std::os::unix::fs::OpenOptionsExt;
    let probe = dir.join("umask_probe");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o777)
        .open(&probe)
        .unwrap();
    let leaves = mode_of(&probe) & 0o777;
    fs::remove_file(&probe).unwrap();
    leaves
}

// A copy is `cp` without `-p`: the umask applies and the special bits go,
// so a file copied where others can reach it is not a setuid program of
// the user who copied it. A move is `mv`, which keeps the mode.
#[test_case(0o4755, false ; "a copy drops setuid")]
#[test_case(0o2755, false ; "a copy drops setgid")]
#[test_case(0o777, false ; "a copy takes the umask")]
#[test_case(0o4755, true ; "a move keeps setuid on the user's own file")]
#[test_case(0o777, true ; "a move ignores the umask")]
fn a_copied_file_keeps_the_special_bits_and_ignores_the_umask_only_when_moved(
    mode: u32,
    is_move: bool,
) {
    let fx = TempDir::new("tasks_file_mode");
    let src = fx.join("src");
    fs::write(&src, b"x").unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(mode)).unwrap();

    let errors = copy_one(is_move, &src, &fx.join("dst"));

    assert!(errors.is_empty(), "{errors:?}");
    let expected = if is_move {
        mode
    } else {
        mode & 0o777 & umask_leaves(fx.path())
    };
    assert_eq!(expected, mode_of(&fx.join("dst")) & 0o7777);
}

/// Run by `a_copy_takes_the_umask_on_the_owner_bits_too` under a umask
/// that clears owner bits; on its own it proves nothing. The fixture is
/// made before the umask is set, so the directory can still be written.
/// The directory's child can only be created if the copy gives the
/// directory owner access while filling it.
#[test]
#[ignore = "run under a umask of 0o277 by the test below"]
fn a_copy_under_a_umask_clearing_owner_bits() {
    if !crate::test_support::alone() {
        return;
    }
    let fx = TempDir::new("tasks_file_mode_umask");
    let src = fx.join("src");
    fs::write(&src, b"x").unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o777)).unwrap();
    let src_dir = fx.join("src_dir");
    fs::create_dir(&src_dir).unwrap();
    fs::write(src_dir.join("child"), b"x").unwrap();
    fs::set_permissions(&src_dir, fs::Permissions::from_mode(0o777)).unwrap();
    let replaced = fx.join("replaced");
    fs::create_dir(&replaced).unwrap();
    fs::write(replaced.join("src"), b"old").unwrap();
    nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o277));

    let errors = copy_one(false, &src, &fx.join("dst"));
    let dir_errors = copy_one(false, &src_dir, &fx.join("dst_dir"));
    // A replacement is staged in a directory the umask leaves without
    // owner write, which it has to give back to write into it.
    let (new, task) = paste_over(false, &replaced, &src);

    let leaves = 0o777 & umask_leaves(fx.path());
    let dir_mode = mode_of(&fx.join("dst_dir")) & 0o7777;
    // Given back before anything is asserted, so a failure still lets the
    // fixture be removed.
    fs::set_permissions(fx.join("dst_dir"), fs::Permissions::from_mode(0o700)).unwrap();

    assert!(errors.is_empty(), "{errors:?}");
    assert!(dir_errors.is_empty(), "{dir_errors:?}");
    assert_eq!(None, task.error_message());
    assert_eq!(b"x".to_vec(), fs::read(&new).unwrap());
    assert_eq!(Vec::<String>::new(), staging_left(&replaced));
    assert_eq!(leaves, mode_of(&fx.join("dst")) & 0o7777);
    assert_eq!(leaves, dir_mode);
    assert_eq!(
        b"x",
        &fs::read(fx.join("dst_dir").join("child")).unwrap()[..]
    );
}

/// A copy's owner bits are the source's as the umask leaves them, like
/// `cp`: a umask that clears the owner's write bit clears it on the copy.
/// The umask is process-wide, so the copy runs in a process of its own.
#[test]
fn a_copy_takes_the_umask_on_the_owner_bits_too() {
    crate::test_support::run_alone(
        "file_system::tasks::copy::tests::a_copy_under_a_umask_clearing_owner_bits",
        "",
        &[],
    );
}

/// The umask `a_tree_copy_under_a_umask` runs under, named by this
/// variable in the process it runs in.
const TREE_UMASK: &str = "FILECTRL_TEST_TREE_UMASK";

/// A directory holding another, copied under a umask whose mode leaves
/// the owner no search permission, which going back up through the
/// directory needs.
#[test]
#[ignore = "run under a umask by the test below"]
fn a_tree_copy_under_a_umask() {
    if !crate::test_support::alone() {
        return;
    }
    let umask = std::env::var(TREE_UMASK).unwrap();
    let umask = u32::from_str_radix(&umask, 8).unwrap();
    let fx = TempDir::new("tasks_tree_umask");
    let src = fx.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("sub").join("child"), b"x").unwrap();
    fs::write(src.join("file"), b"y").unwrap();
    for dir in [src.join("sub"), src.clone()] {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();
    }
    nix::sys::stat::umask(mode_bits(umask));

    let errors = copy_one(false, &src, &fx.join("dst"));
    let dst = fx.join("dst");
    let modes = (mode_of(&dst) & 0o7777, {
        // The top directory may leave its owner no search permission.
        fs::set_permissions(&dst, fs::Permissions::from_mode(0o700)).unwrap();
        mode_of(&dst.join("sub")) & 0o7777
    });
    fs::set_permissions(dst.join("sub"), fs::Permissions::from_mode(0o700)).unwrap();

    assert_eq!(Vec::<String>::new(), errors);
    let leaves = 0o777 & !umask;
    assert_eq!((leaves, leaves), modes);
    assert_eq!(
        b"x".to_vec(),
        fs::read(dst.join("sub").join("child")).unwrap()
    );
    assert_eq!(b"y".to_vec(), fs::read(dst.join("file")).unwrap());
}

/// The umask is process-wide, so each copy runs in a process of its own.
#[test_case("177" ; "read and write but no search")]
#[test_case("377" ; "read only")]
fn a_tree_copies_whole_under_a_umask_that_clears_owner_search(umask: &str) {
    crate::test_support::run_alone(
        "file_system::tasks::copy::tests::a_tree_copy_under_a_umask",
        "",
        &[(TREE_UMASK, OsStr::new(umask))],
    );
}

/// A default ACL that takes the owner's search permission from every new
/// directory: the copy still reaches the whole tree. Needs `setfacl` and
/// ACLs on the temporary filesystem; skipped otherwise.
#[test_case("u::rw-,g::r-x,o::---", 0o600 ; "no search")]
fn a_tree_copies_whole_under_a_default_acl_taking_owner_access(acl: &str, owner: u32) {
    let fx = TempDir::new("tasks_tree_default_acl");
    let src = fx.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("sub").join("child"), b"x").unwrap();
    let dest = fx.join("dest");
    fs::create_dir(&dest).unwrap();
    if !setfacl(&["-d", "-m", acl], &dest) {
        eprintln!("skipped: no default ACLs here");
        return;
    }

    let errors = copy_one(false, &src, &dest.join("src"));
    let copied = dest.join("src");
    // The ACL takes the same access from the copied directories as from
    // anything created there: the owner access added to write them is
    // taken back when they are finished. Read before the access is given
    // back so the tree can be read and removed.
    // Each from the top down: a directory without search permission
    // hides what is in it.
    let child = copied.join("sub").join("child");
    let owners = [copied.clone(), copied.join("sub"), child].map(|entry| {
        let owner = mode_of(&entry) & 0o700;
        let _ = fs::set_permissions(&entry, fs::Permissions::from_mode(0o700));
        owner
    });

    assert_eq!(Vec::<String>::new(), errors);
    assert_eq!([owner, owner, owner], owners);
    assert_eq!(
        b"x".to_vec(),
        fs::read(copied.join("sub").join("child")).unwrap()
    );
}

/// A directory copied into a setgid directory whose default ACL takes
/// owner search from what is created there keeps the setgid bit it
/// inherits, which giving it owner access to write it must not drop.
#[test]
fn a_directory_copied_under_a_default_acl_taking_owner_access_keeps_its_setgid() {
    let fx = TempDir::new("tasks_tree_default_acl_setgid");
    let src = fx.join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("child"), b"x").unwrap();
    let dest = fx.join("dest");
    fs::create_dir(&dest).unwrap();
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o2775)).unwrap();
    if mode_of(&dest) & 0o2000 == 0 || !setfacl(&["-d", "-m", "u::rw-,g::r-x,o::---"], &dest) {
        eprintln!("skipped: no setgid directory with a default ACL here");
        return;
    }

    let errors = copy_one(false, &src, &dest.join("src"));
    let mode = mode_of(&dest.join("src"));
    fs::set_permissions(dest.join("src"), fs::Permissions::from_mode(0o700)).unwrap();

    assert_eq!(Vec::<String>::new(), errors);
    assert_eq!(0o2000, mode & 0o2000, "{mode:o}");
}

/// A default ACL that takes all owner access from new directories leaves
/// the one the copy makes unreadable: it is reported rather than given
/// access by name, and the copy of what is below it is skipped. Needs
/// `setfacl` and ACLs on the temporary filesystem; skipped otherwise.
#[test]
fn a_directory_left_unreadable_by_a_default_acl_is_reported() {
    let fx = TempDir::new("tasks_tree_default_acl_none");
    let src = fx.join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("child"), b"x").unwrap();
    let dest = fx.join("dest");
    fs::create_dir(&dest).unwrap();
    if !setfacl(&["-d", "-m", "u::---,g::r-x,o::---"], &dest) {
        eprintln!("skipped: no default ACLs here");
        return;
    }
    let probe = fx.join("probe");
    fs::create_dir(&probe).unwrap();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = fs::read_dir(&probe).is_err();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o700)).unwrap();
    if !unreadable {
        eprintln!("skipped: a directory without owner access can be read here");
        return;
    }
    let copied = dest.join("src");

    let errors = copy_one(false, &src, &copied);

    let _ = fs::set_permissions(&copied, fs::Permissions::from_mode(0o700));
    assert_eq!(
        vec![format!(
            "Failed to create directory {}: Permission denied (os error 13)",
            compact(&copied)
        )],
        errors
    );
    assert!(!copied.join("child").exists());
}

/// Enters the top-level directory `src`, copied to `dst`, as `copy_path`
/// does, and returns what the walk starts from.
fn enter_top(context: &mut CopyContext<'_>, src: &Path, dst: &Path) -> (Option<CopyLevel>, Paths) {
    let (src_parent, dst_parent) = (open_parent(src).unwrap(), open_parent(dst).unwrap());
    let (src_name, dst_name) = (c_name(src).unwrap(), c_name(dst).unwrap());
    let stat = fstatat(
        &src_parent,
        src_name.as_c_str(),
        AtFlags::AT_SYMLINK_NOFOLLOW,
    )
    .unwrap();
    let at = At {
        src: &src_parent,
        dst: &dst_parent,
        src_name: &src_name,
        dst_name: &dst_name,
    };
    let paths = Paths {
        old: src.to_path_buf(),
        new: dst.to_path_buf(),
        depth: 0,
    };
    (enter_directory(context, &at, &paths, &stat), paths)
}

/// Copies the directory `src` to `dst` through `copy_tree`, with
/// `on_leave` called as each directory is left, returning whether the walk
/// finished and the errors it recorded.
fn copy_tree_leaving(
    src: &Path,
    dst: &Path,
    active: &mut ActiveTask,
    on_leave: Box<dyn FnMut(&Path)>,
) -> (bool, Vec<String>) {
    let mut buffer = [0u8; 64];
    let mut context = context(false, active, &mut buffer);
    context.on_leave = Some(on_leave);
    let (level, mut paths) = enter_top(&mut context, src, dst);
    let level = level.expect("the directory should be entered");
    let finished = copy_tree(&mut context, level, &mut paths);
    (finished, context.into_outcome().errors)
}

/// `src/tree/sub/deeper/file`, and where it is copied to.
fn deep_tree(label: &str) -> (TempDir, PathBuf, PathBuf) {
    let fx = TempDir::new(label);
    let tree = fx.join("src").join("tree");
    fs::create_dir_all(tree.join("sub").join("deeper")).unwrap();
    fs::write(tree.join("sub").join("deeper").join("file"), b"x").unwrap();
    fs::create_dir(fx.join("dst")).unwrap();
    let dst = fx.join("dst").join("tree");
    (fx, tree, dst)
}

/// A directory moved elsewhere while the copy is inside it cannot be gone
/// back up through: the walk ends there with filectrl's own refusal,
/// naming the directory it could not return to.
#[test]
fn a_directory_moved_while_its_copy_is_inside_it_ends_the_walk() {
    let (fx, tree, dst) = deep_tree("tasks_walk_relocated");
    let (deeper, sub, elsewhere) = (
        tree.join("sub").join("deeper"),
        tree.join("sub"),
        fx.join("elsewhere"),
    );
    let (tx, _rx) = mpsc::channel();
    let mut active = copy_task(tx);

    let (finished, errors) = copy_tree_leaving(
        &tree,
        &dst,
        &mut active,
        Box::new(move |left| {
            if left == deeper {
                fs::rename(&sub, &elsewhere).unwrap();
            }
        }),
    );
    active.done();

    assert!(finished);
    assert_eq!(
        vec![format!(
            "Cannot copy {}: it was relocated while it was being read",
            compact(&tree)
        )],
        errors
    );
}

/// A directory that cannot be gone back up into for a reason of the
/// system's is reported in the `Failed to` form, with the reason.
#[test]
fn a_directory_that_cannot_be_returned_to_is_reported_with_its_errno() {
    let (fx, tree, dst) = deep_tree("tasks_walk_locked");
    let probe = fx.join("probe");
    fs::create_dir(&probe).unwrap();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = fs::read_dir(&probe).is_err();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o700)).unwrap();
    if !unreadable {
        eprintln!("skipped: a directory without permissions can be read here");
        return;
    }
    let (deeper, locked) = (tree.join("sub").join("deeper"), tree.clone());
    let (tx, _rx) = mpsc::channel();
    let mut active = copy_task(tx);

    let (finished, errors) = copy_tree_leaving(
        &tree,
        &dst,
        &mut active,
        Box::new(move |left| {
            if left == deeper {
                fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
            }
        }),
    );
    active.done();
    fs::set_permissions(&tree, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(finished);
    assert_eq!(
        vec![format!(
            "Failed to copy {}: Permission denied (os error 13)",
            compact(&tree)
        )],
        errors
    );
}

/// A walk cancelled before it meets a directory it cannot return to
/// still ends as cancelled.
#[test]
fn a_cancelled_walk_that_cannot_return_ends_as_cancelled() {
    let (fx, tree, dst) = deep_tree("tasks_walk_relocated_cancelled");
    let (deeper, sub, elsewhere) = (
        tree.join("sub").join("deeper"),
        tree.join("sub"),
        fx.join("elsewhere"),
    );
    let (tx, _rx) = mpsc::channel();
    let (mut active, token) = copy_task_with(tx, 1);

    let (finished, errors) = copy_tree_leaving(
        &tree,
        &dst,
        &mut active,
        Box::new(move |left| {
            if left == deeper {
                token.cancel();
                fs::rename(&sub, &elsewhere).unwrap();
            }
        }),
    );
    active.cancelled();

    assert!(!finished);
    assert_eq!(
        vec![format!(
            "Cannot copy {}: it was relocated while it was being read",
            compact(&tree)
        )],
        errors
    );
}

/// A plain copy of a directory does not keep its modification time, as
/// `cp -R` without `-p` does not.
#[test]
fn a_plain_copy_does_not_keep_a_directorys_modification_time() {
    let fx = TempDir::new("tasks_copy_dir_mtime");
    let src = fx.join("src");
    fs::create_dir(&src).unwrap();
    let old = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    File::open(&src).unwrap().set_modified(old).unwrap();
    let dst = fx.join("dst");

    let errors = copy_one(false, &src, &dst);

    assert_eq!(Vec::<String>::new(), errors);
    assert_ne!(old, fs::metadata(&dst).unwrap().modified().unwrap());
}

/// A granted overwrite whose source changed type since it was selected
/// is refused, and the entry granted is left, which the message says.
#[test]
fn a_granted_overwrite_of_a_source_whose_type_changed_says_the_entry_was_left() {
    let fx = TempDir::new("tasks_type_changed_granted");
    let src = fx.join("src");
    std::os::unix::fs::symlink("target", &src).unwrap();
    let selected = listed(&src);
    fs::remove_file(&src).unwrap();
    fs::write(&src, b"now a file").unwrap();
    let dst = fx.join("dst");
    fs::write(&dst, b"dest").unwrap();
    let (tx, _rx) = mpsc::channel();
    let mut active = copy_task(tx);
    let mut buffer = [0u8; 64];
    let mut context = context(false, &mut active, &mut buffer);

    assert!(copy_path(&mut context, seen(&dst), &selected, &src, &dst));
    let errors = context.into_outcome().errors;
    active.done();

    assert_eq!(
        vec![format!(
            "Cannot copy {}: its type changed since it was selected; {} was not replaced",
            compact(&src),
            compact(&dst)
        )],
        errors
    );
    assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
}

/// A replacement landing on a name freed since, whose rename fails, never
/// says an entry was left there: none was.
#[test]
fn a_landing_on_a_name_freed_since_that_fails_says_nothing_was_left() {
    let fx = TempDir::new("tasks_land_freed_fails");
    let dest = fx.join("d");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("one"), b"granted").unwrap();
    let granted = seen(&dest.join("one")).unwrap();
    let handle = File::open(&dest).unwrap();
    let staging = staged_beside(&handle, &dest, b"staged");
    fs::remove_file(dest.join("one")).unwrap();
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(dest.join("probe"), b"").is_ok() {
        eprintln!("skipped: a directory without write permission can be written here");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let mut active = idle_task();
    let mut buffer = [0u8; 8];
    let mut context = context(false, &mut active, &mut buffer);
    let at = At {
        src: &handle,
        dst: &handle,
        src_name: c"one",
        dst_name: c"one",
    };
    let paths = Paths {
        old: fx.join("src"),
        new: dest.join("one"),
        depth: 0,
    };

    let landed = land(&mut context, &staging, granted, &at, &paths);
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
    staging.remove().unwrap();

    assert_eq!(
        Err(format!(
            "Failed to copy {} to {}: Permission denied (os error 13)",
            compact(&fx.join("src")),
            compact(&dest.join("one"))
        )),
        landed
    );
}

/// A umask that clears the owner's read bit leaves a new staging
/// directory impossible to open, and nothing is given a mode by name: the
/// replacement is refused with the entry left.
#[test]
#[ignore = "run under a umask of 0o477 by the test below"]
fn a_replacement_under_a_umask_clearing_owner_read() {
    if !crate::test_support::alone() {
        return;
    }
    let fx = TempDir::new("tasks_replace_umask_read");
    let src = fx.join("src");
    fs::write(&src, b"x").unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o644)).unwrap();
    let dest = fx.join("dest");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("src"), b"old").unwrap();
    nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o477));
    let probe = fx.join("probe");
    fs::create_dir(&probe).unwrap();
    if fs::read_dir(&probe).is_ok() {
        eprintln!("skipped: a directory without owner read can be read here");
        return;
    }

    let (new, task) = paste_over(false, &dest, &src);

    assert_eq!(
        Some(format!(
            "Failed to copy {} to {}: Permission denied (os error 13); {} was not replaced",
            compact(&src),
            compact(&new),
            compact(&new)
        )),
        task.error_message()
    );
    assert_eq!(b"old".to_vec(), fs::read(&new).unwrap());
    assert_eq!(Vec::<String>::new(), staging_left(&dest));
}

/// The umask is process-wide, so the replacement runs in a process of its
/// own.
#[test]
fn a_replacement_whose_staging_directory_cannot_be_read_leaves_the_entry() {
    crate::test_support::run_alone(
        "file_system::tasks::copy::tests::a_replacement_under_a_umask_clearing_owner_read",
        "",
        &[],
    );
}

/// The fixture `a_replacement_that_fails_part_way_leaves_the_entry` makes,
/// named by this variable in the process it runs this in.
const FAILING_FIXTURE: &str = "FILECTRL_TEST_FAILING_REPLACEMENT";

/// A replacement whose writing fails part way, under a file size limit set
/// by the test below.
#[test]
#[ignore = "run under a file size limit by the test below"]
fn a_replacement_that_fails_to_write() {
    if !crate::test_support::alone() {
        return;
    }
    let fixture = PathBuf::from(std::env::var_os(FAILING_FIXTURE).unwrap());
    let (src, dest) = (fixture.join("src"), fixture.join("dest"));

    let (new, task) = paste_over(false, &dest, &src);

    assert_eq!(
        Some(format!(
            "Failed to copy {} to {}: File too large (os error {}); {} was not replaced",
            compact(&src),
            compact(&new),
            nix::libc::EFBIG,
            compact(&new)
        )),
        task.error_message()
    );
    assert_eq!(b"old".to_vec(), fs::read(&new).unwrap());
    assert_eq!(Vec::<String>::new(), staging_left(&dest));
}

/// A replacement that fails part way leaves the entry it was to replace,
/// and nothing of itself, whoever runs it: the write fails at a file size
/// limit, in a process of its own that ignores the signal a write past it
/// raises.
#[test]
fn a_replacement_that_fails_part_way_leaves_the_entry() {
    let fx = TempDir::new("tasks_replace_fails");
    let dest = fx.join("dest");
    fs::create_dir(&dest).unwrap();
    fs::write(fx.join("src"), vec![7u8; 64 * 1024]).unwrap();
    fs::write(dest.join("src"), b"old").unwrap();

    crate::test_support::run_alone(
        "file_system::tasks::copy::tests::a_replacement_that_fails_to_write",
        "trap '' XFSZ; ulimit -f 1;",
        &[(FAILING_FIXTURE, fx.path().as_os_str())],
    );
}

/// A default ACL taking owner write from new directories would leave the
/// staging directory unwritable. Needs `setfacl` and ACLs on the temporary
/// filesystem; skipped otherwise.
#[test]
fn a_replacement_writes_its_staging_directory_under_a_default_acl() {
    let fx = TempDir::new("tasks_replace_default_acl");
    let src = fx.join("src");
    fs::write(&src, b"x").unwrap();
    let dest = fx.join("dest");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("src"), b"old").unwrap();
    if !setfacl(&["-d", "-m", "u::r-x,g::r-x,o::---"], &dest) {
        eprintln!("skipped: no default ACLs here");
        return;
    }

    let (new, task) = paste_over(false, &dest, &src);

    assert_eq!(None, task.error_message());
    assert_eq!(b"x".to_vec(), fs::read(&new).unwrap());
    assert_eq!(Vec::<String>::new(), staging_left(&dest));
}

// A directory is created owner-writable so its children can be, then
// given its mode: a copy's owner bits come back to what the source had.
#[test_case(0o555, false ; "a copy of a read only directory")]
#[test_case(0o1777, false ; "a copy drops the sticky bit and takes the umask")]
#[test_case(0o2755, false ; "a copy drops the source's setgid bit")]
#[test_case(0o555, true ; "a move of a read only directory")]
#[test_case(0o1777, true ; "a move keeps the sticky bit")]
fn a_copied_directory_ends_with_the_mode_of_its_kind_of_copy(mode: u32, is_move: bool) {
    let fx = TempDir::new("tasks_directory_mode");
    let (src, dst) = (fx.join("src"), fx.join("dst"));
    fs::create_dir(&src).unwrap();
    fs::write(src.join("child"), b"x").unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(mode)).unwrap();

    let errors = copy_one(is_move, &src, &dst);
    let copied = mode_of(&dst) & 0o7777;
    // Writable again before anything can fail, so the fixture can be
    // removed with the children a read-only mode would keep.
    for dir in [&src, &dst] {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
    }

    assert!(errors.is_empty(), "{errors:?}");
    assert!(dst.join("child").exists());
    let expected = if is_move {
        mode
    } else {
        mode & 0o777 & umask_leaves(fx.path())
    };
    assert_eq!(expected, copied);
}

/// A directory created in a setgid directory inherits its setgid bit, and a
/// copy keeps it, like `cp -R`, so what is later created in the copy takes
/// the shared group. A file created there does not inherit it.
#[cfg(target_os = "linux")]
#[test]
fn a_copied_directory_keeps_the_setgid_bit_its_parent_gives_it() {
    let fx = TempDir::new("tasks_directory_setgid");
    let shared = fx.join("shared");
    fs::create_dir(&shared).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o2775)).unwrap();
    let src = fx.join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("child"), b"x").unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();

    let errors = copy_one(false, &src, &shared.join("dst"));

    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(0o2000, mode_of(&shared.join("dst")) & 0o7000);
    assert_eq!(0, mode_of(&shared.join("dst").join("child")) & 0o7000);
}

/// A node is created with the umask applied, like any other entry, so a
/// move puts the source's mode back afterwards.
#[test_case(true ; "a move keeps the mode")]
#[test_case(false ; "a copy takes the umask")]
fn a_moved_fifo_keeps_its_mode_whatever_the_umask(is_move: bool) {
    let fx = TempDir::new("tasks_fifo_mode");
    let src = fx.join("fifo");
    nix::unistd::mkfifo(&src, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o666)).unwrap();
    let dst = fx.join("moved");

    let errors = copy_one(is_move, &src, &dst);

    assert!(errors.is_empty(), "{errors:?}");
    let expected = if is_move {
        0o666
    } else {
        0o666 & umask_leaves(fx.path())
    };
    assert_eq!(expected, mode_of(&dst) & 0o7777);
}

/// A moved node gets the source's group before its mode, like a moved
/// file, so a group-writable FIFO is not writable by whichever group the
/// destination assigned. Needs a supplementary group to tell the two apart,
/// and proves nothing for a user without one. Linux only: macOS has no
/// `getgroups` in nix.
#[cfg(target_os = "linux")]
#[test]
fn a_moved_fifo_keeps_its_group() {
    let primary = nix::unistd::getegid();
    let Some(other) = nix::unistd::getgroups()
        .unwrap()
        .into_iter()
        .find(|&group| group != primary)
    else {
        return;
    };
    let fx = TempDir::new("tasks_fifo_group");
    let src = fx.join("fifo");
    nix::unistd::mkfifo(&src, nix::sys::stat::Mode::from_bits_truncate(0o660)).unwrap();
    nix::unistd::chown(&src, None, Some(other)).unwrap();
    let dst = fx.join("moved");

    let errors = copy_one(true, &src, &dst);

    assert!(errors.is_empty(), "{errors:?}");
    let gid = std::os::unix::fs::MetadataExt::gid(&fs::symlink_metadata(&dst).unwrap());
    assert_eq!(other.as_raw(), gid);
}

/// `mv` clears setuid and setgid when it cannot carry the ownership over,
/// or a program would run as whoever moved it. Another owner cannot be
/// arranged without root, so the rule is driven with a source that names
/// one.
#[test]
fn a_move_drops_setuid_and_setgid_from_another_users_file() {
    let fx = TempDir::new("tasks_move_other_owner");
    let file = File::create(fx.join("dst")).unwrap();
    let stat = fstat(&file).unwrap();
    let source = Source {
        mode: 0o100_6755,
        uid: stat.st_uid.wrapping_add(1),
        ..Source::of(&stat)
    };

    let paths = Paths {
        old: fx.join("src"),
        new: fx.join("dst"),
        depth: 0,
    };
    apply_final_mode(
        &context(true, &mut idle_task(), &mut [0u8; 1]),
        &paths,
        &file,
        &source,
    );

    assert_eq!(0o755, mode_of(&fx.join("dst")) & 0o7777);
}

/// A move keeps the group bits, like `mv`, so it keeps the group they were
/// granted to as well: the copy is otherwise created with whichever group
/// the destination assigns, and a file readable by one group would become
/// readable by another. Needs a second group to belong to, which
/// `getgroups` reports on Linux.
#[cfg(target_os = "linux")]
#[test_case(false, 0o640 ; "a file")]
#[test_case(true, 0o750 ; "a directory")]
fn a_move_keeps_the_group_of_its_source(is_directory: bool, mode: u32) {
    use std::os::unix::fs::MetadataExt;

    let own = nix::unistd::getegid();
    let Some(other) = nix::unistd::getgroups()
        .unwrap()
        .into_iter()
        .find(|group| *group != own)
    else {
        eprintln!("skipped: the user running the tests belongs to one group only");
        return;
    };
    let fx = TempDir::new("tasks_move_group");
    let src = fx.join("src");
    if is_directory {
        fs::create_dir(&src).unwrap();
    } else {
        fs::write(&src, b"x").unwrap();
    }
    std::os::unix::fs::chown(&src, None, Some(other.as_raw())).unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(mode)).unwrap();

    let errors = copy_one(true, &src, &fx.join("dst"));

    assert!(errors.is_empty(), "{errors:?}");
    let copied = fs::symlink_metadata(fx.join("dst")).unwrap();
    assert_eq!(other.as_raw(), copied.gid());
    assert_eq!(mode, copied.mode() & 0o7777);
}

/// Another user renames a directory the copy created and leaves a link to
/// one of the victim's in its place. The copy continues in the directory it
/// created, and neither writes into nor changes the mode of the victim's.
#[test]
fn a_destination_directory_swapped_for_a_symlink_is_not_written_through() {
    let fx = TempDir::new("tasks_destination_swapped");
    let src = fx.join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("planted.txt"), b"x").unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o777)).unwrap();
    let victim = fx.join("victim");
    fs::create_dir(&victim).unwrap();
    fs::set_permissions(&victim, fs::Permissions::from_mode(0o700)).unwrap();
    let dst = fx.join("dst");
    let (tx, _rx) = mpsc::channel();
    let mut active = copy_task(tx);
    let mut buffer = [0u8; 64];
    // A move, whose final mode is the source's 0o777 whatever the umask.
    let mut context = context(true, &mut active, &mut buffer);
    let (src_parent, dst_parent) = (open_parent(&src).unwrap(), open_parent(&dst).unwrap());
    let source = fstatat(&src_parent, c"src", AtFlags::AT_SYMLINK_NOFOLLOW).unwrap();
    let mut paths = Paths {
        old: src.clone(),
        new: dst.clone(),
        depth: 0,
    };
    let at = At {
        src: &src_parent,
        dst: &dst_parent,
        src_name: c"src",
        dst_name: c"dst",
    };
    let level = enter_directory(&mut context, &at, &paths, &source)
        .expect("the directory should be entered");

    fs::rename(&dst, fx.join("moved")).unwrap();
    std::os::unix::fs::symlink(&victim, &dst).unwrap();
    assert!(copy_tree(&mut context, level, &mut paths));
    let errors = context.into_outcome().errors;
    active.done();

    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(0o700, mode_of(&victim) & 0o7777);
    assert!(fs::read_dir(&victim).unwrap().next().is_none());
    assert!(fx.join("moved").join("planted.txt").exists());
    assert_eq!(0o777, mode_of(&fx.join("moved")) & 0o7777);
}

/// The source's own owner swaps a directory the copy has listed for a link
/// to one outside the tree. The link is copied as a link, and nothing
/// outside the tree is read.
#[test]
fn a_source_directory_swapped_for_a_symlink_is_copied_as_the_link() {
    let fx = TempDir::new("tasks_source_swapped");
    let src = fx.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    let outside = fx.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"secret").unwrap();
    let dst = fx.join("dst");
    let (tx, _rx) = mpsc::channel();
    let mut active = copy_task(tx);
    let mut buffer = [0u8; 64];
    let mut context = context(false, &mut active, &mut buffer);
    let (src_parent, dst_parent) = (open_parent(&src).unwrap(), open_parent(&dst).unwrap());
    let source = fstatat(&src_parent, c"src", AtFlags::AT_SYMLINK_NOFOLLOW).unwrap();
    let mut paths = Paths {
        old: src.clone(),
        new: dst.clone(),
        depth: 0,
    };
    let at = At {
        src: &src_parent,
        dst: &dst_parent,
        src_name: c"src",
        dst_name: c"dst",
    };
    let level = enter_directory(&mut context, &at, &paths, &source)
        .expect("the directory should be entered");

    fs::rename(src.join("sub"), fx.join("sub.orig")).unwrap();
    std::os::unix::fs::symlink(&outside, src.join("sub")).unwrap();
    assert!(copy_tree(&mut context, level, &mut paths));
    let errors = context.into_outcome().errors;
    active.done();

    assert!(errors.is_empty(), "{errors:?}");
    assert!(dst.join("sub").symlink_metadata().unwrap().is_symlink());
    assert_eq!(outside, fs::read_link(dst.join("sub")).unwrap());
}

/// A regular file swapped for something else between being listed and
/// being opened. A FIFO would block the open, and with it the one worker
/// every operation shares; a link to a device would be read without end.
#[test_case("fifo" ; "a fifo")]
#[test_case("link" ; "a symlink to a device")]
#[test_case("file_link" ; "a symlink to a file outside the tree")]
#[test_case("dir" ; "a directory")]
fn a_source_file_that_is_no_longer_a_regular_file_is_refused_at_once(name: &'static str) {
    let fx = TempDir::new("tasks_source_not_regular");
    nix::unistd::mkfifo(
        &fx.join("fifo"),
        nix::sys::stat::Mode::from_bits_truncate(0o600),
    )
    .unwrap();
    std::os::unix::fs::symlink("/dev/zero", fx.join("link")).unwrap();
    fs::write(fx.join("outside.txt"), b"secret").unwrap();
    std::os::unix::fs::symlink(fx.join("outside.txt"), fx.join("file_link")).unwrap();
    fs::create_dir(fx.join("dir")).unwrap();
    let path = fx.join(name);
    let (tx, rx) = mpsc::channel();

    // On a thread, so an open that blocks fails the test instead of
    // hanging it.
    thread::spawn(move || {
        let (task_tx, _task_rx) = mpsc::channel();
        let dst = path.with_file_name("dst");
        let prepared = prepare_destination(
            copy_task(task_tx),
            settings(false),
            None,
            &path,
            &dst,
            0o100_644,
        );
        let _ = tx.send(prepared.is_none());
    });

    assert_eq!(Ok(true), rx.recv_timeout(Duration::from_secs(5)));
}

/// A FIFO selected for a move is a regular file by the time the worker
/// runs. Recreating it as a FIFO and removing the file would lose its data.
#[test]
fn a_move_refuses_a_source_whose_type_changed_since_it_was_selected() {
    let fx = TempDir::new("tasks_move_type_changed");
    let old = fx.join("item");
    nix::unistd::mkfifo(&old, nix::sys::stat::Mode::from_bits_truncate(0o644)).unwrap();
    fs::create_dir(fx.join("dest")).unwrap();

    let (new_path, task) = paste_after(true, &fx.join("dest"), &old, || {
        fs::remove_file(&old).unwrap();
        fs::write(&old, b"data").unwrap();
    });

    assert_eq!(b"data".as_slice(), fs::read(&old).unwrap());
    assert!(new_path.symlink_metadata().is_err());
    assert_eq!(
        Some(format!(
            "Cannot move {}: its type changed since it was selected",
            compact(&old)
        )),
        task.error_message()
    );
}

/// Another file is renamed over the selected name before the worker runs.
/// It is copied with its own mode, never the selected file's: a copy of a
/// 0600 file must not come out readable by others, and a move of one must
/// not get the other file's setuid or execute bits.
#[test_case(false ; "a copy")]
#[test_case(true ; "a move")]
fn a_file_renamed_over_the_selection_is_copied_with_its_own_mode(is_move: bool) {
    let fx = TempDir::new("tasks_renamed_over");
    let old = fx.join("notes");
    fs::write(&old, b"selected").unwrap();
    fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).unwrap();
    let secret = fx.join("secret");
    fs::write(&secret, b"secret").unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    fs::create_dir(fx.join("dest")).unwrap();

    let (new_path, task) = paste_after(is_move, &fx.join("dest"), &old, || {
        fs::rename(&secret, &old).unwrap();
    });

    assert_eq!(None, task.error_message());
    assert_eq!(b"secret".as_slice(), fs::read(&new_path).unwrap());
    assert_eq!(0o600, mode_of(&new_path) & 0o7777);
}

/// The destination directory is swapped for a link into the source after
/// the paste was validated. The copy refuses to descend into the directory
/// it created rather than copying its own output without end.
#[test]
fn a_copy_never_descends_into_a_directory_it_created() {
    let fx = TempDir::new("tasks_copy_into_itself");
    let src = fx.join("s");
    fs::create_dir_all(src.join("sub")).unwrap();
    let dest = fx.join("dest");
    fs::create_dir(&dest).unwrap();
    // The swap lands after the paste was validated and before the copy runs.
    fs::remove_dir(&dest).unwrap();
    std::os::unix::fs::symlink(src.join("sub"), &dest).unwrap();

    let mut active = idle_task();
    let mut buffer = [0u8; 64];
    let mut context = context(false, &mut active, &mut buffer);
    // A copy that did descend would nest `s/sub` without end; the cap stops
    // the walk a few levels down whatever the timing, so the tree stays
    // small enough for the fixture to remove.
    context.max_depth = Some(8);
    copy_path(&mut context, None, &listed(&src), &src, &dest.join("s"));
    let errors = context.into_outcome().errors;
    active.done();

    assert_eq!(1, errors.len(), "{errors:?}");
    assert!(
        errors[0].ends_with("it is inside the destination being written"),
        "{errors:?}"
    );
    assert!(src.join("sub/s/sub").is_dir());
    assert!(!src.join("sub/s/sub/s").exists());
    assert!(src.join("sub/s/sub").read_dir().unwrap().next().is_none());
}

/// Another directory is renamed over one the copy listed, between the
/// listing and the open. It is refused rather than copied under metadata
/// that describes a different directory.
#[test]
fn a_directory_replaced_after_it_was_listed_is_refused() {
    let fx = TempDir::new("tasks_directory_replaced");
    let src = fx.join("src");
    fs::create_dir(&src).unwrap();
    let other = fx.join("other");
    fs::create_dir(&other).unwrap();
    let (src_parent, dst_parent) = (open_parent(&src).unwrap(), open_parent(&src).unwrap());
    let stat = fstatat(&src_parent, c"src", AtFlags::AT_SYMLINK_NOFOLLOW).unwrap();
    fs::rename(&other, &src).unwrap();
    let (tx, _rx) = mpsc::channel();
    let mut active = copy_task(tx);
    let mut buffer = [0u8; 64];
    let mut context = context(false, &mut active, &mut buffer);
    let at = At {
        src: &src_parent,
        dst: &dst_parent,
        src_name: c"src",
        dst_name: c"dst",
    };
    let paths = Paths {
        old: src.clone(),
        new: fx.join("dst"),
        depth: 0,
    };

    let level = enter_directory(&mut context, &at, &paths, &stat);
    let errors = context.into_outcome().errors;
    active.done();

    assert!(level.is_none());
    assert_eq!(1, errors.len());
    assert!(
        errors[0].ends_with("it was replaced while it was being read"),
        "{errors:?}"
    );
}

/// The copy returns through both of its directories, and each has to be
/// the one it left.
#[test_case(false ; "the source moved")]
#[test_case(true ; "the destination moved")]
fn a_copy_returns_only_to_the_parents_it_left(destination_moved: bool) {
    let fx = TempDir::new("tasks_copy_reopen_pair");
    for side in ["src", "dst"] {
        fs::create_dir_all(fx.join(side).join("child")).unwrap();
    }
    fs::create_dir(fx.join("elsewhere")).unwrap();
    let open = |side: &str| {
        let parent = open_directory(CWD, &fx.join(side)).unwrap();
        let child = open_directory(&parent, "child").unwrap();
        (EntryId::of(&parent).unwrap(), child)
    };
    let ((src_id, src), (dst_id, dst)) = (open("src"), open("dst"));
    let child = Pair { src, dst };
    let parent = Pair {
        src: src_id,
        dst: dst_id,
    };
    let reopened = child.reopen_parent(parent, "moved").unwrap();
    assert_eq!(src_id, EntryId::of(&reopened.src).unwrap());
    assert_eq!(dst_id, EntryId::of(&reopened.dst).unwrap());

    let side = if destination_moved { "dst" } else { "src" };
    fs::rename(
        fx.join(side).join("child"),
        fx.join("elsewhere").join("child"),
    )
    .unwrap();

    let error = child.reopen_parent(parent, "moved").err().unwrap();
    assert_eq!("moved", error.to_string());
}

fn set_times(path: &Path, atime: i64, mtime: i64) {
    use nix::sys::stat::{UtimensatFlags, utimensat};
    utimensat(
        nix::fcntl::AT_FDCWD,
        path,
        &TimeSpec::new(atime, 0),
        &TimeSpec::new(mtime, 0),
        UtimensatFlags::NoFollowSymlink,
    )
    .unwrap();
}

fn times_of(path: &Path) -> (i64, i64) {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.atime(), metadata.mtime())
}

#[test]
fn a_move_keeps_the_access_time_of_a_file() {
    let fx = TempDir::new("tasks_move_file_atime");
    let old = fx.join("f");
    fs::write(&old, b"data").unwrap();
    // Older than the modification time, which `relatime` updates on the
    // first read.
    set_times(&old, 1_000_000_000, 1_000_000_100);
    fs::create_dir(fx.join("dest")).unwrap();

    let (new_path, task) = paste_after(true, &fx.join("dest"), &old, || {});

    assert_eq!(None, task.error_message());
    assert_eq!((1_000_000_000, 1_000_000_100), times_of(&new_path));
}

/// Linux only: macOS has no `O_NOATIME`, so the pre-scan's listing sets a
/// directory's access time before the copy reads it.
#[cfg(target_os = "linux")]
#[test]
fn a_move_keeps_the_access_times_of_a_tree_the_scan_listed() {
    let fx = TempDir::new("tasks_move_dir_atime");
    let old = fx.join("d");
    fs::create_dir_all(old.join("sub")).unwrap();
    fs::write(old.join("sub").join("x"), b"x").unwrap();
    // Deepest first, so setting one does not move its parent's again.
    set_times(&old.join("sub"), 1_000_000_000, 1_000_000_100);
    set_times(&old, 1_000_000_200, 1_000_000_300);
    fs::create_dir(fx.join("dest")).unwrap();

    // The progress scan lists every directory before the copy does.
    let (new_path, task) = paste_after(true, &fx.join("dest"), &old, || {});

    assert_eq!(None, task.error_message());
    assert_eq!((1_000_000_200, 1_000_000_300), times_of(&new_path));
    assert_eq!(
        (1_000_000_000, 1_000_000_100),
        times_of(&new_path.join("sub"))
    );
}

/// Runs `setfacl` with `args` on `path`. False where it is not installed
/// or the filesystem has no ACLs, for a test to skip.
fn setfacl(args: &[&str], path: &Path) -> bool {
    std::process::Command::new("setfacl")
        .args(args)
        .arg(path)
        .status()
        .is_ok_and(|status| status.success())
}

fn getfacl(path: &Path) -> String {
    let output = std::process::Command::new("getfacl")
        .args(["-c", "-p"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap()
}

/// A source of mode 600 with a named user granted `rw`, which makes the
/// mask, and so the group bits of its mode, `rw` too.
fn file_with_an_acl(label: &str) -> Option<(TempDir, PathBuf)> {
    let fx = TempDir::new(label);
    let old = fx.join("secret");
    fs::write(&old, b"secret").unwrap();
    fs::set_permissions(&old, fs::Permissions::from_mode(0o600)).unwrap();
    fs::create_dir(fx.join("dest")).unwrap();
    setfacl(&["-m", "u:nobody:rw"], &old).then_some((fx, old))
}

#[test]
fn a_move_keeps_the_acl_so_the_owning_group_gains_nothing() {
    let Some((fx, old)) = file_with_an_acl("tasks_move_acl") else {
        eprintln!("skipped: no ACLs here");
        return;
    };
    let before = getfacl(&old);

    let (new_path, task) = paste_after(true, &fx.join("dest"), &old, || {});

    assert_eq!(None, task.error_message());
    // Without it the mask would be granted to the owning group instead.
    assert_eq!(before, getfacl(&new_path));
    assert!(before.contains("group::---"), "{before}");
    assert_eq!(0o660, mode_of(&new_path) & 0o7777);
}

#[test]
fn a_copy_does_not_keep_the_acl() {
    let Some((fx, old)) = file_with_an_acl("tasks_copy_acl") else {
        eprintln!("skipped: no ACLs here");
        return;
    };

    let (new_path, task) = paste_after(false, &fx.join("dest"), &old, || {});

    // Like `cp` without `-p`.
    assert_eq!(None, task.error_message());
    assert!(!getfacl(&new_path).contains("nobody"));
}

/// Gives `path` the user attributes `user.filectrl`, and `user.empty`
/// with an empty value. False where the filesystem has no user
/// attributes, for a test to skip.
fn set_user_attribute(path: &Path) -> bool {
    let set = |name, value: &[u8]| rustix::fs::setxattr(path, name, value, XattrFlags::empty());
    set("user.filectrl", b"kept").is_ok() && set("user.empty", b"").is_ok()
}

fn attribute(path: &Path, name: &str) -> Option<Vec<u8>> {
    let mut value = Vec::with_capacity(64);
    rustix::fs::getxattr(path, name, rustix::buffer::spare_capacity(&mut value)).ok()?;
    Some(value)
}

fn user_attribute(path: &Path) -> Option<Vec<u8>> {
    attribute(path, "user.filectrl")
}

/// Like `mv`, which keeps every attribute it can, not only the ACLs.
#[test_case(false ; "a file")]
#[test_case(true ; "a directory")]
fn a_move_keeps_the_extended_attributes(is_directory: bool) {
    let fx = TempDir::new("tasks_move_xattr");
    let old = fx.join("item");
    if is_directory {
        fs::create_dir(&old).unwrap();
    } else {
        fs::write(&old, b"data").unwrap();
    }
    fs::create_dir(fx.join("dest")).unwrap();
    if !set_user_attribute(&old) {
        eprintln!("skipped: no user extended attributes here");
        return;
    }

    let (new_path, task) = paste_after(true, &fx.join("dest"), &old, || {});

    assert_eq!(None, task.error_message());
    assert_eq!(Some(b"kept".to_vec()), user_attribute(&new_path));
    assert_eq!(Some(Vec::new()), attribute(&new_path, "user.empty"));
}

#[test]
fn a_copy_does_not_keep_the_extended_attributes() {
    let fx = TempDir::new("tasks_copy_xattr");
    let old = fx.join("item");
    fs::write(&old, b"data").unwrap();
    fs::create_dir(fx.join("dest")).unwrap();
    if !set_user_attribute(&old) {
        eprintln!("skipped: no user extended attributes here");
        return;
    }

    let (new_path, task) = paste_after(false, &fx.join("dest"), &old, || {});

    // Like `cp` without `-p`.
    assert_eq!(None, task.error_message());
    assert_eq!(None, user_attribute(&new_path));
}

/// A value read the way `flistxattr` and `fgetxattr` return one, as the
/// `n`th call finds it: `grows` gives its length per call.
fn reading(grows: impl Fn(usize) -> usize) -> impl Fn(&mut [u8]) -> rustix::io::Result<usize> {
    let calls = std::cell::Cell::new(0);
    move |buffer: &mut [u8]| {
        let len = grows(calls.replace(calls.get() + 1));
        if buffer.is_empty() {
            return Ok(len);
        }
        if buffer.len() < len {
            return Err(rustix::io::Errno::RANGE);
        }
        buffer[..len].fill(b'v');
        Ok(len)
    }
}

#[test]
fn a_value_that_grew_while_it_was_read_is_measured_again() {
    // Measured at 4 bytes, 6 by the time it is read, then stable.
    let read = reading(|call| if call == 0 { 4 } else { 6 });

    assert_eq!(Ok(vec![b'v'; 6]), read_sized(read));
}

/// Measured empty, it is read as empty without asking again: an attribute
/// added in between would otherwise be read into an empty buffer and lost.
#[test]
fn a_value_measured_empty_is_not_read_again() {
    let calls = std::cell::Cell::new(0);
    let read = |_: &mut [u8]| {
        calls.set(calls.get() + 1);
        Ok(if calls.get() == 1 { 0 } else { 5 })
    };

    assert_eq!(Ok(Vec::new()), read_sized(read));
    assert_eq!(1, calls.get());
}

#[test]
fn a_value_that_keeps_growing_is_given_up_on() {
    let read = reading(|call| call + 1);

    assert_eq!(Err(rustix::io::Errno::RANGE), read_sized(read));
}

/// What is still asked for by name when the list cannot be read.
#[cfg(target_os = "linux")]
#[test_case(false => vec![c"system.posix_acl_access".to_owned()] ; "a file")]
#[test_case(true => vec![
    c"system.posix_acl_access".to_owned(),
    c"system.posix_acl_default".to_owned(),
] ; "a directory")]
fn the_acls_are_named_by_kind(is_directory: bool) -> Vec<CString> {
    acl_names(is_directory)
}

/// A list is read as its names. A list that cannot be read, including one
/// the filesystem does not support listing while it still serves a named
/// attribute, falls back to the ACL names.
#[test_case(Ok(b"user.a\0user.b\0".to_vec()) => vec![
    c"user.a".to_owned(),
    c"user.b".to_owned(),
] ; "a list")]
#[test_case(Err(rustix::io::Errno::NOTSUP) => acl_names(false) ; "listing unsupported")]
#[test_case(Err(rustix::io::Errno::IO) => acl_names(false) ; "listing failed")]
fn the_names_read_follow_the_listing(listed: rustix::io::Result<Vec<u8>>) -> Vec<CString> {
    attribute_names(Path::new("moved"), false, listed)
}

#[test]
fn a_move_keeps_both_acls_of_a_directory() {
    let fx = TempDir::new("tasks_move_dir_acl");
    let old = fx.join("d");
    fs::create_dir(&old).unwrap();
    fs::set_permissions(&old, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(fx.join("dest")).unwrap();
    if !(setfacl(&["-m", "u:nobody:rx"], &old) && setfacl(&["-d", "-m", "u:nobody:rwx"], &old)) {
        eprintln!("skipped: no ACLs here");
        return;
    }
    let before = getfacl(&old);

    let (new_path, task) = paste_after(true, &fx.join("dest"), &old, || {});

    assert_eq!(None, task.error_message());
    assert_eq!(before, getfacl(&new_path));
    assert!(before.contains("default:user:nobody:rwx"), "{before}");
}

/// `O_NOFOLLOW` refuses a symlink swapped in at the name, rather than
/// reading its target.
#[test]
fn a_source_file_is_not_opened_through_a_symlink() {
    let fx = TempDir::new("tasks_source_symlink");
    std::fs::write(fx.join("target"), b"x").unwrap();
    std::os::unix::fs::symlink(fx.join("target"), fx.join("link")).unwrap();
    let dir = File::open(fx.path()).unwrap();

    let error = open_source_file(&dir, c"link").expect_err("a link is refused");

    assert_eq!(Some(nix::libc::ELOOP), error.raw_os_error());
}

/// Opened non-blocking so a FIFO cannot hang the open, then switched back
/// so the copy's reads block as usual.
#[test]
fn a_source_file_is_read_blocking() {
    let fx = TempDir::new("tasks_source_blocking");
    std::fs::write(fx.join("file"), b"x").unwrap();
    let dir = File::open(fx.path()).unwrap();

    let (file, _) = open_source_file(&dir, c"file").unwrap();

    let status = OFlag::from_bits_retain(fcntl(&file, FcntlArg::F_GETFL).unwrap());
    assert!(!status.contains(OFlag::O_NONBLOCK), "{status:?}");
}

/// A name taken, by a file or by a symlink planted there, is reported as
/// taken, which is what settles it as a raced collision.
#[test_case(false ; "a file")]
#[test_case(true ; "a symlink")]
fn a_destination_file_is_not_created_over_a_taken_name(is_symlink: bool) {
    let fx = TempDir::new("tasks_create_taken");
    std::fs::write(fx.join("elsewhere"), b"keep").unwrap();
    if is_symlink {
        std::os::unix::fs::symlink(fx.join("elsewhere"), fx.join("dst")).unwrap();
    } else {
        std::fs::write(fx.join("dst"), b"keep").unwrap();
    }
    let dir = File::open(fx.path()).unwrap();

    let error = create_file_at(&dir, c"dst", 0o100_644, false).unwrap_err();

    assert_eq!(ErrorKind::AlreadyExists, error.kind());
    assert_eq!(
        b"keep".to_vec(),
        std::fs::read(fx.join("elsewhere")).unwrap()
    );
}

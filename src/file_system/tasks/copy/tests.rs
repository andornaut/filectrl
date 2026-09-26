#[cfg(target_os = "linux")]
use std::ffi::OsStr;
use std::{
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

use test_case::test_case;

use super::{
    super::{
        PROGRESS_DEBOUNCE_PERCENTAGE, PROGRESS_MIN_INTERVAL,
        test_support::{copy_task, copy_task_with, mode_of, paste_after, paste_over},
    },
    *,
};
use crate::{
    command::{
        Command,
        progress::{ActiveTask, Progress},
    },
    file_system::path_info::{PathInfo, compact},
    test_support::TempDir,
};

fn listed(path: impl AsRef<Path>) -> PathInfo {
    PathInfo::try_from(path.as_ref()).unwrap()
}

/// Copies `src` to `dst` (as a move's copy stage when `is_move`, over what holds `dst` when
/// `overwrite`) with `active`. Returns whether it finished and the errors recorded.
fn copy_with(
    active: &mut ActiveTask,
    is_move: bool,
    overwrite: bool,
    src: &Path,
    dst: &Path,
) -> (bool, Vec<String>) {
    let mut buffer = [0u8; 64];
    let mut context = CopyContext::new(is_move, active, &mut buffer, 0);
    let finished = copy_path(&mut context, overwrite, src, dst);
    (finished, context.into_outcome().errors)
}

/// Copies `src` to `dst` (as a move's copy stage when `is_move`), returning the errors recorded.
fn copy_one(is_move: bool, src: &Path, dst: &Path) -> Vec<String> {
    let (tx, _rx) = mpsc::channel();
    let mut active = copy_task(tx);
    let (finished, errors) = copy_with(&mut active, is_move, false, src, dst);
    assert!(finished);
    active.done();
    errors
}

/// Copies `src` to `dst` with a task cancelled before it starts. Returns whether it finished and
/// the errors recorded.
fn copy_cancelled(overwrite: bool, src: &Path, dst: &Path) -> (bool, Vec<String>) {
    let (tx, rx) = mpsc::channel();
    std::mem::forget(rx);
    let (mut active, token) = copy_task_with(tx, 1);
    token.cancel();
    let copied = copy_with(&mut active, false, overwrite, src, dst);
    active.cancelled();
    copied
}

/// The names in `dir`, sorted: a replacement leaves no temporary name behind.
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// Linux only: macOS refuses mknod of a socket to an unprivileged process.
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
    assert_eq!(0o644 & umask_leaves(fx.path()), dst_mode & 0o7777);
}

#[test]
fn copy_path_continues_past_unreadable_entries() {
    let fx = TempDir::new("tasks");
    let src = fx.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.txt"), b"a").unwrap();
    fs::write(src.join("bad"), b"x").unwrap();
    fs::write(src.join("c.txt"), b"c").unwrap();
    fs::set_permissions(src.join("bad"), fs::Permissions::from_mode(0o000)).unwrap();
    // Root and permission-ignoring mounts read a mode-000 file; probe rather than inspect the euid.
    let is_unreadable = File::open(src.join("bad")).is_err();
    let dst = fx.join("dst");

    let errors = copy_one(false, &src, &dst);

    if is_unreadable {
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
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert!(dst.join("bad").exists());
    }
    assert!(dst.join("a.txt").exists());
    assert!(dst.join("c.txt").exists());
}

#[test]
fn copy_path_reuses_one_buffer_without_leaking_bytes_between_files() {
    let fx = TempDir::new("tasks");
    let src = fx.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a_long.txt"), b"aaaaaaaaaaaaaaaa").unwrap();
    fs::write(src.join("b_short.txt"), b"b").unwrap();
    let dst = fx.join("dst");

    // One buffer serves the whole tree, so a short file after a longer one must not get its
    // trailing bytes.
    let errors = copy_one(false, &src, &dst);
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");

    assert_eq!(
        b"aaaaaaaaaaaaaaaa".to_vec(),
        fs::read(dst.join("a_long.txt")).unwrap()
    );
    assert_eq!(b"b".to_vec(), fs::read(dst.join("b_short.txt")).unwrap());
}

/// The modification times of `root` and everything under it, by relative name.
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
fn a_move_keeps_the_modification_times_of_the_whole_tree() {
    let fx = TempDir::new("tasks_times");
    let src = fx.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("a.txt"), b"a").unwrap();
    fs::write(src.join("sub").join("b.txt"), b"b").unwrap();
    // Deepest first, so writing a child does not move the parent's time again.
    let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    for relative in ["sub/b.txt", "sub", "a.txt", "."] {
        let file = File::options().read(true).open(src.join(relative)).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(old))
            .unwrap();
    }
    let before = modified_times(&src);
    let dst = fx.join("dst");

    let errors = copy_one(true, &src, &dst);
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");

    assert_eq!(before, modified_times(&dst));
}

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

#[test]
fn a_cancelled_file_copy_is_left_with_the_source_mode() {
    let fx = TempDir::new("tasks_cancelled_file_mode");
    let src = fx.join("src.txt");
    fs::write(&src, b"src").unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o640)).unwrap();
    let dst = fx.join("dst.txt");

    // Cancelled after the destination is created and before a byte is written.
    let (finished, _) = copy_cancelled(false, &src, &dst);

    assert!(!finished);
    assert!(dst.exists());
    assert_eq!(0o640 & umask_leaves(fx.path()), mode_of(&dst) & 0o7777);
}

#[test]
fn a_cancelled_directory_copy_is_left_with_the_source_mode() {
    let fx = TempDir::new("tasks_cancelled_directory_mode");
    let src = fx.join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("a.txt"), b"a").unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o750)).unwrap();
    let dst = fx.join("dst");

    let (finished, _) = copy_cancelled(false, &src, &dst);

    assert!(!finished);
    assert!(!dst.join("a.txt").exists());
    assert_eq!(0o750 & umask_leaves(fx.path()), mode_of(&dst) & 0o7777);
}

#[test]
fn a_cancelled_replacement_leaves_the_entry_it_would_have_replaced() {
    let fx = TempDir::new("tasks_replace_cancelled");
    let (src, dst) = (fx.join("src.txt"), fx.join("dest.txt"));
    fs::write(&src, b"src").unwrap();
    fs::write(&dst, b"dest").unwrap();

    let (finished, errors) = copy_cancelled(true, &src, &dst);

    assert!(!finished);
    assert_eq!(Vec::<String>::new(), errors);
    assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
    assert_eq!(vec!["dest.txt", "src.txt"], names_in(fx.path()));
}

/// Creating a device node fails for a user after the copy began.
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

    let message = task.error_message().expect("the copy fails");
    assert!(message.contains("Operation not permitted"), "{message}");
    assert_eq!(b"dest".to_vec(), fs::read(&new).unwrap());
    assert_eq!(vec!["null"], names_in(fx.path()));
}

/// Names the fixture `a_replacement_that_fails_part_way_leaves_the_entry` makes, in the process it
/// runs.
const FAILING_FIXTURE: &str = "FILECTRL_TEST_FAILING_REPLACEMENT";

/// Run by the test below under a file size limit; on its own it proves nothing.
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
            "Failed to write {}: File too large (os error {})",
            compact(&new),
            nix::libc::EFBIG,
        )),
        task.error_message()
    );
    assert_eq!(b"old".to_vec(), fs::read(&new).unwrap());
    assert_eq!(vec!["src"], names_in(&dest));
}

/// The write fails at a file size limit, in a process of its own that ignores the signal a write
/// past it raises.
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

#[test]
fn dir_total_size_sums_the_files_of_the_whole_tree_but_not_its_symlinks() {
    let fx = TempDir::new("tasks");
    let root = fx.join("tree");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("a.txt"), b"abc").unwrap();
    fs::write(root.join("sub").join("b.txt"), b"defgh").unwrap();
    std::os::unix::fs::symlink(root.join("a.txt"), root.join("link")).unwrap();
    let (tx, _rx) = mpsc::channel();
    let active = copy_task(tx);

    assert_eq!(Some(8), dir_total_size(&active, &root));
    active.done();
}

#[test]
fn dir_total_size_stops_when_the_task_is_cancelled() {
    let fx = TempDir::new("tasks");
    fs::write(fx.join("a.txt"), b"x").unwrap();
    let (tx, _rx) = mpsc::channel();
    let (active, token) = copy_task_with(tx, 1);
    token.cancel();

    assert_eq!(None, dir_total_size(&active, fx.path()));
    active.done();
}

#[test]
fn copy_path_advances_progress_from_the_bytes_written() {
    let fx = TempDir::new("tasks_copy_progress");
    let src = fx.join("src.bin");
    fs::write(&src, [7u8; 200]).unwrap();
    let (tx, rx) = mpsc::channel();
    let (mut active, _) = copy_task_with(tx, 200);

    let (finished, _) = copy_with(&mut active, false, false, &src, &fx.join("dst.bin"));
    assert!(finished);
    active.done();
    let completed: Vec<u64> = rx
        .try_iter()
        .filter_map(|command| match command {
            Command::Progress(task) if !task.is_terminal() => Some(task.progress().completed),
            _ => None,
        })
        .collect();

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
    let (active, outcome) =
        copy_with_progress(false, false, &listed(&src), active, &src, &fx.join("dst"))
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

    // `set_total` sends one, the first chunk another, and the floor admits one more per interval.
    let bound = 2 + elapsed.as_millis() / PROGRESS_MIN_INTERVAL.as_millis();
    assert!(
        (updates.len() as u128) <= bound,
        "{} updates in {elapsed:?} for {FILES} files",
        updates.len()
    );
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
    // The copy finishes inside the floor, so only the threshold shows the share of the total.
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
    fs::write(&target, b"hello").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    let link = fx.join("link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let dst = fx.join("copied_link.txt");

    let errors = copy_one(false, &link, &dst);
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");

    assert!(fs::symlink_metadata(&dst).unwrap().is_symlink());
    assert_eq!(fs::read_link(&dst).unwrap(), target);
    assert_eq!(0o600, mode_of(&target) & 0o7777, "the target is untouched");
    assert_eq!(fs::read(&target).unwrap(), b"hello");
}

#[test]
fn a_symlink_to_a_directory_inside_a_tree_is_copied_as_the_link() {
    let fx = TempDir::new("tasks_tree_link");
    let src = fx.join("src");
    fs::create_dir(&src).unwrap();
    let outside = fx.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"secret").unwrap();
    std::os::unix::fs::symlink(&outside, src.join("sub")).unwrap();
    let dst = fx.join("dst");

    let errors = copy_one(false, &src, &dst);

    assert!(errors.is_empty(), "{errors:?}");
    assert!(dst.join("sub").symlink_metadata().unwrap().is_symlink());
    assert_eq!(outside, fs::read_link(dst.join("sub")).unwrap());
}

/// The permission bits the umask leaves of `0o777`. Probed, since reading the umask means setting
/// it for every thread.
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

#[test_case(0o4755, false ; "a copy drops setuid")]
#[test_case(0o2755, false ; "a copy drops setgid")]
#[test_case(0o777, false ; "a copy takes the umask")]
#[test_case(0o4755, true ; "a move keeps setuid")]
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
    // Writable again before anything can fail, so the fixture can be removed.
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

#[test]
fn a_move_keeps_the_modification_time_of_a_file() {
    use std::os::unix::fs::MetadataExt;
    let fx = TempDir::new("tasks_move_file_mtime");
    let old = fx.join("f");
    fs::write(&old, b"data").unwrap();
    let mtime = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_100);
    File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    fs::create_dir(fx.join("dest")).unwrap();

    let (new_path, task) = paste_after(true, &fx.join("dest"), &old, || {});

    assert_eq!(None, task.error_message());
    assert_eq!(1_000_000_100, fs::metadata(&new_path).unwrap().mtime());
}

/// Without an overwrite a taken name fails, and a symlink there is not written through.
#[test_case(false ; "a file")]
#[test_case(true ; "a symlink")]
fn a_copy_never_writes_over_a_taken_name(is_symlink: bool) {
    let fx = TempDir::new("tasks_create_taken");
    let src = fx.join("src");
    fs::write(&src, b"src").unwrap();
    fs::write(fx.join("elsewhere"), b"keep").unwrap();
    let dst = fx.join("dst");
    if is_symlink {
        std::os::unix::fs::symlink(fx.join("elsewhere"), &dst).unwrap();
    } else {
        fs::write(&dst, b"keep").unwrap();
    }

    let errors = copy_one(false, &src, &dst);

    assert_eq!(
        vec![format!(
            "Failed to copy {} to {}: File exists (os error 17)",
            compact(&src),
            compact(&dst)
        )],
        errors
    );
    assert_eq!(b"keep".to_vec(), fs::read(fx.join("elsewhere")).unwrap());
    assert_eq!(is_symlink, dst.symlink_metadata().unwrap().is_symlink());
}

/// Linux only: APFS refuses a name that is not valid UTF-8.
#[cfg(target_os = "linux")]
#[test]
fn a_file_name_that_is_not_utf8_is_copied() {
    use std::os::unix::ffi::OsStrExt;
    let fx = TempDir::new("tasks_non_utf8");
    let src = fx.join("src");
    fs::create_dir(&src).unwrap();
    let name = OsStr::from_bytes(b"caf\xe9");
    fs::write(src.join(name), b"x").unwrap();

    let errors = copy_one(false, &src, &fx.join("dst"));

    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(b"x".to_vec(), fs::read(fx.join("dst").join(name)).unwrap());
}

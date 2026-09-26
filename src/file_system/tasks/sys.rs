//! The system calls the tasks make through nix.

use nix::{libc, sys::stat::Mode};

/// The permission bits of `mode`, setuid, setgid and sticky included.
pub(super) fn mode_bits(mode: u32) -> Mode {
    // `mode_t` is u32 on Linux but u16 on macOS; the permission bits fit.
    #[allow(clippy::cast_possible_truncation)]
    let raw = (mode & 0o7777) as libc::mode_t;
    Mode::from_bits_truncate(raw)
}

/// Sets the mode of `name` in `dir` without following a symlink at the name. `EOPNOTSUPP` is
/// returned, never retried with a following call: a link swapped in before a retry would have its
/// target changed.
pub(in crate::file_system) fn set_mode_at(
    dir: impl std::os::fd::AsFd,
    name: &(impl ?Sized + nix::NixPath),
    mode: u32,
) -> std::io::Result<()> {
    use nix::sys::stat::{FchmodatFlags, fchmodat};
    Ok(fchmodat(
        dir,
        name,
        mode_bits(mode),
        FchmodatFlags::NoFollowSymlink,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_permission_bits_keep_the_special_bits_and_drop_the_type() {
        assert_eq!(Mode::from_bits_truncate(0o4755), mode_bits(0o104_755));
    }
}

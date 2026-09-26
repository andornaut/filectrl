//! The system calls the tasks make through nix.

use nix::{libc, sys::stat::Mode};

/// The permission bits of `mode`, setuid, setgid and sticky included.
pub(super) fn mode_bits(mode: u32) -> Mode {
    // `mode_t` is u32 on Linux but u16 on macOS; the permission bits fit.
    #[allow(clippy::cast_possible_truncation)]
    let raw = (mode & 0o7777) as libc::mode_t;
    Mode::from_bits_truncate(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_permission_bits_keep_the_special_bits_and_drop_the_type() {
        assert_eq!(Mode::from_bits_truncate(0o4755), mode_bits(0o104_755));
    }
}

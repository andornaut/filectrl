//! The paste state machine: what each source of a paste meets at its
//! destination, and what to do about it.

use std::{
    collections::{HashSet, VecDeque},
    ffi::OsStr,
};

use unicode_normalization::UnicodeNormalization;

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
    /// The standing `*All` answer, which the queue applies to each collision
    /// it meets from then on (though "overwrite all" still asks about a
    /// directory, which is never replaced), shared with the workers running
    /// this paste's sources, so a standing "skip all" also skips a name taken
    /// at a destination after the queue saw it free, in sources handed out
    /// before it was given.
    pub(super) conflicts: Conflicts,
    /// The folded names (`fold_keys`) of the sources started earlier in this
    /// paste. Two marked sources can share a basename (search results make it
    /// easy), and the second is refused rather than asked about, like `mv a/x
    /// b/x d/`: whatever the answer, replacing the name would destroy what the
    /// first source put there, and for a cut that is the only copy.
    pub(super) claimed: HashSet<String>,
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
/// `Entry` is what `Run` may replace: the entry found at the destination
/// (`Seen`), and anything standing in for it where the matrix is tested alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PasteStep<Entry = Seen> {
    /// Stop and ask. `can_overwrite` is false for an irreplaceable occupant,
    /// whose replacement is never offered.
    Ask { can_overwrite: bool },
    /// Drop the source without running anything.
    Skip,
    /// Run the source, replacing `replace`, the entry found at its
    /// destination, when that is given.
    Run { replace: Option<Entry> },
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
            asked: None,
        }
    }

    /// What to do with `src`, given what holds its destination name on disk
    /// and the paste's standing answer. When it asks, the entry found there
    /// is recorded as the only one an "overwrite" answer may replace.
    pub(super) fn meet(&mut self, src: &PathInfo) -> PasteStep {
        let found = existing_destination(&self.dest, src);
        let step = step(self.conflicts.standing(), found);
        if let PasteStep::Ask { .. } = step {
            self.asked = found.map(|(_, seen)| seen);
        }
        step
    }

    /// Whether an earlier source of this paste already took `src`'s
    /// destination name, compared folded (`fold_keys`) so that two names a
    /// destination filesystem may treat as one entry count as one. Decided
    /// from the names alone, never the disk, which may not show the earlier
    /// source yet (its work is only queued) and cannot say which names it
    /// folds together.
    pub(super) fn is_claimed(&self, src: &PathInfo) -> bool {
        src.path
            .file_name()
            .is_some_and(|name| fold_keys(name).iter().any(|key| self.claimed.contains(key)))
    }

    /// Records that `src`'s destination name is taken, once its work is
    /// actually running.
    pub(super) fn claim(&mut self, src: &PathInfo) {
        if let Some(name) = src.path.file_name() {
            self.claimed.extend(fold_keys(name));
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

/// The keys of `name` folded the way the filesystems filectrl pastes onto may
/// fold it, so two sources that could land on one entry are never both
/// started:
///
/// - Case, by full case mapping taken to a fixed point: case-insensitive APFS,
///   HFS+, FAT, exFAT, NTFS, SMB, ZFS, and ext4, f2fs, tmpfs and bcachefs
///   casefold. `ẞ`, `ß` and `ss` meet, as do the three sigmas.
/// - Unicode normalization, to the compatibility form NFKD: APFS and HFS+ are
///   normalization-insensitive, Linux casefold normalizes, and ZFS can apply
///   a compatibility form, under which `ﬁ` is `fi`, `²` is `2` and `？` is `?`.
/// - The combining ypogegrammeni, which case mapping turns into `ι`, ordered
///   among the marks around it as the mark it was, since filesystems that fold
///   it do so in different orders.
/// - Default-ignorable code points, dropped: Linux casefold and HFS+ skip them
///   when comparing, so `❤\u{FE0F}` (the emoji variation selector) is `❤`.
/// - The private-use characters CIFS writes for characters Windows forbids
///   (the SFM and SFU mappings, `\u{F022}` for `:` and so on), as those
///   characters.
/// - Georgian Nuskhuri, taken to Mkhedruli, as HFS+'s table folds Asomtavruli.
/// - Trailing dots and spaces, dropped: the Windows family and SMB ignore
///   them. A name made only of them keys as empty, like every other such name.
/// - A `?`, and a byte that is not UTF-8, as one placeholder: CIFS sends a
///   byte it cannot convert as `?`. A name that is not all UTF-8 also gets a
///   second key with every byte outside ASCII, and `i` and `I`, as the
///   placeholder, since a mount with an 8-bit character set reads each byte
///   as a character and folds its case, and Turkish ones fold `İ` and `ı`
///   onto `i` and `I`; two sources share a name when any of their keys match.
///
/// Folding too much only refuses a source that could have been pasted, never
/// loses anything. Aliasing this does not express is not covered:
///
/// - On any mount with an 8-bit character set (vfat, exfat, ntfs3, CIFS), a
///   name that is all UTF-8 against one that is not.
/// - A character set with case pairs among the bytes UTF-8 uses to continue a
///   character (0x80-0xBF: ISO 8859-2, -3 and -15, the cp125x and koi8-r
///   sets) can fold two different UTF-8 names onto one entry; vfat does even
///   when mounted with `utf8`, since it still compares through the character
///   set. Keys cannot cover it without folding every name outside ASCII
///   together.
/// - A FAT or NTFS short name (`LONGFI~1.TXT`), and a Windows stream name
///   (`a.txt::$DATA`).
fn fold_keys(name: &OsStr) -> Vec<String> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = name.as_bytes();
    if let Ok(valid) = std::str::from_utf8(bytes) {
        return vec![fold_text(&readable(valid).collect::<String>())];
    }
    let chunked: String = bytes
        .utf8_chunks()
        .flat_map(|chunk| {
            readable(chunk.valid()).chain(std::iter::repeat_n(UNREADABLE, chunk.invalid().len()))
        })
        .collect();
    // `i` and `I` go too: Turkish 8-bit character sets fold `İ` and `ı` onto
    // them (0xDD and 0xFD in ISO 8859-9).
    let bytewise: String = bytes
        .iter()
        .map(|&byte| match byte {
            b'i' | b'I' => UNREADABLE,
            byte if byte.is_ascii() => char::from(byte),
            _ => UNREADABLE,
        })
        .collect();
    let mut keys = vec![fold_text(&chunked), fold_text(&bytewise)];
    keys.dedup();
    keys
}

/// The characters of `text` a name is compared by: without the
/// default-ignorable ones, and with CIFS's private-use stand-ins as the
/// characters they stand for.
fn readable(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars()
        .filter(|&c| !is_default_ignorable(c))
        .map(windows_character)
}

/// `text` folded for `fold_keys`: normalized, case mapped, with `?` as the
/// placeholder, marks ordered, and trailing dots and spaces dropped.
fn fold_text(text: &str) -> String {
    let decomposed: String = text.nfkd().collect();
    // Lowercase, uppercase, then lowercase again: a single mapping in either
    // direction leaves some letters apart (`ẞ` lowercases to `ß`, which
    // uppercases to `SS`), and this order reaches the fixed point for all.
    // Normalizing the case-mapped text again would change no key, for any
    // code point, so it is not done.
    let cased = decomposed.to_lowercase().to_uppercase().to_lowercase();
    let folded: Vec<char> = cased
        .chars()
        .map(|c| if c == '?' { UNREADABLE } else { c })
        .map(nuskhuri_to_mkhedruli)
        .collect();
    let ordered: String = order_marks(folded).into_iter().collect();
    ordered.trim_end_matches(['.', ' ']).to_string()
}

/// What `fold_keys` puts for `?` and for a byte of a name that is not UTF-8.
const UNREADABLE: char = '\u{FFFD}';

/// `ι`, which case mapping makes of the combining ypogegrammeni (U+0345).
const IOTA: char = '\u{3B9}';

/// The canonical combining class `order_marks` sorts by: a mark's own, and
/// for `ι` the class of the ypogegrammeni it may have been (240).
fn mark_class(c: char) -> u8 {
    if c == IOTA {
        240
    } else {
        unicode_normalization::char::canonical_combining_class(c)
    }
}

/// `chars` with each run of marks, counting `ι` as one, stably sorted by
/// combining class. The ypogegrammeni is a mark that normalization orders
/// last among its neighbours, but case mapping makes it `ι`, which is not;
/// where a filesystem folds it relative to the marks differs, so every order
/// is taken to one. This also puts `ι` after the accents of the letter before
/// it (`αί` keys as `άι`), which only folds more.
fn order_marks(mut chars: Vec<char>) -> Vec<char> {
    let mut start = 0;
    while start < chars.len() {
        if mark_class(chars[start]) == 0 {
            start += 1;
            continue;
        }
        let end = chars[start..]
            .iter()
            .position(|&c| mark_class(c) == 0)
            .map_or(chars.len(), |offset| start + offset);
        chars[start..end].sort_by_key(|&c| mark_class(c));
        start = end;
    }
    chars
}

/// The character CIFS stands a private-use code point in for, or `c` itself.
/// The SFM mapping (Services for Mac, the kernel default `mapposix`) takes
/// U+F001-U+F01F for the controls, U+F020-U+F027 for `" * : < > ? / |`, and
/// U+F028 and U+F029 for a trailing space and dot; the SFU mapping
/// (`mapchars`) takes U+F000 plus the ASCII code of `* : < > ? \ |`.
fn windows_character(c: char) -> char {
    match u32::from(c) {
        code @ 0xF001..=0xF01F => char::from_u32(code - 0xF000).unwrap_or(c),
        0xF020 => '"',
        0xF021 | 0xF02A => '*',
        0xF022 | 0xF03A => ':',
        0xF023 | 0xF03C => '<',
        0xF024 | 0xF03E => '>',
        0xF025 | 0xF03F => '?',
        0xF026 => '/',
        0xF027 | 0xF07C => '|',
        0xF028 => ' ',
        0xF029 => '.',
        0xF05C => '\\',
        _ => c,
    }
}

/// Unicode's Default_Ignorable_Code_Point property (15.1), which Linux casefold
/// drops when it folds a name and HFS+ skips when it compares two.
fn is_default_ignorable(c: char) -> bool {
    matches!(
        u32::from(c),
        0xAD | 0x34F
            | 0x61C
            | 0x115F..=0x1160
            | 0x17B4..=0x17B5
            | 0x180B..=0x180F
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0x3164
            | 0xFE00..=0xFE0F
            | 0xFEFF
            | 0xFFA0
            | 0xFFF0..=0xFFF8
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0000..=0xE0FFF
    )
}

/// Georgian Nuskhuri (U+2D00-U+2D25), which lowercasing Asomtavruli gives, as
/// the Mkhedruli letter (U+10D0-U+10F5) HFS+ folds Asomtavruli to.
fn nuskhuri_to_mkhedruli(c: char) -> char {
    match u32::from(c) {
        code @ 0x2D00..=0x2D25 => char::from_u32(code - 0x2D00 + 0x10D0).unwrap_or(c),
        _ => c,
    }
}

/// What to do with a source, given the paste's standing answer and what already
/// holds its destination name, if anything, with the entry it is. Pure, so the
/// whole answer matrix can be exercised without a filesystem or a worker.
fn step<Entry>(
    standing: Option<ConflictChoice>,
    found: Option<(Occupant, Entry)>,
) -> PasteStep<Entry> {
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

    fn pending(standing: Option<ConflictChoice>) -> PendingPaste {
        let mut pending =
            PendingPaste::new(false, &PathInfo::try_from(Path::new("/")).unwrap(), &[]);
        if let Some(standing) = standing {
            pending.answer(standing);
        }
        pending
    }

    #[test_case(None, None => PasteStep::Run { replace: None } ; "a free name just runs")]
    #[test_case(None, Some(Occupant::Replaceable) => PasteStep::Ask { can_overwrite: true } ; "a file asks, offering overwrite")]
    #[test_case(None, Some(Occupant::Irreplaceable) => PasteStep::Ask { can_overwrite: false } ; "a directory asks, withholding overwrite")]
    #[test_case(Some(ConflictChoice::SkipAll), None => PasteStep::Run { replace: None } ; "skip all does not skip a free name")]
    #[test_case(Some(ConflictChoice::SkipAll), Some(Occupant::Replaceable) => PasteStep::Skip ; "skip all skips a file")]
    #[test_case(Some(ConflictChoice::SkipAll), Some(Occupant::Irreplaceable) => PasteStep::Skip ; "skip all skips a directory")]
    #[test_case(Some(ConflictChoice::OverwriteAll), None => PasteStep::Run { replace: None } ; "overwrite all does not force a free name")]
    #[test_case(Some(ConflictChoice::OverwriteAll), Some(Occupant::Replaceable) => PasteStep::Run { replace: Some("found") } ; "overwrite all replaces the file found")]
    #[test_case(Some(ConflictChoice::OverwriteAll), Some(Occupant::Irreplaceable) => PasteStep::Ask { can_overwrite: false } ; "overwrite all still asks about a directory")]
    fn the_paste_step_matrix(
        standing: Option<ConflictChoice>,
        occupant: Option<Occupant>,
    ) -> PasteStep<&'static str> {
        step(standing, occupant.map(|occupant| (occupant, "found")))
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

    /// `name`'s fold keys, from its bytes.
    fn keys(name: &[u8]) -> Vec<String> {
        use std::os::unix::ffi::OsStrExt;
        fold_keys(OsStr::from_bytes(name))
    }

    /// The one fold key of a name that is all UTF-8.
    fn key(name: &[u8]) -> String {
        let keys = keys(name);
        assert_eq!(1, keys.len(), "{name:?} has one key");
        keys.into_iter().next().unwrap()
    }

    /// Whether any fold key of `a` is one of `b`'s, which is what the queue
    /// refuses the later of two sources for.
    fn fold_together(a: &[u8], b: &[u8]) -> bool {
        let b = keys(b);
        keys(a).iter().any(|key| b.contains(key))
    }

    /// Names a destination may fold into one entry key alike; names every
    /// filesystem keeps apart do not.
    #[test_case("Notes.txt", "notes.txt" => true ; "ascii case")]
    #[test_case("\u{c4}RGER", "\u{e4}rger" => true ; "non-ascii case")]
    #[test_case("stra\u{df}e", "STRASSE" => true ; "sharp s and double s")]
    #[test_case("STRA\u{1e9e}E", "stra\u{df}e" => true ; "capital sharp s and sharp s")]
    #[test_case("\u{1e9e}", "ss" => true ; "capital sharp s and double s")]
    #[test_case("\u{39f}\u{394}\u{39f}\u{3a3}", "\u{3bf}\u{3b4}\u{3bf}\u{3c2}" => true ; "capital and final sigma")]
    #[test_case("\u{3bf}\u{3b4}\u{3bf}\u{3c3}", "\u{3bf}\u{3b4}\u{3bf}\u{3c2}" => true ; "medial and final sigma")]
    #[test_case("caf\u{e9}", "cafe\u{301}" => true ; "precomposed and decomposed")]
    #[test_case("\u{d55c}", "\u{1112}\u{1161}\u{11ab}" => true ; "a hangul syllable and its jamo")]
    #[test_case("\u{3b1}\u{345}\u{301}", "\u{3ac}\u{345}" => true ; "ypogegrammeni out of canonical order")]
    #[test_case("\u{fb01}le", "file" => true ; "a compatibility ligature")]
    #[test_case("file\u{b2}", "file2" => true ; "a superscript digit")]
    #[test_case("\u{ff21}", "a" => true ; "a fullwidth letter")]
    #[test_case("a\u{3f2}b", "a\u{3c2}b" => true ; "a lunate sigma, whose compatibility form is cased")]
    #[test_case("\u{2764}\u{fe0f}.txt", "\u{2764}.txt" => true ; "an emoji variation selector")]
    #[test_case("a\u{200c}b", "ab" => true ; "a zero width non-joiner")]
    #[test_case("a\u{200d}b", "ab" => true ; "a zero width joiner")]
    #[test_case("co\u{ad}op", "coop" => true ; "a soft hyphen")]
    #[test_case("\u{10a0}", "\u{10d0}" => true ; "georgian asomtavruli and mkhedruli")]
    #[test_case("notes.txt.", "notes.txt" => true ; "a trailing dot")]
    #[test_case("notes.txt  ", "notes.txt" => true ; "trailing spaces")]
    #[test_case("NOTES.TXT. ", "notes.txt" => true ; "case, a dot and a space together")]
    #[test_case("CAF\u{c9}", "cafe\u{301}" => true ; "case and normalization together")]
    #[test_case("caf?", "caf\u{fffd}" => true ; "a question mark and the placeholder")]
    #[test_case("caf\u{e9}", "cafe" => false ; "an accent is kept")]
    #[test_case("a", "b" => false ; "different letters")]
    #[test_case("foo", "foo.txt" => false ; "an extension is kept")]
    #[test_case(".foo", "foo" => false ; "a leading dot is kept")]
    #[test_case(" foo", "foo" => false ; "a leading space is kept")]
    #[test_case("a.b", "ab" => false ; "an inner dot is kept")]
    #[test_case("notes.txt\t", "notes.txt" => false ; "a trailing tab is kept")]
    #[test_case("...", " " => true ; "names that are all dots or spaces key alike")]
    #[test_case("...", "." => true ; "a run of dots is one dot")]
    #[test_case("\u{fe0f}.", "\u{fe0f}" => true ; "an ignorable and a dot")]
    #[test_case("\u{2026}", "\u{2026}." => true ; "an ellipsis and a dot")]
    #[test_case("a:b", "a\u{f022}b" => true ; "a colon and its sfm character")]
    #[test_case("x?", "x\u{f025}" => true ; "a question mark and its sfm character")]
    #[test_case("notes.", "notes\u{f029}" => true ; "a trailing dot and its sfm character")]
    #[test_case("notes ", "notes\u{f028}" => true ; "a trailing space and its sfm character")]
    #[test_case("a\u{1}b", "a\u{f001}b" => true ; "a control character and its sfm character")]
    #[test_case("a*b", "a\u{f02a}b" => true ; "an asterisk and its sfu character")]
    #[test_case("a|b", "a\u{f07c}b" => true ; "a pipe and its sfu character")]
    #[test_case("a\\b", "a\u{f05c}b" => true ; "a backslash and its sfu character")]
    #[test_case("\u{306a}\u{305c}\u{ff1f}.txt", "\u{306a}\u{305c}?.txt" => true ; "a fullwidth question mark")]
    #[test_case("\u{2047}", "??" => true ; "a double question mark")]
    #[test_case("\u{3b1}\u{345}\u{e0102}\u{301}", "\u{3b1}\u{3b9}\u{e0102}\u{301}" => true ; "ypogegrammeni around a dropped ignorable")]
    #[test_case("\u{3b1}\u{345}\u{ff9f}", "\u{3b1}\u{3b9}\u{ff9f}" => true ; "ypogegrammeni before a compatibility mark")]
    #[test_case("\u{345}\u{301}", "\u{399}\u{301}" => true ; "ypogegrammeni and a capital iota before a mark")]
    #[test_case("\u{3b1}\u{3b9}\u{301}", "\u{3ac}\u{3b9}" => true ; "an iota after an accent folds past it")]
    #[test_case("\u{10c5}", "\u{10f5}" => true ; "the last asomtavruli and mkhedruli letters")]
    #[test_case("a-b", "ab" => false ; "a hyphen is kept")]
    #[test_case("a_b", "ab" => false ; "an underscore is kept")]
    #[test_case("a\u{180a}b", "ab" => false ; "a mongolian nirugu is kept")]
    #[test_case("a\u{2010}b", "ab" => false ; "a unicode hyphen is kept")]
    #[test_case("a\u{2070}b", "ab" => false ; "a superscript zero is kept")]
    fn names_fold_together(a: &str, b: &str) -> bool {
        fold_together(a.as_bytes(), b.as_bytes())
    }

    /// In a name that is not all UTF-8 every byte outside ASCII folds to one
    /// placeholder, as a `?` does: vfat with an 8-bit character set folds such
    /// bytes by case, and CIFS sends each invalid one as `?`.
    #[test_case(b"caf\xe9", b"caf?" => true ; "an invalid byte and a question mark")]
    #[test_case(b"caf\xe9", b"caf\xe8" => true ; "two invalid bytes")]
    #[test_case(b"\xe9", b"\xc9" => true ; "an invalid byte and its latin-1 capital")]
    #[test_case(b"CAF\xe9", b"caf\xe9" => true ; "case around an invalid byte")]
    #[test_case(b"caf\xe9", b"caf\xe9\xe9" => false ; "one invalid byte and two")]
    #[test_case(b"caf\xe9", b"cafe" => false ; "an invalid byte and a letter")]
    #[test_case(b"caf\xe2\x82", b"caf\xe9" => false ; "each byte of a truncated character is one placeholder")]
    // Latin-1 `Ã` and `ã` are one letter to vfat: every byte outside ASCII is
    // a placeholder once the name holds an invalid one, whatever runs of it
    // would have been valid UTF-8.
    #[test_case(b"\xc3\xa9\x80\xff", b"\xe3\xa9\x80\xff" => true ; "a latin-1 case pair in names that are not utf8")]
    // A name mixing UTF-8 with an invalid byte meets the name CIFS writes for
    // it, where only the invalid byte became `?`.
    #[test_case(b"\xc3\xa9\xff", b"\xc3\xa9?" => true ; "a valid character and an invalid byte")]
    // Latin-5 `İçerik` and `içerik`: ISO 8859-9 folds `İ` onto `i`.
    #[test_case(b"\xdd\xe7erik", b"i\xe7erik" => true ; "a turkish capital dotted i and an ascii i")]
    #[test_case(b"\xfd\xe7", b"I\xe7" => true ; "a turkish dotless i and an ascii capital i")]
    // ASCII outside `i` keeps apart, as the filesystem does.
    #[test_case(b"a-\xff", b"a_\xff" => false ; "two different ascii characters")]
    fn names_that_are_not_utf8_fold_together(a: &[u8], b: &[u8]) -> bool {
        fold_together(a, b)
    }

    /// Only a name that is not all UTF-8 gets the key that drops `i`: an
    /// ASCII name keeps its letters, and keeps apart from one that differs
    /// only in an `i`.
    #[test]
    fn an_ascii_name_keeps_its_i() {
        assert_eq!(vec!["ink".to_string()], keys(b"Ink"));
        assert!(!fold_together(b"ink", b"unk"));
    }

    /// Both ends of every default-ignorable range are dropped.
    #[test]
    fn every_default_ignorable_range_is_dropped_from_end_to_end() {
        let ends = [
            0xAD, 0x34F, 0x61C, 0x115F, 0x1160, 0x17B4, 0x17B5, 0x180B, 0x180F, 0x200B, 0x200F,
            0x202A, 0x202E, 0x2060, 0x206F, 0x3164, 0xFE00, 0xFE0F, 0xFEFF, 0xFFA0, 0xFFF0, 0xFFF8,
            0x1BCA0, 0x1BCA3, 0x1D173, 0x1D17A, 0xE0000, 0xE0FFF,
        ];
        for code in ends {
            let c = char::from_u32(code).unwrap();
            assert_eq!(key(b"ab"), key(format!("a{c}b").as_bytes()), "U+{code:04X}");
        }
    }

    /// Folding a folded name changes nothing, so a key never depends on how
    /// many times a name was folded.
    #[test]
    fn a_fold_key_is_a_fixed_point() {
        let names = [
            "Notes.txt",
            "STRA\u{1e9e}E",
            "stra\u{df}e",
            "\u{39f}\u{394}\u{39f}\u{3a3}",
            "CAF\u{c9}",
            "\u{d55c}",
            "\u{3b1}\u{345}\u{301}",
            "\u{fb01}le\u{b2}",
            "\u{ff21}",
            "\u{2764}\u{fe0f}.txt",
            "co\u{ad}op",
            "\u{10a0}\u{2d00}",
            "NOTES.TXT. ",
            "caf?",
            "...",
            " ",
            "\u{130}\u{131}",
            "\u{ff1f}",
            "\u{2047}",
            "a\u{f022}b\u{f029}",
            "a\u{f03f}\u{f07c}",
            "\u{3b1}\u{345}\u{301}\u{3b9}",
            "\u{345}\u{301}",
            "\u{fffd}",
        ];
        for name in names {
            let once = key(name.as_bytes());
            assert_eq!(once, key(once.as_bytes()), "{name:?}");
        }
    }

    /// The queue's claims are decided from folded names alone, with nothing
    /// read from the disk.
    #[test]
    fn a_claim_takes_every_name_that_folds_onto_it() {
        let source = |path: &str| {
            let mut info = PathInfo::try_from(Path::new("/")).unwrap();
            info.path = PathBuf::from(path);
            info
        };
        let mut pending = pending(None);

        pending.claim(&source("/nowhere/one/Notes.txt"));

        assert!(pending.is_claimed(&source("/nowhere/two/notes.txt")));
        assert!(pending.is_claimed(&source("/nowhere/two/NOTES.TXT.")));
        assert!(!pending.is_claimed(&source("/nowhere/two/notes.md")));
    }

    /// A name that is not all UTF-8 claims under both its keys, and one
    /// matching either is taken: these two share only the key that reads
    /// every byte outside ASCII as the placeholder, as vfat with an 8-bit
    /// character set would.
    #[test]
    fn a_claim_takes_a_name_matching_any_of_its_keys() {
        use std::os::unix::ffi::OsStrExt;
        let source = |name: &[u8]| {
            let mut info = PathInfo::try_from(Path::new("/")).unwrap();
            info.path = Path::new("/nowhere").join(OsStr::from_bytes(name));
            info
        };
        let (first, second) = (b"\xc3\xa9\x80\xff", b"\xe3\xa9\x80\xff");
        assert_eq!(
            fold_keys(OsStr::from_bytes(first))[1],
            fold_keys(OsStr::from_bytes(second))[1]
        );
        assert_ne!(
            fold_keys(OsStr::from_bytes(first))[0],
            fold_keys(OsStr::from_bytes(second))[0]
        );
        let mut pending = pending(None);

        pending.claim(&source(first));

        assert!(pending.is_claimed(&source(second)));
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

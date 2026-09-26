//! The mime-apps spec's association and default resolution algorithm.
//!
//! <https://specifications.freedesktop.org/mime-apps/latest-single/>

use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

/// A desktop file id, including the ".desktop" suffix (unlike
/// `DesktopEntry::appid`).
pub(super) type DesktopId = String;

const ADDED_ASSOCIATIONS: &str = "Added Associations";
const DEFAULT_APPLICATIONS: &str = "Default Applications";
const REMOVED_ASSOCIATIONS: &str = "Removed Associations";

/// One parsed `mimeapps.list`, preserving value order (preference).
#[derive(Debug, Default)]
pub(super) struct MimeAppsList {
    added: BTreeMap<String, Vec<DesktopId>>,
    defaults: BTreeMap<String, Vec<DesktopId>>,
    /// A `$desktop-mimeapps.list`, which may only set defaults.
    desktop_specific: bool,
    removed: BTreeMap<String, Vec<DesktopId>>,
}

impl MimeAppsList {
    /// Parse the groups this spec defines, skipping unknown groups and
    /// malformed lines.
    pub(super) fn parse(desktop_specific: bool, text: &str) -> Self {
        let mut list = Self {
            desktop_specific,
            ..Self::default()
        };
        let mut group: Option<&str> = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                group = Some(name.trim());
                continue;
            }
            let Some(group) = group else { continue };
            let Some((mime, ids)) = line.split_once('=') else {
                continue;
            };
            let target = match group {
                ADDED_ASSOCIATIONS => &mut list.added,
                DEFAULT_APPLICATIONS => &mut list.defaults,
                REMOVED_ASSOCIATIONS => &mut list.removed,
                _ => continue,
            };
            let ids: Vec<DesktopId> = ids
                .split(';')
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(ToString::to_string)
                .collect();
            if !ids.is_empty() {
                target
                    .entry(mime.trim().to_string())
                    .or_default()
                    .extend(ids);
            }
        }
        list
    }

    /// Rewrite every MIME key through `canonicalize`, merging aliases.
    pub(super) fn canonicalize_keys(&mut self, canonicalize: impl Fn(&str) -> String) {
        for map in [&mut self.added, &mut self.defaults, &mut self.removed] {
            let mut canonicalized: BTreeMap<String, Vec<DesktopId>> = BTreeMap::new();
            for (mime, ids) in std::mem::take(map) {
                canonicalized
                    .entry(canonicalize(&mime))
                    .or_default()
                    .extend(ids);
            }
            *map = canonicalized;
        }
    }
}

/// The `.desktop` files reachable from one `applications` directory.
#[derive(Debug, Default)]
pub(super) struct AppDirIndex {
    /// Desktop id to the file that defines it.
    pub(super) by_id: BTreeMap<DesktopId, PathBuf>,
    /// Desktop id to the MIME types its `MimeType=` key declares.
    pub(super) mime_types: BTreeMap<DesktopId, Vec<String>>,
}

/// One precedence level: its lists and, for a data directory, its applications.
#[derive(Debug, Default)]
pub(super) struct Level {
    pub(super) apps: Option<AppDirIndex>,
    /// Desktop-specific lists first, then the generic one.
    pub(super) lists: Vec<MimeAppsList>,
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct Associations {
    /// The configured defaults, most specific type then highest precedence
    /// first, less removed ones. Unchecked: the first the picker can offer is
    /// the default. Not required to be associated, as desktops do.
    pub(super) defaults: Vec<DesktopId>,
    /// The associated applications, most preferred first.
    pub(super) ordered: Vec<DesktopId>,
}

/// Resolve which applications are associated with a file. `mime_chain` runs
/// most specific first, `levels` highest precedence first.
pub(super) fn associations(levels: &[Level], mime_chain: &[String]) -> Associations {
    let mut ordered: Vec<DesktopId> = Vec::new();
    let mut seen: HashSet<DesktopId> = HashSet::new();
    let mut defaults: Vec<DesktopId> = Vec::new();
    // A removal holds for every later source and less specific type, as in GLib.
    let mut removed: HashSet<&DesktopId> = HashSet::new();

    for mime in mime_chain {
        // An id defined in a higher applications directory masks lower
        // associations of that id, which name another file. Restarts per type.
        let mut shadowed: HashSet<&DesktopId> = HashSet::new();

        for level in levels {
            for list in &level.lists {
                for id in list.defaults.get(mime).into_iter().flatten() {
                    if !removed.contains(id) {
                        defaults.push(id.clone());
                    }
                }
                if list.desktop_specific {
                    continue;
                }
                for id in list.added.get(mime).into_iter().flatten() {
                    if !removed.contains(id) && !shadowed.contains(id) && seen.insert(id.clone()) {
                        ordered.push(id.clone());
                    }
                }
                removed.extend(list.removed.get(mime).into_iter().flatten());
            }

            let Some(apps) = &level.apps else { continue };
            for (id, types) in &apps.mime_types {
                if types.contains(mime)
                    && !removed.contains(id)
                    && !shadowed.contains(id)
                    && seen.insert(id.clone())
                {
                    ordered.push(id.clone());
                }
            }
            shadowed.extend(apps.by_id.keys());
        }
    }

    Associations { defaults, ordered }
}

/// Find the file that defines a desktop id; the first directory wins.
pub(super) fn resolve<'a>(levels: &'a [Level], id: &str) -> Option<&'a Path> {
    levels
        .iter()
        .filter_map(|level| level.apps.as_ref())
        .find_map(|apps| apps.by_id.get(id))
        .map(PathBuf::as_path)
}

/// The desktop id of a `.desktop` file under `app_dir`: `kde4/konsole.desktop`
/// is `kde4-konsole.desktop`.
pub(super) fn desktop_id(app_dir: &Path, file: &Path) -> Option<DesktopId> {
    let relative = file.strip_prefix(app_dir).ok()?;
    let id = relative
        .iter()
        .map(|component| component.to_str())
        .collect::<Option<Vec<_>>>()?
        .join("-");
    id.ends_with(".desktop").then_some(id)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use test_case::test_case;

    use super::{AppDirIndex, DesktopId, Level, MimeAppsList, associations, desktop_id, resolve};

    const TEXT: &str = "text/plain";

    fn ids(ids: &[&str]) -> Vec<DesktopId> {
        ids.iter().map(ToString::to_string).collect()
    }

    fn app_dir(entries: &[(&str, &[&str])]) -> AppDirIndex {
        app_dir_in("/apps", entries)
    }

    fn app_dir_in(dir: &str, entries: &[(&str, &[&str])]) -> AppDirIndex {
        let mut index = AppDirIndex::default();
        for (id, mime_types) in entries {
            index
                .by_id
                .insert(id.to_string(), PathBuf::from(format!("{dir}/{id}")));
            index.mime_types.insert(id.to_string(), ids(mime_types));
        }
        index
    }

    fn level(lists: Vec<MimeAppsList>, apps: Option<AppDirIndex>) -> Level {
        Level { apps, lists }
    }

    #[test]
    fn parse_reads_every_group() {
        let list = MimeAppsList::parse(
            false,
            "# a comment\n\
             \n\
             [Default Applications]\n\
             text/plain=d.desktop\n\
             \n\
             [Added Associations]\n\
             text/plain=a.desktop;b.desktop;\n\
             \n\
             [Removed Associations]\n\
             text/plain=r.desktop\n",
        );
        assert_eq!(ids(&["d.desktop"]), list.defaults[TEXT]);
        assert_eq!(ids(&["a.desktop", "b.desktop"]), list.added[TEXT]);
        assert_eq!(ids(&["r.desktop"]), list.removed[TEXT]);
    }

    #[test_case("[Added Associations]\ntext/plain = a.desktop ; b.desktop " ; "surrounding whitespace")]
    #[test_case("[Added Associations]\ntext/plain=a.desktop;;b.desktop;;" ; "empty entries")]
    #[test_case("[Added Associations]\ntext/plain=a.desktop\ntext/plain=b.desktop" ; "a repeated key accumulates")]
    fn parse_normalizes_values(text: &str) {
        let list = MimeAppsList::parse(false, text);
        assert_eq!(ids(&["a.desktop", "b.desktop"]), list.added[TEXT]);
    }

    #[test_case("text/plain=a.desktop" ; "no group header")]
    #[test_case("[Unknown Group]\ntext/plain=a.desktop" ; "unknown group")]
    #[test_case("[Added Associations]\nno equals sign" ; "malformed line")]
    #[test_case("[Added Associations]\ntext/plain=;;" ; "no usable ids")]
    #[test_case("[Added Associations]\n#text/plain=a.desktop" ; "a commented-out association")]
    fn parse_skips(text: &str) {
        assert!(MimeAppsList::parse(false, text).added.is_empty());
    }

    #[test]
    fn associations_preserve_added_order_across_precedence() {
        let levels = vec![
            level(
                vec![MimeAppsList::parse(
                    false,
                    "[Added Associations]\ntext/plain=b.desktop;a.desktop",
                )],
                None,
            ),
            level(vec![], Some(app_dir(&[("c.desktop", &[TEXT])]))),
        ];
        let result = associations(&levels, &ids(&[TEXT]));
        assert_eq!(
            ids(&["b.desktop", "a.desktop", "c.desktop"]),
            result.ordered
        );
    }

    #[test]
    fn an_id_added_at_two_levels_is_ranked_once() {
        let added = || {
            MimeAppsList::parse(
                false,
                "[Added Associations]\ntext/plain=a.desktop;b.desktop",
            )
        };
        let levels = vec![level(vec![added()], None), level(vec![added()], None)];
        assert_eq!(
            ids(&["a.desktop", "b.desktop"]),
            associations(&levels, &ids(&[TEXT])).ordered
        );
    }

    #[test]
    fn canonicalizing_merges_an_alias_into_its_canonical_key() {
        let mut list = MimeAppsList::parse(
            false,
            "[Added Associations]\n\
             text/plain=b.desktop\n\
             text/x-alias=a.desktop\n\
             [Removed Associations]\n\
             text/x-alias=r.desktop\n",
        );

        list.canonicalize_keys(|mime| if mime == "text/x-alias" { TEXT } else { mime }.to_string());

        assert_eq!(ids(&["b.desktop", "a.desktop"]), list.added[TEXT]);
        assert_eq!(ids(&["r.desktop"]), list.removed[TEXT]);
        assert_eq!(1, list.added.len());
    }

    #[test]
    fn a_removed_association_suppresses_a_lower_precedence_addition() {
        let levels = vec![
            level(
                vec![MimeAppsList::parse(
                    false,
                    "[Removed Associations]\ntext/plain=a.desktop",
                )],
                None,
            ),
            level(vec![], Some(app_dir(&[("a.desktop", &[TEXT])]))),
        ];
        assert_eq!(
            Vec::<DesktopId>::new(),
            associations(&levels, &ids(&[TEXT])).ordered
        );
    }

    #[test]
    fn a_removed_association_suppresses_a_lower_precedence_lists_addition() {
        let levels = vec![
            level(
                vec![MimeAppsList::parse(
                    false,
                    "[Removed Associations]\ntext/plain=a.desktop",
                )],
                None,
            ),
            level(
                vec![MimeAppsList::parse(
                    false,
                    "[Added Associations]\ntext/plain=a.desktop;b.desktop",
                )],
                Some(app_dir(&[("a.desktop", &[]), ("b.desktop", &[])])),
            ),
        ];

        assert_eq!(
            ids(&["b.desktop"]),
            associations(&levels, &ids(&[TEXT])).ordered
        );
    }

    #[test]
    fn a_directory_shadows_the_same_id_below_it() {
        let levels = vec![
            level(vec![], Some(app_dir(&[("a.desktop", &[TEXT])]))),
            level(vec![], Some(app_dir(&[("a.desktop", &["image/png"])]))),
        ];
        assert_eq!(
            Vec::<DesktopId>::new(),
            associations(&levels, &ids(&["image/png"])).ordered
        );
        assert_eq!(
            ids(&["a.desktop"]),
            associations(&levels, &ids(&[TEXT])).ordered
        );
    }

    #[test]
    fn a_desktop_specific_list_contributes_only_a_default() {
        let levels = vec![
            level(
                vec![MimeAppsList::parse(
                    true,
                    "[Default Applications]\ntext/plain=b.desktop\n\
                     [Added Associations]\ntext/plain=ignored.desktop",
                )],
                None,
            ),
            level(
                vec![],
                Some(app_dir(&[("a.desktop", &[TEXT]), ("b.desktop", &[TEXT])])),
            ),
        ];
        let result = associations(&levels, &ids(&[TEXT]));
        assert_eq!(ids(&["a.desktop", "b.desktop"]), result.ordered);
        assert_eq!(ids(&["b.desktop"]), result.defaults);
    }

    #[test]
    fn defaults_run_most_specific_type_first_then_by_precedence() {
        let levels = vec![
            level(
                vec![MimeAppsList::parse(
                    false,
                    "[Default Applications]\ntext/plain=plain-high.desktop",
                )],
                None,
            ),
            level(
                vec![MimeAppsList::parse(
                    false,
                    "[Default Applications]\n\
                     text/plain=plain-low.desktop\n\
                     text/markdown=markdown-low.desktop",
                )],
                None,
            ),
        ];
        assert_eq!(
            ids(&[
                "markdown-low.desktop",
                "plain-high.desktop",
                "plain-low.desktop"
            ]),
            associations(&levels, &ids(&["text/markdown", TEXT])).defaults
        );
    }

    #[test]
    fn a_higher_directory_masks_a_lower_lists_addition_of_the_same_id() {
        let levels = vec![
            level(vec![], Some(app_dir(&[("a.desktop", &["image/png"])]))),
            level(
                vec![MimeAppsList::parse(
                    false,
                    "[Added Associations]\ntext/plain=a.desktop;b.desktop",
                )],
                Some(app_dir(&[("a.desktop", &[TEXT]), ("b.desktop", &[])])),
            ),
        ];
        assert_eq!(
            ids(&["b.desktop"]),
            associations(&levels, &ids(&[TEXT])).ordered
        );
    }

    #[test]
    fn a_removed_default_is_not_listed() {
        let levels = vec![
            level(
                vec![MimeAppsList::parse(
                    false,
                    "[Removed Associations]\ntext/plain=gone.desktop",
                )],
                None,
            ),
            level(
                vec![MimeAppsList::parse(
                    false,
                    "[Default Applications]\ntext/plain=gone.desktop;kept.desktop",
                )],
                None,
            ),
        ];
        assert_eq!(
            ids(&["kept.desktop"]),
            associations(&levels, &ids(&[TEXT])).defaults
        );
    }

    #[test]
    fn a_more_specific_type_outranks_its_parent() {
        let levels = vec![level(
            vec![],
            Some(app_dir(&[
                ("generic.desktop", &[TEXT]),
                ("specific.desktop", &["text/markdown"]),
            ])),
        )];
        let result = associations(&levels, &ids(&["text/markdown", TEXT]));
        assert_eq!(
            ids(&["specific.desktop", "generic.desktop"]),
            result.ordered
        );
    }

    #[test]
    fn a_removal_for_a_type_also_holds_for_its_parents() {
        // a.desktop reaches markdown only through text/plain.
        let levels = vec![
            level(
                vec![MimeAppsList::parse(
                    false,
                    "[Removed Associations]\ntext/markdown=a.desktop",
                )],
                None,
            ),
            level(vec![], Some(app_dir(&[("a.desktop", &[TEXT])]))),
        ];
        assert_eq!(
            Vec::<DesktopId>::new(),
            associations(&levels, &ids(&["text/markdown", TEXT])).ordered
        );
        assert_eq!(
            ids(&["a.desktop"]),
            associations(&levels, &ids(&[TEXT])).ordered
        );
    }

    #[test]
    fn resolve_returns_the_highest_precedence_definition() {
        let levels = vec![
            level(vec![], Some(app_dir_in("/user", &[("a.desktop", &[])]))),
            level(
                vec![],
                Some(app_dir_in(
                    "/system",
                    &[("a.desktop", &[]), ("b.desktop", &[])],
                )),
            ),
        ];
        assert_eq!(
            Some(Path::new("/user/a.desktop")),
            resolve(&levels, "a.desktop")
        );
        assert_eq!(
            Some(Path::new("/system/b.desktop")),
            resolve(&levels, "b.desktop")
        );
        assert_eq!(None, resolve(&levels, "missing.desktop"));
    }

    #[test_case("/apps/foo.desktop", Some("foo.desktop") ; "flat")]
    #[test_case("/apps/kde4/konsole.desktop", Some("kde4-konsole.desktop") ; "nested")]
    #[test_case("/apps/a/b/c.desktop", Some("a-b-c.desktop") ; "deeply nested")]
    #[test_case("/apps/mimeapps.list", None ; "not a desktop file")]
    #[test_case("/elsewhere/foo.desktop", None ; "outside the directory")]
    fn desktop_id_derives(file: &str, expected: Option<&str>) {
        let actual = desktop_id(Path::new("/apps"), Path::new(file));
        assert_eq!(expected.map(ToString::to_string), actual);
    }
}

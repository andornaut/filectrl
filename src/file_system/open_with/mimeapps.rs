//! The mime-apps spec's association and default resolution algorithm.
//!
//! <https://specifications.freedesktop.org/mime-apps/latest-single/>

use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

/// A desktop file id, including the ".desktop" suffix. `DesktopEntry::appid`
/// strips the suffix, but mimeapps.list keys it with the suffix intact.
pub(super) type DesktopId = String;

const ADDED_ASSOCIATIONS: &str = "Added Associations";
const DEFAULT_APPLICATIONS: &str = "Default Applications";
const REMOVED_ASSOCIATIONS: &str = "Removed Associations";

/// One parsed `mimeapps.list`. Value order is preserved: the order in
/// `[Added Associations]` reflects preference.
#[derive(Debug, Default)]
pub(super) struct MimeAppsList {
    added: BTreeMap<String, Vec<DesktopId>>,
    defaults: BTreeMap<String, Vec<DesktopId>>,
    /// True for a `$desktop-mimeapps.list`, which the spec allows to specify
    /// only the default application, never to add or remove associations.
    desktop_specific: bool,
    removed: BTreeMap<String, Vec<DesktopId>>,
}

impl MimeAppsList {
    /// Parse the desktop-entry style groups this spec defines. Unknown groups
    /// and malformed lines are skipped rather than failing the whole lookup,
    /// because one bad file must not hide every application on the system.
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

    /// Rewrite every MIME key through `canonicalize`, so that a list keyed by
    /// an alias still matches the canonical type the lookup chain carries.
    pub(super) fn canonicalize_keys(&mut self, canonicalize: impl Fn(&str) -> String) {
        for map in [&mut self.added, &mut self.defaults, &mut self.removed] {
            let mut canonicalized: BTreeMap<String, Vec<DesktopId>> = BTreeMap::new();
            // An alias and its canonical form can both be keyed, so merge
            // rather than overwrite.
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

/// One rung of the precedence ladder: the lists that apply at this rung, plus
/// the applications directory scanned alongside them. The `$XDG_CONFIG_*` rungs
/// carry lists but no directory.
#[derive(Debug, Default)]
pub(super) struct Level {
    pub(super) apps: Option<AppDirIndex>,
    /// Desktop-specific lists first, then the generic one.
    pub(super) lists: Vec<MimeAppsList>,
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct Associations {
    /// The configured default when one names an installed application, and
    /// otherwise the most preferred association, which is the fallback the
    /// spec ends with. `xdg-mime query default` and `gio mime` report that
    /// same fallback, so the picker's marker agrees with those tools.
    pub(super) default: Option<DesktopId>,
    pub(super) ordered: Vec<DesktopId>,
}

/// Resolve which applications are associated with a file, most preferred first.
///
/// `mime_chain` runs most specific to least specific: the guessed type, then
/// its parents from the subclass graph, then `all/allfiles` and `all/all`.
/// `levels` runs highest precedence first.
pub(super) fn associations(levels: &[Level], mime_chain: &[String]) -> Associations {
    let mut ordered: Vec<DesktopId> = Vec::new();
    let mut seen: HashSet<DesktopId> = HashSet::new();
    // Collected in (type, then precedence) order and validated at the end.
    let mut default_candidates: Vec<DesktopId> = Vec::new();

    for mime in mime_chain {
        // Both exclusions are keyed by MIME type, so they restart here.
        // `ordered` and `seen` do not: an id already ranked for a more specific
        // type keeps its rank.
        //
        // `[Removed Associations]` cancels an id outright and so applies to every
        // later source. An id already defined by a higher precedence applications
        // directory is excluded from the directory scan alone, to avoid adding
        // the same file twice; the spec does not let that cancel an association a
        // lower level states explicitly.
        let mut removed: HashSet<&DesktopId> = HashSet::new();
        let mut shadowed: HashSet<&DesktopId> = HashSet::new();

        for level in levels {
            for list in &level.lists {
                for id in list.defaults.get(mime).into_iter().flatten() {
                    if !removed.contains(id) {
                        default_candidates.push(id.clone());
                    }
                }
                if list.desktop_specific {
                    continue;
                }
                for id in list.added.get(mime).into_iter().flatten() {
                    if !removed.contains(id) && seen.insert(id.clone()) {
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

    // The most specific type's highest precedence default wins and is offered
    // first, whatever position the directory scan gave it. Only a default whose
    // desktop file exists counts, so an entry left by an uninstalled application
    // does not take the marker with it.
    //
    // The spec also requires the default to be an associated application, but
    // every desktop honours an explicit default anyway, so one that is not
    // associated is promoted rather than skipped.
    let default = default_candidates
        .into_iter()
        .find(|id| resolve(levels, id).is_some());
    if let Some(id) = &default {
        ordered.retain(|existing| existing != id);
        ordered.insert(0, id.clone());
    }
    let default = default.or_else(|| ordered.first().cloned());
    Associations { default, ordered }
}

/// Find the file that defines a desktop id. The first directory to define it
/// wins, which is the same shadowing rule `associations` applies.
pub(super) fn resolve<'a>(levels: &'a [Level], id: &str) -> Option<&'a Path> {
    levels
        .iter()
        .filter_map(|level| level.apps.as_ref())
        .find_map(|apps| apps.by_id.get(id))
        .map(PathBuf::as_path)
}

/// The desktop id of a `.desktop` file found under `app_dir`. Subdirectory
/// components are joined with '-', so `<dir>/kde4/konsole.desktop` is
/// `kde4-konsole.desktop`.
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
        let mut index = AppDirIndex::default();
        for (id, mime_types) in entries {
            index
                .by_id
                .insert(id.to_string(), PathBuf::from(format!("/apps/{id}")));
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
        assert_eq!(Some("b.desktop".to_string()), result.default);
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
    fn a_directory_shadows_the_same_id_below_it() {
        // Only the higher precedence definition of a.desktop exists, so the
        // types the lower one declares are never seen.
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
        assert_eq!(ids(&["b.desktop", "a.desktop"]), result.ordered);
        assert_eq!(Some("b.desktop".to_string()), result.default);
    }

    #[test]
    fn an_associated_default_is_hoisted_above_the_directory_scan_order() {
        // The scan orders ids alphabetically, so without the hoist the default
        // would sit second and the picker would preselect the wrong row.
        let levels = vec![
            level(
                vec![MimeAppsList::parse(
                    false,
                    "[Default Applications]\ntext/plain=zed.desktop",
                )],
                None,
            ),
            level(
                vec![],
                Some(app_dir(&[("a.desktop", &[TEXT]), ("zed.desktop", &[TEXT])])),
            ),
        ];
        let result = associations(&levels, &ids(&[TEXT]));
        assert_eq!(ids(&["zed.desktop", "a.desktop"]), result.ordered);
        assert_eq!(Some("zed.desktop".to_string()), result.default);
    }

    #[test]
    fn an_unassociated_default_is_promoted_to_the_top() {
        // chosen.desktop is installed but declares no MimeType of its own.
        let levels = vec![
            level(
                vec![MimeAppsList::parse(
                    false,
                    "[Default Applications]\ntext/plain=chosen.desktop",
                )],
                None,
            ),
            level(
                vec![],
                Some(app_dir(&[("a.desktop", &[TEXT]), ("chosen.desktop", &[])])),
            ),
        ];
        let result = associations(&levels, &ids(&[TEXT]));
        assert_eq!(ids(&["chosen.desktop", "a.desktop"]), result.ordered);
        assert_eq!(Some("chosen.desktop".to_string()), result.default);
    }

    #[test]
    fn a_default_naming_an_uninstalled_application_is_ignored() {
        // A leftover entry for an application that has since been removed must
        // not take the default marker with it.
        let levels = vec![
            level(
                vec![MimeAppsList::parse(
                    false,
                    "[Default Applications]\ntext/plain=uninstalled.desktop",
                )],
                None,
            ),
            level(vec![], Some(app_dir(&[("a.desktop", &[TEXT])]))),
        ];
        let result = associations(&levels, &ids(&[TEXT]));
        assert_eq!(ids(&["a.desktop"]), result.ordered);
        assert_eq!(Some("a.desktop".to_string()), result.default);
    }

    #[test]
    fn shadowing_does_not_cancel_an_explicit_association_below_it() {
        // The user's own copy of a.desktop shadows the system file for the
        // directory scan, but the system list's explicit association stands.
        let levels = vec![
            level(vec![], Some(app_dir(&[("a.desktop", &["image/png"])]))),
            level(
                vec![MimeAppsList::parse(
                    false,
                    "[Added Associations]\ntext/plain=a.desktop",
                )],
                Some(app_dir(&[("a.desktop", &[TEXT])])),
            ),
        ];
        assert_eq!(
            ids(&["a.desktop"]),
            associations(&levels, &ids(&[TEXT])).ordered
        );
    }

    #[test]
    fn a_removed_default_is_not_promoted() {
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
                    "[Default Applications]\ntext/plain=gone.desktop",
                )],
                Some(app_dir(&[("a.desktop", &[TEXT])])),
            ),
        ];
        let result = associations(&levels, &ids(&[TEXT]));
        assert_eq!(ids(&["a.desktop"]), result.ordered);
        assert_eq!(Some("a.desktop".to_string()), result.default);
    }

    #[test]
    fn with_no_configured_default_the_first_association_is_the_default() {
        // The spec, and so xdg-mime and gio, fall back to the most
        // preferred association when nothing is configured.
        let levels = vec![level(
            vec![],
            Some(app_dir(&[("a.desktop", &[TEXT]), ("b.desktop", &[TEXT])])),
        )];
        let result = associations(&levels, &ids(&[TEXT]));
        assert_eq!(ids(&["a.desktop", "b.desktop"]), result.ordered);
        assert_eq!(Some("a.desktop".to_string()), result.default);
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
        assert_eq!(Some("specific.desktop".to_string()), result.default);
    }

    #[test]
    fn a_removal_for_one_type_does_not_leak_into_another() {
        let levels = vec![
            level(
                vec![MimeAppsList::parse(
                    false,
                    "[Removed Associations]\ntext/markdown=a.desktop",
                )],
                None,
            ),
            level(
                vec![],
                Some(app_dir(&[("a.desktop", &[TEXT, "text/markdown"])])),
            ),
        ];
        let result = associations(&levels, &ids(&["text/markdown", TEXT]));
        assert_eq!(ids(&["a.desktop"]), result.ordered);
    }

    #[test]
    fn resolve_returns_the_highest_precedence_definition() {
        let levels = vec![
            level(vec![], Some(app_dir(&[("a.desktop", &[])]))),
            level(
                vec![],
                Some(app_dir(&[("a.desktop", &[]), ("b.desktop", &[])])),
            ),
        ];
        assert_eq!(
            Some(Path::new("/apps/a.desktop")),
            resolve(&levels, "a.desktop")
        );
        assert_eq!(
            Some(Path::new("/apps/b.desktop")),
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

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, anyhow};
use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use serde::Deserialize;

/// An application action that can be triggered by a key press.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Action {
    // Global
    CancelTask,
    ClearAlerts,
    ClearProgress,
    Quit,
    ResetView,
    ToggleHelp,

    // Navigation (filesystem)
    GoToParentDirectory,
    GoToPreviousDirectory,
    GoHome,
    Open,
    OpenCurrentDirectory,
    OpenNewWindow,
    OpenWith,
    Refresh,

    // Selection
    SelectNext,
    SelectPrevious,
    SelectFirst,
    SelectLast,
    SelectMiddle,
    SelectFirstVisible,
    SelectMiddleVisible,
    SelectLastVisible,
    PageUp,
    PageDown,

    // Marks
    ToggleMark,
    RangeMark,

    // Clipboard
    Copy,
    Cut,
    Paste,

    // File operations
    AddBookmark,
    Chmod,
    CreateDirectory,
    Delete,
    Filter,
    Goto,
    Rename,
    Search,
    GetBookmarks,

    // Sort
    SortByModified,
    SortByName,
    SortBySize,
    ToggleShowHidden,

    // Prompt
    PromptCancel,
    PromptAcceptSuggestion,
    PromptNextSuggestion,
    PromptPreviousSuggestion,
    PromptCopy,
    PromptCut,
    PromptPaste,
    PromptReset,
    PromptSelectAll,
    PromptSubmit,
}

/// A key combination (key code + modifiers).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct KeyCombo {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl KeyCombo {
    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }
}

/// TOML keybinding value: either a single key string or an array of key strings.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum KeySpec {
    Single(String),
    Multiple(Vec<String>),
}

type BindingList = Vec<(Action, Vec<KeyCombo>)>;

/// Declares the `TomlKeybindings` struct (one `KeySpec` field per binding) and
/// its `to_bindings` conversion from a single `field => Action` table per mode.
macro_rules! keybindings {
    (
        normal { $($n_field:ident => $n_action:ident),+ $(,)? }
        prompt { $($p_field:ident => $p_action:ident),+ $(,)? }
    ) => {
        /// Keybindings from the TOML `[keybindings]` section.
        /// All fields are required; defaults are provided by the embedded default_config.toml.
        #[derive(Debug, Deserialize)]
        pub struct TomlKeybindings {
            $($n_field: KeySpec,)+
            $($p_field: KeySpec,)+
        }

        impl TomlKeybindings {
            /// Convert TOML fields into (normal, prompt) binding lists.
            fn to_bindings(&self) -> Result<(BindingList, BindingList)> {
                // Hardcoded-only actions (no TOML fields, but must be in the binding
                // list so that hardcoded keys are inserted into the action map)
                let mut normal: BindingList = vec![(Action::ResetView, vec![])];
                let mut prompt: BindingList = vec![
                    (Action::PromptCancel, vec![]),
                    (Action::PromptAcceptSuggestion, vec![]),
                    (Action::PromptNextSuggestion, vec![]),
                    (Action::PromptPreviousSuggestion, vec![]),
                ];

                $(
                    normal.push((
                        Action::$n_action,
                        parse_key_spec(&self.$n_field).with_context(|| {
                            format!("Invalid keybinding for {:?}", Action::$n_action)
                        })?,
                    ));
                )+
                $(
                    prompt.push((
                        Action::$p_action,
                        parse_key_spec(&self.$p_field).with_context(|| {
                            format!("Invalid keybinding for {:?}", Action::$p_action)
                        })?,
                    ));
                )+

                Ok((normal, prompt))
            }
        }
    };
}

keybindings! {
    normal {
        back => GoToParentDirectory,
        go_to_previous_directory => GoToPreviousDirectory,
        add_bookmark => AddBookmark,
        cancel_task => CancelTask,
        chmod => Chmod,
        clear_alerts => ClearAlerts,
        clear_progress => ClearProgress,
        copy => Copy,
        create_directory => CreateDirectory,
        cut => Cut,
        delete => Delete,
        filter => Filter,
        go_home => GoHome,
        goto => Goto,
        open => Open,
        open_current_directory => OpenCurrentDirectory,
        open_new_window => OpenNewWindow,
        open_with => OpenWith,
        page_down => PageDown,
        page_up => PageUp,
        paste => Paste,
        quit => Quit,
        range_mark => RangeMark,
        refresh => Refresh,
        rename => Rename,
        search => Search,
        show_bookmarks => GetBookmarks,
        select_first => SelectFirst,
        select_last => SelectLast,
        select_middle => SelectMiddle,
        select_first_visible => SelectFirstVisible,
        select_middle_visible => SelectMiddleVisible,
        select_last_visible => SelectLastVisible,
        select_next => SelectNext,
        select_previous => SelectPrevious,
        sort_by_modified => SortByModified,
        sort_by_name => SortByName,
        sort_by_size => SortBySize,
        toggle_help => ToggleHelp,
        toggle_mark => ToggleMark,
        toggle_show_hidden => ToggleShowHidden,
    }
    prompt {
        prompt_copy => PromptCopy,
        prompt_cut => PromptCut,
        prompt_paste => PromptPaste,
        prompt_reset => PromptReset,
        prompt_select_all => PromptSelectAll,
        prompt_submit => PromptSubmit,
    }
}

/// Resolved keybindings with fast lookup in both directions.
#[derive(Debug)]
pub struct KeyBindings {
    normal: HashMap<KeyCombo, Action>,
    prompt: HashMap<KeyCombo, Action>,
    action_display: HashMap<Action, String>,
}

impl KeyBindings {
    pub fn new(toml: &TomlKeybindings) -> Result<Self> {
        let (normal_bindings, prompt_bindings) = toml.to_bindings()?;

        let normal = build_action_map(&normal_bindings)?;
        let prompt = build_action_map(&prompt_bindings)?;
        let action_display = build_display_map(&normal_bindings, &prompt_bindings);

        Ok(Self {
            normal,
            prompt,
            action_display,
        })
    }

    /// Look up an action for a key press in normal mode.
    pub fn normal_action(&self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        Self::lookup(&self.normal, code, modifiers)
    }

    /// Look up an action for a key press in prompt mode.
    pub fn prompt_action(&self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        Self::lookup(&self.prompt, code, modifiers)
    }

    fn lookup(
        map: &HashMap<KeyCombo, Action>,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Option<Action> {
        let combo = KeyCombo::new(code, modifiers);
        if let Some(action) = map.get(&combo) {
            return Some(*action);
        }
        // Fallback: uppercase chars may arrive with or without SHIFT depending on terminal
        if let KeyCode::Char(c) = code
            && c.is_uppercase()
        {
            let toggled = modifiers ^ KeyModifiers::SHIFT;
            return map.get(&KeyCombo::new(code, toggled)).copied();
        }
        None
    }

    /// Get the display string for an action (includes hardcoded + rebindable keys).
    /// Keys are separated by "/", e.g. "↓/j". Suitable for help table columns.
    pub fn display_for(&self, action: Action) -> &str {
        self.action_display.get(&action).map_or("", |s| s.as_str())
    }

    /// Get a display string for use in hints, e.g. `"D" or "x"`.
    /// Each key is quoted and joined with " or ".
    /// Accepts multiple actions to combine all their keys into one list.
    pub fn hint_for(&self, actions: &[Action]) -> String {
        actions
            .iter()
            .filter_map(|action| self.action_display.get(action))
            .flat_map(|s| s.split('/'))
            .map(|k| {
                let mut chars = k.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if c.is_ascii_uppercase() => {
                        format!("\"{c}\" (Uppercase)")
                    }
                    _ => format!("\"{k}\""),
                }
            })
            .collect::<Vec<_>>()
            .join(" or ")
    }
}

/// Hardcoded keys per normal-mode action (arrow keys, Home/End,
/// PageUp/PageDown, Esc). These are always active in normal mode regardless
/// of config and are included in display strings.
const HARDCODED_NORMAL: &[(Action, &[KeyCombo])] = &[
    (
        Action::GoToParentDirectory,
        &[KeyCombo::new(KeyCode::Left, KeyModifiers::NONE)],
    ),
    (
        Action::Open,
        &[KeyCombo::new(KeyCode::Right, KeyModifiers::NONE)],
    ),
    (
        Action::SelectNext,
        &[KeyCombo::new(KeyCode::Down, KeyModifiers::NONE)],
    ),
    (
        Action::SelectPrevious,
        &[KeyCombo::new(KeyCode::Up, KeyModifiers::NONE)],
    ),
    (
        Action::SelectFirst,
        &[KeyCombo::new(KeyCode::Home, KeyModifiers::NONE)],
    ),
    (
        Action::SelectLast,
        &[KeyCombo::new(KeyCode::End, KeyModifiers::NONE)],
    ),
    (
        Action::PageUp,
        &[KeyCombo::new(KeyCode::PageUp, KeyModifiers::NONE)],
    ),
    (
        Action::PageDown,
        &[KeyCombo::new(KeyCode::PageDown, KeyModifiers::NONE)],
    ),
    (
        Action::ResetView,
        &[KeyCombo::new(KeyCode::Esc, KeyModifiers::NONE)],
    ),
];

/// Hardcoded keys per prompt-mode action (Esc, Tab, Up/Down). These are
/// always active in prompt mode regardless of config and are included in
/// display strings. They are reachable through the prompt action map, which
/// seeds them via `build_action_map`.
const HARDCODED_PROMPT: &[(Action, &[KeyCombo])] = &[
    (
        Action::PromptCancel,
        &[KeyCombo::new(KeyCode::Esc, KeyModifiers::NONE)],
    ),
    (
        Action::PromptAcceptSuggestion,
        &[KeyCombo::new(KeyCode::Tab, KeyModifiers::NONE)],
    ),
    (
        Action::PromptNextSuggestion,
        &[KeyCombo::new(KeyCode::Down, KeyModifiers::NONE)],
    ),
    (
        Action::PromptPreviousSuggestion,
        &[KeyCombo::new(KeyCode::Up, KeyModifiers::NONE)],
    ),
];

/// Hardcoded keys for an action in either mode, or an empty slice if it has none.
fn hardcoded_keys(action: Action) -> &'static [KeyCombo] {
    HARDCODED_NORMAL
        .iter()
        .chain(HARDCODED_PROMPT.iter())
        .find(|(a, _)| *a == action)
        .map_or(&[], |(_, keys)| *keys)
}

/// Look up an action from a key press using only normal-mode hardcoded
/// bindings. Returns `None` if the key combo is not hardcoded in normal mode.
/// Prompt-mode hardcoded keys (e.g. Tab) are excluded so they cannot shadow
/// configurable normal-mode bindings such as the default `goto = "Tab"`.
pub fn hardcoded_normal_action(code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
    let combo = KeyCombo::new(code, modifiers);
    for (action, keys) in HARDCODED_NORMAL {
        if keys.contains(&combo) {
            return Some(*action);
        }
    }
    None
}

/// Build the key→action HashMap, detecting duplicate key mappings. Hardcoded
/// keys go in first, for actions this mode binds. A config binding may repeat
/// one of its own action's keys; binding a key belonging to another action is an
/// error, because hardcoded keys stay active and it could never take effect.
fn build_action_map(bindings: &[(Action, Vec<KeyCombo>)]) -> Result<HashMap<KeyCombo, Action>> {
    let mut map = HashMap::new();

    let binding_actions: HashSet<Action> = bindings.iter().map(|(a, _)| *a).collect();
    for (action, keys) in HARDCODED_NORMAL.iter().chain(HARDCODED_PROMPT.iter()) {
        if binding_actions.contains(action) {
            for combo in *keys {
                map.insert(*combo, *action);
            }
        }
    }

    for (action, combos) in bindings {
        for combo in combos {
            if let Some(existing) = map.insert(*combo, *action)
                && existing != *action
            {
                return Err(anyhow!(
                    "Key '{}' is bound to both {:?} and {:?}",
                    format_key_combo(combo),
                    existing,
                    action,
                ));
            }
        }
    }
    Ok(map)
}

/// Build action→display string map. Combines hardcoded + rebindable keys.
fn build_display_map(
    normal: &[(Action, Vec<KeyCombo>)],
    prompt: &[(Action, Vec<KeyCombo>)],
) -> HashMap<Action, String> {
    let mut map = HashMap::new();

    for (action, combos) in normal.iter().chain(prompt.iter()) {
        let hardcoded = hardcoded_keys(*action);
        let display: Vec<String> = hardcoded
            .iter()
            .chain(combos.iter())
            .map(format_key_combo)
            .collect();
        map.insert(*action, display.join("/"));
    }

    map
}

fn parse_key_spec(spec: &KeySpec) -> Result<Vec<KeyCombo>> {
    match spec {
        KeySpec::Single(s) => Ok(vec![parse_key_combo(s)?]),
        KeySpec::Multiple(v) => v.iter().map(|s| parse_key_combo(s)).collect(),
    }
}

/// Parse a key string like "q", "Ctrl+c", "Shift+G", "F5", "Enter".
///
/// Modifier prefixes (`Ctrl+`, `Shift+`, `Alt+`, case-insensitive) are stripped
/// one at a time from the front; what remains is the key name. So `+` is itself
/// a valid key (`"+"`, `"Ctrl++"`) rather than a separator.
fn parse_key_combo(s: &str) -> Result<KeyCombo> {
    const PREFIXES: &[(&str, KeyModifiers)] = &[
        ("ctrl+", KeyModifiers::CONTROL),
        ("shift+", KeyModifiers::SHIFT),
        ("alt+", KeyModifiers::ALT),
    ];

    let mut modifiers = KeyModifiers::NONE;
    let mut rest = s;

    'outer: loop {
        for (prefix, modifier) in PREFIXES {
            // `>` (not `>=`) so the key name is never empty: "Ctrl+" with no
            // key falls through to the unknown-key error below. `get(..)`
            // (not direct slicing) returns None, rather than panicking, when
            // `prefix.len()` is not a char boundary (multibyte key strings).
            if rest.len() > prefix.len()
                && rest
                    .get(..prefix.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
            {
                modifiers |= *modifier;
                rest = &rest[prefix.len()..];
                continue 'outer;
            }
        }
        break;
    }

    let key_str = rest;
    let mut code = match key_str {
        "Enter" | "Return" => KeyCode::Enter,
        "Esc" | "Escape" => KeyCode::Esc,
        "Backspace" => KeyCode::Backspace,
        "Delete" | "Del" => KeyCode::Delete,
        "Space" => KeyCode::Char(' '),
        "Tab" => KeyCode::Tab,
        "BackTab" => KeyCode::BackTab,
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PgUp" | "PageUp" => KeyCode::PageUp,
        "PgDn" | "PageDown" => KeyCode::PageDown,
        s if s.starts_with('F') && s.len() > 1 => {
            let num: u8 = s[1..]
                .parse()
                .map_err(|_| anyhow!("Invalid F-key: '{s}'"))?;
            // No terminal emits F0 or beyond F24; reject them so a typo fails
            // config loading instead of producing a binding that never fires.
            if !(1..=24).contains(&num) {
                return Err(anyhow!("Invalid F-key: '{s}' (must be F1-F24)"));
            }
            KeyCode::F(num)
        }
        s if s.len() == 1 => {
            let mut ch = s.chars().next().expect("s.len() == 1 guarantees a char");
            // Terminals emit a plain shifted letter as the uppercase character,
            // so normalize "Shift+q" to the same combo as "Q". With further
            // modifiers (e.g. "Ctrl+Shift+a") the kitty protocol reports the
            // unshifted codepoint instead, so the letter stays lowercase.
            if ch.is_ascii_lowercase() && modifiers == KeyModifiers::SHIFT {
                ch = ch.to_ascii_uppercase();
            }
            // Uppercase letter without explicit Shift modifier → add SHIFT
            if ch.is_ascii_uppercase() && !modifiers.contains(KeyModifiers::SHIFT) {
                modifiers |= KeyModifiers::SHIFT;
            }
            KeyCode::Char(ch)
        }
        _ => return Err(anyhow!("Unknown key: '{key_str}'")),
    };

    // Terminals report Shift+Tab as BackTab (with the SHIFT modifier), never
    // as Tab+SHIFT, so normalize to the combo that will actually arrive.
    if code == KeyCode::Tab && modifiers.contains(KeyModifiers::SHIFT) {
        code = KeyCode::BackTab;
    }
    if code == KeyCode::BackTab {
        modifiers |= KeyModifiers::SHIFT;
    }

    Ok(KeyCombo::new(code, modifiers))
}

/// Format a KeyCombo into a human-readable display string.
fn format_key_combo(combo: &KeyCombo) -> String {
    let mut prefix = String::new();

    if combo.modifiers.contains(KeyModifiers::CONTROL) {
        prefix.push_str("Ctrl+");
    }
    if combo.modifiers.contains(KeyModifiers::ALT) {
        prefix.push_str("Alt+");
    }
    // Only show Shift explicitly for non-character keys (uppercase chars imply
    // Shift, and BackTab renders as "Shift+Tab" below)
    if combo.modifiers.contains(KeyModifiers::SHIFT)
        && !matches!(combo.code, KeyCode::Char(_) | KeyCode::BackTab)
    {
        prefix.push_str("Shift+");
    }

    match combo.code {
        KeyCode::Char(' ') => format!("{prefix}Space"),
        KeyCode::Char(c) => format!("{prefix}{c}"),
        KeyCode::Enter => format!("{prefix}Enter"),
        KeyCode::Esc => format!("{prefix}Esc"),
        KeyCode::Backspace => format!("{prefix}Backspace"),
        KeyCode::Delete => format!("{prefix}Delete"),
        KeyCode::Tab => format!("{prefix}Tab"),
        KeyCode::BackTab => format!("{prefix}Shift+Tab"),
        KeyCode::Up => format!("{prefix}↑"),
        KeyCode::Down => format!("{prefix}↓"),
        KeyCode::Left => format!("{prefix}←"),
        KeyCode::Right => format!("{prefix}→"),
        KeyCode::Home => format!("{prefix}Home"),
        KeyCode::End => format!("{prefix}End"),
        KeyCode::PageUp => format!("{prefix}PgUp"),
        KeyCode::PageDown => format!("{prefix}PgDn"),
        KeyCode::F(n) => format!("{prefix}F{n}"),
        _ => unreachable!("all KeyCode variants must be handled in format_key_combo"),
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    const DEFAULT_CONFIG: &str = include_str!("default_config.toml");
    const CTRL_SHIFT: KeyModifiers = KeyModifiers::CONTROL.union(KeyModifiers::SHIFT);

    /// Parse the embedded default config's `[keybindings]` section into a `TomlKeybindings`.
    fn default_toml_keybindings() -> TomlKeybindings {
        let value: toml::Value = toml::from_str(DEFAULT_CONFIG).unwrap();
        let kb_value = value.get("keybindings").unwrap().clone();
        kb_value.try_into().unwrap()
    }

    /// Build `KeyBindings` from the embedded default config.
    fn default_keybindings() -> KeyBindings {
        KeyBindings::new(&default_toml_keybindings()).unwrap()
    }

    /// Parse a TOML string with a `[keybindings]` section that overrides specific
    /// keys on top of the defaults (using TOML deep merge, same as the config system).
    fn keybindings_with_override(toml_fragment: &str) -> Result<KeyBindings> {
        use crate::app::config::merge_toml_values;

        let base: toml::Value = toml::from_str(DEFAULT_CONFIG).unwrap();
        let overlay: toml::Value = toml::from_str(toml_fragment).unwrap();
        let merged = merge_toml_values(base, overlay);
        let kb_value = merged.get("keybindings").unwrap().clone();
        let toml_kb: TomlKeybindings = kb_value.try_into().unwrap();
        KeyBindings::new(&toml_kb)
    }

    #[test_case("q", KeyCode::Char('q'), KeyModifiers::NONE     ; "a bare character")]
    #[test_case("+", KeyCode::Char('+'), KeyModifiers::NONE     ; "the separator itself")]
    #[test_case("G", KeyCode::Char('G'), KeyModifiers::SHIFT    ; "an uppercase character carries shift")]
    #[test_case("Ctrl+c", KeyCode::Char('c'), KeyModifiers::CONTROL ; "a modifier")]
    #[test_case("Ctrl++", KeyCode::Char('+'), KeyModifiers::CONTROL ; "a modifier on the separator")]
    #[test_case("Ctrl+Shift+a", KeyCode::Char('a'), CTRL_SHIFT  ; "two modifiers")]
    #[test_case("Ctrl+Shift++", KeyCode::Char('+'), CTRL_SHIFT  ; "two modifiers on the separator")]
    #[test_case("Enter", KeyCode::Enter, KeyModifiers::NONE     ; "a named key")]
    #[test_case("Esc", KeyCode::Esc, KeyModifiers::NONE         ; "esc")]
    #[test_case("Backspace", KeyCode::Backspace, KeyModifiers::NONE ; "backspace")]
    #[test_case("Delete", KeyCode::Delete, KeyModifiers::NONE   ; "delete")]
    #[test_case("Space", KeyCode::Char(' '), KeyModifiers::NONE ; "space names a character")]
    #[test_case("Home", KeyCode::Home, KeyModifiers::NONE       ; "home")]
    #[test_case("End", KeyCode::End, KeyModifiers::NONE         ; "end")]
    #[test_case("PgUp", KeyCode::PageUp, KeyModifiers::NONE     ; "the short paging spelling")]
    #[test_case("PgDn", KeyCode::PageDown, KeyModifiers::NONE   ; "the short paging spelling, down")]
    #[test_case("PageUp", KeyCode::PageUp, KeyModifiers::NONE   ; "the long paging spelling")]
    #[test_case("PageDown", KeyCode::PageDown, KeyModifiers::NONE ; "the long paging spelling, down")]
    // F1 and F24 are the ends of the accepted range.
    #[test_case("F1", KeyCode::F(1), KeyModifiers::NONE         ; "the first function key")]
    #[test_case("F5", KeyCode::F(5), KeyModifiers::NONE         ; "a function key")]
    #[test_case("F12", KeyCode::F(12), KeyModifiers::NONE       ; "a two digit function key")]
    #[test_case("F24", KeyCode::F(24), KeyModifiers::NONE       ; "the last function key")]
    // "F" alone is the letter, not a function key missing its number.
    #[test_case("F", KeyCode::Char('F'), KeyModifiers::SHIFT     ; "F on its own is a character")]
    fn a_spelling_parses_to_its_combo(spelling: &str, code: KeyCode, modifiers: KeyModifiers) {
        let combo = parse_key_combo(spelling).unwrap();
        assert_eq!(code, combo.code);
        assert_eq!(modifiers, combo.modifiers);
    }

    #[test_case("F0"              => "Invalid F-key: 'F0' (must be F1-F24)"  ; "a function key below the range")]
    #[test_case("F25"             => "Invalid F-key: 'F25' (must be F1-F24)" ; "a function key above the range")]
    #[test_case("F99"             => "Invalid F-key: 'F99' (must be F1-F24)" ; "a function key far above the range")]
    #[test_case("InvalidKey"      => "Unknown key: 'InvalidKey'" ; "a name that is not a key")]
    #[test_case("Ctrl+InvalidKey" => "Unknown key: 'InvalidKey'" ; "a modifier on a name that is not a key")]
    // A spelling starting with "F" reaches the F-key arm, so the number is
    // what it is reported as failing to parse.
    #[test_case("Foo+c"           => "Invalid F-key: 'Foo+c'"    ; "a modifier that does not exist")]
    // The key name is never empty: the modifier prefix is stripped only when
    // something follows it, so the whole spelling is what the error names.
    #[test_case("Ctrl+"           => "Unknown key: 'Ctrl+'"      ; "a modifier with no key")]
    // A multibyte char straddling the prefix-length byte index must not panic
    // the str slicing; it has to come back as a normal parse error.
    #[test_case("aaa\u{2713}x"     => "Unknown key: 'aaa\u{2713}x'" ; "a multibyte char straddling the prefix index")]
    #[test_case("\u{2713}"         => "Unknown key: '\u{2713}'"     ; "a lone multibyte char")]
    fn a_spelling_that_is_not_a_key_is_an_error(spelling: &str) -> String {
        // No terminal emits these, so they must fail config loading rather
        // than silently producing a binding that never fires. Which refusal
        // fired is the assertion: every one of these is an error whatever the
        // parser did with the modifier prefix or the F-key range.
        parse_key_combo(spelling)
            .expect_err("a spelling that is not a key must be refused")
            .to_string()
    }

    #[test]
    fn parse_shift_lowercase_normalizes_to_uppercase() {
        // Terminals emit shifted letters as the uppercase character, so all
        // three spellings must produce the same combo.
        let combo = parse_key_combo("Shift+q").unwrap();
        assert_eq!(combo.code, KeyCode::Char('Q'));
        assert_eq!(combo.modifiers, KeyModifiers::SHIFT);
        assert_eq!(combo, parse_key_combo("Shift+Q").unwrap());
        assert_eq!(combo, parse_key_combo("Q").unwrap());
    }

    #[test]
    fn parse_shift_tab_normalizes_to_backtab() {
        // Terminals report Shift+Tab as BackTab with SHIFT, never Tab+SHIFT.
        let combo = parse_key_combo("Shift+Tab").unwrap();
        assert_eq!(combo.code, KeyCode::BackTab);
        assert_eq!(combo.modifiers, KeyModifiers::SHIFT);
        assert_eq!(combo, parse_key_combo("BackTab").unwrap());
    }

    #[test]
    fn formatting_a_combo_produces_a_spelling_that_parses_back() {
        let cases = [
            "q",
            "G",
            "Ctrl+c",
            "F5",
            "Enter",
            "Esc",
            "Space",
            "/",
            "+",
            "Ctrl++",
            "Shift+Tab",
        ];
        for case in cases {
            let combo = parse_key_combo(case).unwrap();
            let formatted = format_key_combo(&combo);
            let reparsed = parse_key_combo(&formatted).unwrap();
            assert_eq!(
                combo, reparsed,
                "Round-trip failed for '{case}': formatted as '{formatted}'"
            );
        }
    }

    #[test]
    fn default_config_keybindings_have_no_conflicts() {
        default_keybindings();
    }

    #[test]
    fn two_actions_bound_to_the_same_key_is_an_error() {
        let result = keybindings_with_override(
            r#"
            [keybindings]
            quit = "j"
            select_next = "j"
            "#,
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains('j'), "Error should mention the key: {err}");
    }

    #[test]
    fn binding_a_hardcoded_key_to_another_action_is_an_error() {
        // ↓ is hardcoded to SelectNext and always active, so a config binding
        // of the same key to a different action could never take effect.
        let result = keybindings_with_override(
            r#"
            [keybindings]
            select_previous = "Down"
            "#,
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("↓"), "Error should mention the key: {err}");
    }

    #[test]
    fn rebinding_a_hardcoded_key_to_its_own_action_is_allowed() {
        let kb = keybindings_with_override(
            r#"
            [keybindings]
            select_next = ["j", "Down"]
            "#,
        )
        .unwrap();
        assert_eq!(
            kb.normal_action(KeyCode::Down, KeyModifiers::NONE),
            Some(Action::SelectNext)
        );
    }

    #[test]
    fn a_configured_binding_replaces_the_default_for_that_action() {
        let kb = keybindings_with_override(
            r#"
            [keybindings]
            quit = "x"
            chmod = "Ctrl+Shift+x"
            cut = "Ctrl+x"
            "#,
        )
        .unwrap();
        assert_eq!(
            kb.normal_action(KeyCode::Char('x'), KeyModifiers::NONE),
            Some(Action::Quit)
        );
        // An override replaces the default binding rather than adding to it,
        // so the key it displaced is left unbound.
        assert_eq!(
            kb.normal_action(KeyCode::Char('q'), KeyModifiers::NONE),
            None
        );
    }

    #[test]
    fn display_includes_hardcoded_keys() {
        let kb = default_keybindings();
        let display = kb.display_for(Action::SelectNext);
        assert!(
            display.contains('↓'),
            "SelectNext display should include hardcoded ↓: {display}"
        );
        assert!(
            display.contains('j'),
            "SelectNext display should include configurable j: {display}"
        );
    }

    #[test]
    fn hint_for_quotes_each_key() {
        let kb = default_keybindings();
        let hint = kb.hint_for(&[Action::SelectNext]);
        assert!(
            hint.contains("\"↓\""),
            "hint should quote hardcoded ↓: {hint}"
        );
        assert!(
            hint.contains("\"j\""),
            "hint should quote configurable j: {hint}"
        );
        assert!(
            hint.contains(" or "),
            "hint should join keys with ' or ': {hint}"
        );
    }

    /// An uppercase binding parses to the letter plus SHIFT, but a terminal may
    /// report the letter alone, which only the fallback resolves.
    #[test]
    fn an_uppercase_key_reported_without_shift_resolves_through_the_fallback() {
        let kb = default_keybindings();
        // SelectLast is bound to "G", so this is Char('G') + SHIFT in the map.
        assert_eq!(
            kb.normal_action(KeyCode::Char('G'), KeyModifiers::NONE),
            Some(Action::SelectLast)
        );
    }

    #[test]
    fn an_uppercase_key_reported_with_shift_matches_directly() {
        let kb = default_keybindings();
        // RangeMark is bound to "V", which parses to Char('V') + SHIFT.
        assert_eq!(
            kb.normal_action(KeyCode::Char('V'), KeyModifiers::SHIFT),
            Some(Action::RangeMark)
        );
    }

    /// The hardcoded-only actions have no TOML field, so they reach the action
    /// maps only through the placeholders `to_bindings` seeds. Drop one and its
    /// keys resolve to nothing: Esc would stop resetting the view and cancelling
    /// a prompt, Tab would stop accepting a suggestion.
    #[test]
    fn hardcoded_only_actions_resolve_through_the_action_maps() {
        let kb = default_keybindings();
        assert_eq!(
            Some(Action::ResetView),
            kb.normal_action(KeyCode::Esc, KeyModifiers::NONE)
        );
        assert_eq!(
            Some(Action::PromptCancel),
            kb.prompt_action(KeyCode::Esc, KeyModifiers::NONE)
        );
        assert_eq!(
            Some(Action::PromptAcceptSuggestion),
            kb.prompt_action(KeyCode::Tab, KeyModifiers::NONE)
        );
        assert_eq!(
            Some(Action::PromptNextSuggestion),
            kb.prompt_action(KeyCode::Down, KeyModifiers::NONE)
        );
        assert_eq!(
            Some(Action::PromptPreviousSuggestion),
            kb.prompt_action(KeyCode::Up, KeyModifiers::NONE)
        );
    }

    #[test]
    fn normal_mode_tab_resolves_to_goto() {
        // Tab is hardcoded only in prompt mode, so it must not shadow the
        // configurable normal-mode binding (default: goto = [":", "Tab"]).
        assert_eq!(
            hardcoded_normal_action(KeyCode::Tab, KeyModifiers::NONE),
            None
        );
        let kb = default_keybindings();
        assert_eq!(
            kb.normal_action(KeyCode::Tab, KeyModifiers::NONE),
            Some(Action::Goto)
        );
    }

    #[test]
    fn display_includes_hardcoded_keys_from_both_modes() {
        let kb = default_keybindings();
        let accept = kb.display_for(Action::PromptAcceptSuggestion);
        assert_eq!(accept, "Tab");
        let goto = kb.display_for(Action::Goto);
        assert!(
            goto.contains(':') && goto.contains("Tab"),
            "Goto display should list its configured keys: {goto}"
        );
        let cancel = kb.display_for(Action::PromptCancel);
        assert_eq!(cancel, "Esc");
    }

    #[test]
    fn prompt_mode_resolves_its_own_bindings() {
        let kb = default_keybindings();
        assert_eq!(
            kb.prompt_action(KeyCode::Enter, KeyModifiers::NONE),
            Some(Action::PromptSubmit)
        );
        assert_eq!(
            kb.prompt_action(KeyCode::Char('z'), KeyModifiers::CONTROL),
            Some(Action::PromptReset)
        );
    }
}

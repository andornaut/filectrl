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
    Edit,
    Page,
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
    SelectAll,

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
                            format!("Invalid keybinding for {}", stringify!($n_field))
                        })?,
                    ));
                )+
                $(
                    prompt.push((
                        Action::$p_action,
                        parse_key_spec(&self.$p_field).with_context(|| {
                            format!("Invalid keybinding for {}", stringify!($p_field))
                        })?,
                    ));
                )+

                Ok((normal, prompt))
            }
        }

        impl Action {
            /// How an error names the action: its `[keybindings]` key, or
            /// for an action with only hardcoded keys, what the key does.
            fn config_key(self) -> &'static str {
                match self {
                    $(Action::$n_action => stringify!($n_field),)+
                    $(Action::$p_action => stringify!($p_field),)+
                    Action::ResetView => "the hardcoded reset view",
                    Action::PromptCancel => "the hardcoded prompt cancel",
                    Action::PromptAcceptSuggestion => "the hardcoded accept suggestion",
                    Action::PromptNextSuggestion => "the hardcoded next suggestion",
                    Action::PromptPreviousSuggestion => "the hardcoded previous suggestion",
                }
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
        edit => Edit,
        filter => Filter,
        go_home => GoHome,
        goto => Goto,
        open => Open,
        open_current_directory => OpenCurrentDirectory,
        open_new_window => OpenNewWindow,
        open_with => OpenWith,
        page => Page,
        page_down => PageDown,
        page_up => PageUp,
        paste => Paste,
        quit => Quit,
        range_mark => RangeMark,
        refresh => Refresh,
        rename => Rename,
        search => Search,
        select_all => SelectAll,
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
    /// Each action's keys as displayed, hardcoded ones first.
    action_keys: HashMap<Action, Vec<String>>,
    /// `action_keys` joined with "/", for the help table.
    action_display: HashMap<Action, String>,
}

impl KeyBindings {
    /// Parses every key in `toml` without building the maps, so an invalid
    /// key is reported without checking for conflicts.
    pub fn check(toml: &TomlKeybindings) -> Result<()> {
        toml.to_bindings().map(|_| ())
    }

    pub fn new(toml: &TomlKeybindings) -> Result<Self> {
        let (normal_bindings, prompt_bindings) = toml.to_bindings()?;

        let normal = build_action_map(&normal_bindings)?;
        let prompt = build_action_map(&prompt_bindings)?;
        let action_keys = build_display_map(&normal_bindings, &prompt_bindings);
        let action_display = action_keys
            .iter()
            .map(|(action, keys)| (*action, keys.join("/")))
            .collect();

        Ok(Self {
            normal,
            prompt,
            action_keys,
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
        let KeyCode::Char(c) = code else {
            return None;
        };
        if !c.is_uppercase() {
            return None;
        }
        // Fallback: uppercase chars may arrive with or without SHIFT depending on terminal
        let toggled = modifiers ^ KeyModifiers::SHIFT;
        if let Some(action) = map.get(&KeyCombo::new(code, toggled)) {
            return Some(*action);
        }
        // The legacy encoding sends Alt+Shift+q as ESC then "Q", which arrives
        // as the uppercase letter with Alt, where a binding holds the lowercase
        // letter with Shift (the kitty protocol's spelling).
        if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return None;
        }
        let mut lower = c.to_lowercase();
        let (Some(lower), None) = (lower.next(), lower.next()) else {
            return None;
        };
        map.get(&KeyCombo::new(
            KeyCode::Char(lower),
            modifiers | KeyModifiers::SHIFT,
        ))
        .copied()
    }

    /// Get the display string for an action (includes hardcoded + rebindable keys).
    /// Keys are separated by "/", e.g. "↓/j". Suitable for help table columns.
    pub fn display_for(&self, action: Action) -> &str {
        self.action_display.get(&action).map_or("", |s| s.as_str())
    }

    /// The keys bound to `action`, in binding order.
    pub fn keys_for(&self, action: Action) -> &[String] {
        self.action_keys.get(&action).map_or(&[], Vec::as_slice)
    }

    /// Get a display string for use in hints, e.g. `"D" or "x"`.
    /// Each key is quoted and joined with " or ".
    /// Accepts multiple actions to combine all their keys into one list.
    pub fn hint_for(&self, actions: &[Action]) -> String {
        actions
            .iter()
            .filter_map(|action| self.action_keys.get(action))
            .flatten()
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
                    "Key '{}' is bound to both {} and {}",
                    format_key_combo(combo),
                    existing.config_key(),
                    action.config_key(),
                ));
            }
        }
    }
    Ok(map)
}

/// Build action→displayed keys map. Combines hardcoded + rebindable keys.
/// Kept as a list rather than joined, since a key may itself be "/".
fn build_display_map(
    normal: &[(Action, Vec<KeyCombo>)],
    prompt: &[(Action, Vec<KeyCombo>)],
) -> HashMap<Action, Vec<String>> {
    let mut map = HashMap::new();

    for (action, combos) in normal.iter().chain(prompt.iter()) {
        let hardcoded = hardcoded_keys(*action);
        // A configured key may repeat a hardcoded one (`select_next =
        // ["Down", "j"]`); each is shown once.
        let mut display: Vec<String> = Vec::new();
        for key in hardcoded.iter().chain(combos.iter()).map(format_key_combo) {
            if !display.contains(&key) {
                display.push(key);
            }
        }
        map.insert(*action, display);
    }

    map
}

fn parse_key_spec(spec: &KeySpec) -> Result<Vec<KeyCombo>> {
    match spec {
        KeySpec::Single(s) => Ok(vec![parse_key_combo(s)?]),
        // An empty list would leave the action with no key at all, and the
        // help and the hints that name its key with nothing to show.
        KeySpec::Multiple(v) if v.is_empty() => Err(anyhow!("no key given")),
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
    let spelling = s;

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
    // Named keys match in any case, like the modifiers. Every name is longer
    // than one character, so a single letter never matches one.
    let mut code = match key_str.to_ascii_lowercase().as_str() {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        // Shift+Space arrives as a plain space, like "Shift+1" arrives as "!".
        "space" if modifiers == KeyModifiers::SHIFT => {
            return Err(anyhow!(
                "Invalid key: '{spelling}' (Shift+Space arrives as a plain Space; bind 'Space')"
            ));
        }
        "space" => KeyCode::Char(' '),
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pgup" | "pageup" => KeyCode::PageUp,
        "pgdn" | "pagedown" => KeyCode::PageDown,
        // Only "F" and digits is a function key, so a word such as an
        // unknown modifier ("Foo+x") is reported as an unknown key.
        s if s.len() > 1 && s.starts_with('f') && s[1..].bytes().all(|b| b.is_ascii_digit()) => {
            // No terminal emits F0 or beyond F24; reject them so a typo fails
            // config loading instead of producing a binding that never fires.
            // A number too large for a u8 is out of range too.
            let num = s[1..]
                .parse::<u8>()
                .ok()
                .filter(|num| (1..=24).contains(num))
                .ok_or_else(|| anyhow!("Invalid F-key: '{key_str}' (must be F1-F24)"))?;
            KeyCode::F(num)
        }
        // Counted in chars, not bytes, so a non-ASCII key such as "é" is one
        // character rather than a multi-byte name.
        _ if key_str.chars().count() == 1 => {
            let mut ch = key_str.chars().next().expect("the guard counted one char");
            // A key that draws nothing, or runs as a command when shown, could
            // not be told apart in the help screen from another binding or none.
            if crate::is_disguising(ch) {
                return Err(anyhow!(
                    "Invalid key: '{}' (a control or invisible character cannot be bound)",
                    crate::visible(key_str)
                ));
            }
            // Terminals emit a plain shifted letter as the uppercase character,
            // so normalize "Shift+q" to the same combo as "Q". With further
            // modifiers (e.g. "Ctrl+Shift+a") the kitty protocol reports the
            // unshifted codepoint instead, so the letter stays lowercase.
            // A letter whose uppercase is several characters (ß) has no
            // shifted key, so it is left as it is.
            if ch.is_lowercase() && modifiers == KeyModifiers::SHIFT {
                let mut upper = ch.to_uppercase();
                if let (Some(single), None) = (upper.next(), upper.next()) {
                    ch = single;
                }
            }
            // Any other character arrives already shifted ("!" for Shift+1),
            // with no modifier, so the binding as written could never fire.
            if modifiers == KeyModifiers::SHIFT && !ch.is_uppercase() {
                return Err(anyhow!(
                    "Invalid key: '{spelling}' (Shift applies only to letters with a single uppercase form; bind the shifted character itself)"
                ));
            }
            // With Ctrl or Alt, the kitty protocol reports the unshifted
            // letter plus Shift ("Ctrl+Shift+g" arrives as g with Ctrl+Shift),
            // so that is the one spelling bound. The legacy encoding cannot
            // carry the Shift with Ctrl at all (Ctrl+G is the byte Ctrl+g
            // sends); its Alt+Shift+g arrives as G with Alt, which `lookup`
            // matches to the same binding.
            if ch.is_uppercase() && modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            {
                return Err(uppercase_with_modifier(spelling, ch, modifiers));
            }
            // Uppercase letter without explicit Shift modifier → add SHIFT
            if ch.is_uppercase() && !modifiers.contains(KeyModifiers::SHIFT) {
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

/// The refusal of an uppercase letter with Ctrl or Alt, naming the spelling
/// to write instead.
fn uppercase_with_modifier(spelling: &str, ch: char, modifiers: KeyModifiers) -> anyhow::Error {
    let lower: String = ch.to_lowercase().collect();
    let named = modifiers.difference(KeyModifiers::SHIFT);
    let prefix: String = [
        (KeyModifiers::CONTROL, "Ctrl+"),
        (KeyModifiers::ALT, "Alt+"),
    ]
    .iter()
    .filter(|(modifier, _)| named.contains(*modifier))
    .map(|(_, name)| *name)
    .collect();
    anyhow!(
        "Invalid key: '{spelling}' (write a letter with Ctrl or Alt in lowercase, adding Shift for the uppercase one, as '{prefix}Shift+{lower}')"
    )
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
    // Shift is implied by an uppercase letter on its own, and BackTab renders
    // as "Shift+Tab" below. With Ctrl or Alt a letter is bound lowercase, so
    // Shift is the only thing telling Ctrl+Shift+a from Ctrl+a.
    let shift_is_implied = match combo.code {
        KeyCode::Char(_) => !combo
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT),
        KeyCode::BackTab => true,
        _ => false,
    };
    if combo.modifiers.contains(KeyModifiers::SHIFT) && !shift_is_implied {
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
    #[test_case("Alt+Shift+a", KeyCode::Char('a'), KeyModifiers::ALT.union(KeyModifiers::SHIFT) ; "alt and shift on a lowercase letter")]
    #[test_case("Alt+x", KeyCode::Char('x'), KeyModifiers::ALT ; "alt")]
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
    #[test_case("\u{e9}", KeyCode::Char('\u{e9}'), KeyModifiers::NONE ; "a non-ASCII character")]
    // Named keys match in any case, like the modifiers.
    #[test_case("enter", KeyCode::Enter, KeyModifiers::NONE     ; "a named key in lowercase")]
    #[test_case("ESC", KeyCode::Esc, KeyModifiers::NONE         ; "a named key in uppercase")]
    #[test_case("ctrl+space", KeyCode::Char(' '), KeyModifiers::CONTROL ; "space in lowercase")]
    #[test_case("f5", KeyCode::F(5), KeyModifiers::NONE         ; "a function key in lowercase")]
    #[test_case("f", KeyCode::Char('f'), KeyModifiers::NONE      ; "f on its own is still the letter")]
    #[test_case("Alt+\u{2713}", KeyCode::Char('\u{2713}'), KeyModifiers::ALT ; "a modifier on a multibyte character")]
    fn a_spelling_parses_to_its_combo(spelling: &str, code: KeyCode, modifiers: KeyModifiers) {
        let combo = parse_key_combo(spelling).unwrap();
        assert_eq!(code, combo.code);
        assert_eq!(modifiers, combo.modifiers);
    }

    #[test_case("F0"              => "Invalid F-key: 'F0' (must be F1-F24)"  ; "a function key below the range")]
    #[test_case("F25"             => "Invalid F-key: 'F25' (must be F1-F24)" ; "a function key above the range")]
    #[test_case("F99"             => "Invalid F-key: 'F99' (must be F1-F24)" ; "a function key far above the range")]
    #[test_case("\u{9b}"          => "Invalid key: '\\u{9b}' (a control or invisible character cannot be bound)" ; "a control character")]
    #[test_case("Ctrl+\u{2800}"    => "Invalid key: '\\u{2800}' (a control or invisible character cannot be bound)" ; "a modifier on a character that draws nothing")]
    #[test_case("InvalidKey"      => "Unknown key: 'InvalidKey'" ; "a name that is not a key")]
    #[test_case("Ctrl+InvalidKey" => "Unknown key: 'InvalidKey'" ; "a modifier on a name that is not a key")]
    #[test_case("F1000"           => "Invalid F-key: 'F1000' (must be F1-F24)" ; "a function key too large for a u8")]
    // Only "F" and digits is a function key, so a word starting with "F" is
    // an unknown key rather than a function key that failed to parse.
    #[test_case("Foo+c"           => "Unknown key: 'Foo+c'"      ; "a modifier that does not exist")]
    #[test_case("Fn"              => "Unknown key: 'Fn'"         ; "a name starting with F")]
    // The key name is never empty: the modifier prefix is stripped only when
    // something follows it, so the whole spelling is what the error names.
    #[test_case("Ctrl+"           => "Unknown key: 'Ctrl+'"      ; "a modifier with no key")]
    // A multibyte char straddling the prefix-length byte index must not panic
    // the str slicing; it has to come back as a normal parse error.
    #[test_case("aaa\u{2713}x"     => "Unknown key: 'aaa\u{2713}x'" ; "a multibyte char straddling the prefix index")]
    // A shifted digit or symbol arrives as the character it produces, with no
    // modifier, and "\u{df}" uppercases to two characters, so it has no
    // shifted key at all.
    #[test_case("Shift+1"         => "Invalid key: 'Shift+1' (Shift applies only to letters with a single uppercase form; bind the shifted character itself)" ; "shift on a digit")]
    #[test_case("Shift+/"         => "Invalid key: 'Shift+/' (Shift applies only to letters with a single uppercase form; bind the shifted character itself)" ; "shift on a symbol")]
    #[test_case("Shift+Space"     => "Invalid key: 'Shift+Space' (Shift+Space arrives as a plain Space; bind 'Space')" ; "shift on space")]
    #[test_case("Shift+\u{df}"    => "Invalid key: 'Shift+\u{df}' (Shift applies only to letters with a single uppercase form; bind the shifted character itself)" ; "shift on a letter with no single uppercase")]
    // With Ctrl or Alt a letter is bound lowercase, plus Shift for the
    // uppercase one: the spelling the kitty protocol reports, and the one
    // `lookup` matches a legacy Alt+Shift press to.
    #[test_case("Ctrl+G"          => "Invalid key: 'Ctrl+G' (write a letter with Ctrl or Alt in lowercase, adding Shift for the uppercase one, as 'Ctrl+Shift+g')" ; "ctrl on an uppercase letter")]
    #[test_case("Alt+A"           => "Invalid key: 'Alt+A' (write a letter with Ctrl or Alt in lowercase, adding Shift for the uppercase one, as 'Alt+Shift+a')" ; "alt on an uppercase letter")]
    #[test_case("Ctrl+Shift+A"    => "Invalid key: 'Ctrl+Shift+A' (write a letter with Ctrl or Alt in lowercase, adding Shift for the uppercase one, as 'Ctrl+Shift+a')" ; "ctrl and shift on an uppercase letter")]
    #[test_case("Ctrl+Alt+\u{c9}" => "Invalid key: 'Ctrl+Alt+\u{c9}' (write a letter with Ctrl or Alt in lowercase, adding Shift for the uppercase one, as 'Ctrl+Alt+Shift+\u{e9}')" ; "two modifiers on a non-ascii uppercase letter")]
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
    fn parse_shift_normalizes_a_non_ascii_letter_to_uppercase() {
        let combo = parse_key_combo("Shift+\u{e9}").unwrap();
        assert_eq!(combo.code, KeyCode::Char('\u{c9}'));
        assert_eq!(combo.modifiers, KeyModifiers::SHIFT);
        assert_eq!(combo, parse_key_combo("\u{c9}").unwrap());
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
            "Ctrl+Shift+a",
            "Alt+Shift+n",
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

    #[test_case("G" => "G" ; "an uppercase letter implies its shift")]
    #[test_case("Shift+Tab" => "Shift+Tab" ; "backtab names its shift once")]
    #[test_case("Space" => "Space" ; "space is named rather than blank")]
    #[test_case("PgUp" => "PgUp" ; "page up")]
    #[test_case("PageDown" => "PgDn" ; "page down in its short spelling")]
    #[test_case("Alt+x" => "Alt+x" ; "alt")]
    #[test_case("Ctrl+Alt+Shift+Down" => "Ctrl+Alt+Shift+\u{2193}" ; "every modifier in order")]
    #[test_case("Ctrl+Shift+a" => "Ctrl+Shift+a" ; "shift on a letter with ctrl")]
    #[test_case("Alt+Shift+n" => "Alt+Shift+n" ; "shift on a letter with alt")]
    #[test_case("Ctrl+a" => "Ctrl+a" ; "a letter with ctrl alone")]
    fn a_combo_displays_as(spelling: &str) -> String {
        format_key_combo(&parse_key_combo(spelling).unwrap())
    }

    #[test_case(KeyCode::Left => Some(Action::GoToParentDirectory) ; "left")]
    #[test_case(KeyCode::Right => Some(Action::Open) ; "right")]
    #[test_case(KeyCode::Down => Some(Action::SelectNext) ; "down")]
    #[test_case(KeyCode::Up => Some(Action::SelectPrevious) ; "up")]
    #[test_case(KeyCode::Home => Some(Action::SelectFirst) ; "home")]
    #[test_case(KeyCode::End => Some(Action::SelectLast) ; "end")]
    #[test_case(KeyCode::PageUp => Some(Action::PageUp) ; "page up")]
    #[test_case(KeyCode::PageDown => Some(Action::PageDown) ; "page down")]
    #[test_case(KeyCode::Esc => Some(Action::ResetView) ; "esc")]
    fn a_hardcoded_normal_key_resolves_to_its_action(code: KeyCode) -> Option<Action> {
        default_keybindings().normal_action(code, KeyModifiers::NONE)
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
    fn a_conflict_names_both_config_keys() {
        let err = keybindings_with_override(
            r#"
            [keybindings]
            back = "q"
            "#,
        )
        .unwrap_err()
        .to_string();
        assert_eq!("Key 'q' is bound to both back and quit", err);
    }

    #[test]
    fn a_conflict_with_a_hardcoded_only_key_says_what_the_key_does() {
        let err = keybindings_with_override(
            r#"
            [keybindings]
            quit = "Esc"
            "#,
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            "Key 'Esc' is bound to both the hardcoded reset view and quit",
            err
        );
    }

    #[test_case("show_bookmarks" ; "a normal mode key")]
    #[test_case("prompt_submit"  ; "a prompt mode key")]
    fn an_unparsable_binding_names_its_config_key(key: &str) {
        let err = keybindings_with_override(&format!("[keybindings]\n{key} = \"NotAKey\"\n"))
            .unwrap_err();
        assert_eq!(format!("Invalid keybinding for {key}"), err.to_string());
        assert_eq!("Unknown key: 'NotAKey'", err.root_cause().to_string());
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
        // Shown once, though it is both hardcoded and configured.
        assert_eq!("↓/j", kb.display_for(Action::SelectNext));
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

    #[test]
    fn a_hint_marks_an_uppercase_key() {
        // SelectLast is bound to "G". The quoted letter alone does not say
        // that it needs Shift.
        let hint = default_keybindings().hint_for(&[Action::SelectLast]);
        assert!(hint.contains("\"G\" (Uppercase)"), "{hint}");
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

    /// The legacy encoding sends Alt+Shift+q as ESC then "Q", which crossterm
    /// reports as the uppercase letter with Alt, with or without Shift.
    #[test_case(KeyModifiers::ALT ; "alt alone")]
    #[test_case(KeyModifiers::ALT.union(KeyModifiers::SHIFT) ; "alt and shift")]
    fn a_legacy_alt_shift_letter_resolves_to_its_binding(modifiers: KeyModifiers) {
        let kb = keybindings_with_override(
            r#"
            [keybindings]
            quit = "Alt+Shift+q"
            "#,
        )
        .unwrap();

        assert_eq!(
            Some(Action::Quit),
            kb.normal_action(KeyCode::Char('Q'), modifiers)
        );
        // The unshifted press stays unbound.
        assert_eq!(
            None,
            kb.normal_action(KeyCode::Char('q'), KeyModifiers::ALT)
        );
    }

    /// A key that is itself "/" is one key in a hint, not a separator between
    /// two empty ones.
    #[test]
    fn a_hint_names_a_slash_key() {
        let kb = keybindings_with_override(
            r#"
            [keybindings]
            clear_alerts = ["/", "Ctrl+l"]
            search = "F9"
            "#,
        )
        .unwrap();

        assert_eq!("\"/\" or \"Ctrl+l\"", kb.hint_for(&[Action::ClearAlerts]));
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

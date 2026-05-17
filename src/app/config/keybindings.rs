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
    Back,
    GoHome,
    Open,
    OpenCurrentDirectory,
    OpenNewWindow,
    Refresh,

    // Selection
    SelectNext,
    SelectPrevious,
    SelectFirst,
    SelectLast,
    SelectMiddle,
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
    Chmod,
    CreateDirectory,
    Delete,
    Filter,
    Goto,
    Rename,
    Search,

    // Sort
    SortByModified,
    SortByName,
    SortBySize,

    // Prompt
    PromptCancel,
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
        /// All fields are required — defaults are provided by the embedded default_config.toml.
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
                let mut prompt: BindingList = vec![(Action::PromptCancel, vec![])];

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
        back => Back,
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
        page_down => PageDown,
        page_up => PageUp,
        paste => Paste,
        quit => Quit,
        range_mark => RangeMark,
        refresh => Refresh,
        rename => Rename,
        search => Search,
        select_first => SelectFirst,
        select_last => SelectLast,
        select_middle => SelectMiddle,
        select_next => SelectNext,
        select_previous => SelectPrevious,
        sort_by_modified => SortByModified,
        sort_by_name => SortByName,
        sort_by_size => SortBySize,
        toggle_help => ToggleHelp,
        toggle_mark => ToggleMark,
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
    pub fn normal_action(&self, code: &KeyCode, modifiers: &KeyModifiers) -> Option<Action> {
        Self::lookup(&self.normal, code, modifiers)
    }

    /// Look up an action for a key press in prompt mode.
    pub fn prompt_action(&self, code: &KeyCode, modifiers: &KeyModifiers) -> Option<Action> {
        Self::lookup(&self.prompt, code, modifiers)
    }

    fn lookup(
        map: &HashMap<KeyCombo, Action>,
        code: &KeyCode,
        modifiers: &KeyModifiers,
    ) -> Option<Action> {
        let combo = KeyCombo::new(*code, *modifiers);
        if let Some(action) = map.get(&combo) {
            return Some(*action);
        }
        // Fallback: uppercase chars may arrive with or without SHIFT depending on terminal
        if let KeyCode::Char(c) = code {
            if c.is_uppercase() {
                let toggled = *modifiers ^ KeyModifiers::SHIFT;
                return map.get(&KeyCombo::new(*code, toggled)).copied();
            }
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
            .map(|k| format!("\"{}\"", k))
            .collect::<Vec<_>>()
            .join(" or ")
    }
}

/// Hardcoded keys per action (arrow keys, Home/End, PageUp/PageDown, Esc).
/// These are always active regardless of config and are included in display strings.
const HARDCODED: &[(Action, &[KeyCombo])] = &[
    (
        Action::Back,
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
    (
        Action::PromptCancel,
        &[KeyCombo::new(KeyCode::Esc, KeyModifiers::NONE)],
    ),
];

/// Hardcoded keys for an action, or an empty slice if it has none.
fn hardcoded_keys(action: Action) -> &'static [KeyCombo] {
    HARDCODED
        .iter()
        .find(|(a, _)| *a == action)
        .map_or(&[], |(_, keys)| *keys)
}

/// Build the key→action HashMap, detecting duplicate key mappings.
/// Hardcoded keys are inserted first for actions present in this mode's
/// binding list, then user bindings override them.
fn build_action_map(bindings: &[(Action, Vec<KeyCombo>)]) -> Result<HashMap<KeyCombo, Action>> {
    let mut map = HashMap::new();

    let binding_actions: HashSet<Action> = bindings.iter().map(|(a, _)| *a).collect();
    for (action, keys) in HARDCODED {
        if binding_actions.contains(action) {
            for combo in *keys {
                map.insert(*combo, *action);
            }
        }
    }

    for (action, combos) in bindings {
        for combo in combos {
            if let Some(existing) = map.insert(*combo, *action) {
                if existing != *action && !hardcoded_keys(existing).contains(combo) {
                    return Err(anyhow!(
                        "Key '{}' is bound to both {:?} and {:?}",
                        format_key_combo(combo),
                        existing,
                        action,
                    ));
                }
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

/// Parse a key string like "q", "Ctrl+c", "Shift+G", "F5", "Enter" into a KeyCombo.
fn parse_key_combo(s: &str) -> Result<KeyCombo> {
    let parts: Vec<&str> = s.split('+').collect();
    let mut modifiers = KeyModifiers::NONE;

    for part in &parts[..parts.len() - 1] {
        match part.to_lowercase().as_str() {
            "ctrl" => modifiers |= KeyModifiers::CONTROL,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            "alt" => modifiers |= KeyModifiers::ALT,
            _ => return Err(anyhow!("Unknown modifier: '{part}'")),
        }
    }

    let key_str = parts
        .last()
        .expect("split always yields at least one element");
    let code = match *key_str {
        "Enter" | "Return" => KeyCode::Enter,
        "Esc" | "Escape" => KeyCode::Esc,
        "Backspace" => KeyCode::Backspace,
        "Delete" | "Del" => KeyCode::Delete,
        "Space" => KeyCode::Char(' '),
        "Tab" => KeyCode::Tab,
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
            KeyCode::F(num)
        }
        s if s.len() == 1 => {
            let ch = s.chars().next().expect("s.len() == 1 guarantees a char");
            // Uppercase letter without explicit Shift modifier → add SHIFT
            if ch.is_ascii_uppercase() && !modifiers.contains(KeyModifiers::SHIFT) {
                modifiers |= KeyModifiers::SHIFT;
            }
            KeyCode::Char(ch)
        }
        _ => return Err(anyhow!("Unknown key: '{key_str}'")),
    };

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
    // Only show Shift explicitly for non-character keys (uppercase chars imply Shift)
    if combo.modifiers.contains(KeyModifiers::SHIFT) && !matches!(combo.code, KeyCode::Char(_)) {
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
        KeyCode::Up => format!("{prefix}↑"),
        KeyCode::Down => format!("{prefix}↓"),
        KeyCode::Left => format!("{prefix}←"),
        KeyCode::Right => format!("{prefix}→"),
        KeyCode::Home => format!("{prefix}Home"),
        KeyCode::End => format!("{prefix}End"),
        KeyCode::PageUp => format!("{prefix}PgUp"),
        KeyCode::PageDown => format!("{prefix}PgDn"),
        KeyCode::F(n) => format!("{prefix}F{n}"),
        _ => format!("{prefix}?"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_CONFIG: &str = include_str!("default_config.toml");

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

    #[test]
    fn parse_single_char() {
        let combo = parse_key_combo("q").unwrap();
        assert_eq!(combo.code, KeyCode::Char('q'));
        assert_eq!(combo.modifiers, KeyModifiers::NONE);
    }

    #[test]
    fn parse_uppercase_adds_shift() {
        let combo = parse_key_combo("G").unwrap();
        assert_eq!(combo.code, KeyCode::Char('G'));
        assert_eq!(combo.modifiers, KeyModifiers::SHIFT);
    }

    #[test]
    fn parse_ctrl_modifier() {
        let combo = parse_key_combo("Ctrl+c").unwrap();
        assert_eq!(combo.code, KeyCode::Char('c'));
        assert_eq!(combo.modifiers, KeyModifiers::CONTROL);
    }

    #[test]
    fn parse_ctrl_shift() {
        let combo = parse_key_combo("Ctrl+Shift+a").unwrap();
        assert_eq!(combo.code, KeyCode::Char('a'));
        assert_eq!(combo.modifiers, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    }

    #[test]
    fn parse_named_keys() {
        assert_eq!(parse_key_combo("Enter").unwrap().code, KeyCode::Enter);
        assert_eq!(parse_key_combo("Esc").unwrap().code, KeyCode::Esc);
        assert_eq!(
            parse_key_combo("Backspace").unwrap().code,
            KeyCode::Backspace
        );
        assert_eq!(parse_key_combo("Delete").unwrap().code, KeyCode::Delete);
        assert_eq!(parse_key_combo("Space").unwrap().code, KeyCode::Char(' '));
        assert_eq!(parse_key_combo("Home").unwrap().code, KeyCode::Home);
        assert_eq!(parse_key_combo("End").unwrap().code, KeyCode::End);
        assert_eq!(parse_key_combo("PgUp").unwrap().code, KeyCode::PageUp);
        assert_eq!(parse_key_combo("PgDn").unwrap().code, KeyCode::PageDown);
        assert_eq!(parse_key_combo("PageUp").unwrap().code, KeyCode::PageUp);
        assert_eq!(parse_key_combo("PageDown").unwrap().code, KeyCode::PageDown);
    }

    #[test]
    fn parse_f_keys() {
        assert_eq!(parse_key_combo("F2").unwrap().code, KeyCode::F(2));
        assert_eq!(parse_key_combo("F5").unwrap().code, KeyCode::F(5));
        assert_eq!(parse_key_combo("F12").unwrap().code, KeyCode::F(12));
    }

    #[test]
    fn parse_invalid_key() {
        assert!(parse_key_combo("InvalidKey").is_err());
        assert!(parse_key_combo("Ctrl+InvalidKey").is_err());
        assert!(parse_key_combo("Foo+c").is_err());
    }

    #[test]
    fn format_round_trips() {
        let cases = ["q", "G", "Ctrl+c", "F5", "Enter", "Esc", "Space", "/"];
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
    fn duplicate_key_detected() {
        let result = keybindings_with_override(
            r#"
            [keybindings]
            quit = "j"
            select_next = "j"
            "#,
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("j"), "Error should mention the key: {err}");
    }

    #[test]
    fn override_replaces_default() {
        let kb = keybindings_with_override(
            r#"
            [keybindings]
            quit = "x"
            chmod = "Ctrl+Shift+x"
            cut = "Ctrl+x"
            "#,
        )
        .unwrap();
        // 'x' should now be Quit
        assert_eq!(
            kb.normal_action(&KeyCode::Char('x'), &KeyModifiers::NONE),
            Some(Action::Quit)
        );
        // 'q' should no longer be Quit (overridden)
        assert_eq!(
            kb.normal_action(&KeyCode::Char('q'), &KeyModifiers::NONE),
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
    fn display_for_quit_shows_q() {
        let kb = default_keybindings();
        let display = kb.display_for(Action::Quit);
        assert_eq!(display, "q");
    }

    #[test]
    fn hint_for_quotes_each_key() {
        let kb = default_keybindings();
        let hint = kb.hint_for(&[Action::SelectNext]);
        // Should quote each key individually and join with " or "
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
    fn uppercase_fallback_with_shift() {
        let kb = default_keybindings();
        // SelectLast default is "G" which parses to Char('G') + SHIFT.
        // Terminal might send Char('G') with NONE — fallback should find it.
        assert_eq!(
            kb.normal_action(&KeyCode::Char('G'), &KeyModifiers::NONE),
            Some(Action::SelectLast)
        );
    }

    #[test]
    fn uppercase_fallback_without_shift() {
        let kb = default_keybindings();
        // RangeMark default is "V" which parses to Char('V') + SHIFT.
        // Direct match with SHIFT.
        assert_eq!(
            kb.normal_action(&KeyCode::Char('V'), &KeyModifiers::SHIFT),
            Some(Action::RangeMark)
        );
    }

    #[test]
    fn prompt_action_lookup() {
        let kb = default_keybindings();
        assert_eq!(
            kb.prompt_action(&KeyCode::Enter, &KeyModifiers::NONE),
            Some(Action::PromptSubmit)
        );
        assert_eq!(
            kb.prompt_action(&KeyCode::Char('z'), &KeyModifiers::CONTROL),
            Some(Action::PromptReset)
        );
    }
}

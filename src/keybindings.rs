use std::collections::HashMap;

use anyhow::{Result, anyhow};
use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use serde::Deserialize;

/// An application action that can be triggered by a key press.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Action {
    // Global
    Quit,
    Reset,
    ToggleHelp,
    ClearAlerts,
    ClearProgress,

    // Navigation (filesystem)
    Back,
    Open,
    OpenCustom,
    OpenNewWindow,
    OpenTerminal,
    GoHome,
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
    Delete,
    Rename,
    Filter,

    // Sort
    SortByName,
    SortByModified,
    SortBySize,

    // Prompt
    PromptSubmit,
    PromptCancel,
    PromptReset,
    PromptSelectAll,
    PromptCopy,
    PromptCut,
    PromptPaste,
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

/// Keybindings from the TOML `[keybindings]` section.
/// All fields are required — defaults are provided by the embedded default_config.toml.
#[derive(Debug, Deserialize)]
pub struct TomlKeybindings {
    // Normal mode
    quit: KeySpec,
    toggle_help: KeySpec,
    clear_alerts: KeySpec,
    clear_progress: KeySpec,
    back: KeySpec,
    open: KeySpec,
    open_custom: KeySpec,
    open_new_window: KeySpec,
    open_terminal: KeySpec,
    go_home: KeySpec,
    refresh: KeySpec,
    select_next: KeySpec,
    select_previous: KeySpec,
    select_first: KeySpec,
    select_last: KeySpec,
    select_middle: KeySpec,
    page_up: KeySpec,
    page_down: KeySpec,
    toggle_mark: KeySpec,
    range_mark: KeySpec,
    copy: KeySpec,
    cut: KeySpec,
    paste: KeySpec,
    delete: KeySpec,
    rename: KeySpec,
    filter: KeySpec,
    sort_by_name: KeySpec,
    sort_by_modified: KeySpec,
    sort_by_size: KeySpec,
    // Prompt mode
    prompt_submit: KeySpec,
    prompt_reset: KeySpec,
    prompt_select_all: KeySpec,
    prompt_copy: KeySpec,
    prompt_cut: KeySpec,
    prompt_paste: KeySpec,
}

type BindingList = Vec<(Action, Vec<KeyCombo>)>;

impl TomlKeybindings {
    /// Convert TOML fields into (normal, prompt) binding lists.
    fn to_bindings(&self) -> Result<(BindingList, BindingList)> {
        let mut normal = Vec::new();
        let mut prompt = Vec::new();

        macro_rules! bind {
            ($list:expr, $field:expr, $action:expr) => {
                $list.push(($action, parse_key_spec(&$field)?));
            };
        }

        // Normal mode
        bind!(normal, self.quit, Action::Quit);
        bind!(normal, self.toggle_help, Action::ToggleHelp);
        bind!(normal, self.clear_alerts, Action::ClearAlerts);
        bind!(normal, self.clear_progress, Action::ClearProgress);
        bind!(normal, self.back, Action::Back);
        bind!(normal, self.open, Action::Open);
        bind!(normal, self.open_custom, Action::OpenCustom);
        bind!(normal, self.open_new_window, Action::OpenNewWindow);
        bind!(normal, self.open_terminal, Action::OpenTerminal);
        bind!(normal, self.go_home, Action::GoHome);
        bind!(normal, self.refresh, Action::Refresh);
        bind!(normal, self.select_next, Action::SelectNext);
        bind!(normal, self.select_previous, Action::SelectPrevious);
        bind!(normal, self.select_first, Action::SelectFirst);
        bind!(normal, self.select_last, Action::SelectLast);
        bind!(normal, self.select_middle, Action::SelectMiddle);
        bind!(normal, self.page_up, Action::PageUp);
        bind!(normal, self.page_down, Action::PageDown);
        bind!(normal, self.toggle_mark, Action::ToggleMark);
        bind!(normal, self.range_mark, Action::RangeMark);
        bind!(normal, self.copy, Action::Copy);
        bind!(normal, self.cut, Action::Cut);
        bind!(normal, self.paste, Action::Paste);
        bind!(normal, self.delete, Action::Delete);
        bind!(normal, self.rename, Action::Rename);
        bind!(normal, self.filter, Action::Filter);
        bind!(normal, self.sort_by_name, Action::SortByName);
        bind!(normal, self.sort_by_modified, Action::SortByModified);
        bind!(normal, self.sort_by_size, Action::SortBySize);

        // Prompt mode
        bind!(prompt, self.prompt_submit, Action::PromptSubmit);
        bind!(prompt, self.prompt_reset, Action::PromptReset);
        bind!(prompt, self.prompt_select_all, Action::PromptSelectAll);
        bind!(prompt, self.prompt_copy, Action::PromptCopy);
        bind!(prompt, self.prompt_cut, Action::PromptCut);
        bind!(prompt, self.prompt_paste, Action::PromptPaste);

        Ok((normal, prompt))
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
    pub fn display_for(&self, action: Action) -> &str {
        self.action_display
            .get(&action)
            .map_or("", |s| s.as_str())
    }
}

/// Hardcoded keys per action (arrow keys, Home/End, PageUp/PageDown, Esc).
/// These are always active regardless of config and are included in display strings.
fn hardcoded_keys(action: Action) -> Vec<KeyCombo> {
    let kc = KeyCombo::new;
    match action {
        Action::Back => vec![kc(KeyCode::Left, KeyModifiers::NONE)],
        Action::Open => vec![kc(KeyCode::Right, KeyModifiers::NONE)],
        Action::SelectNext => vec![kc(KeyCode::Down, KeyModifiers::NONE)],
        Action::SelectPrevious => vec![kc(KeyCode::Up, KeyModifiers::NONE)],
        Action::SelectFirst => vec![kc(KeyCode::Home, KeyModifiers::NONE)],
        Action::SelectLast => vec![kc(KeyCode::End, KeyModifiers::NONE)],
        Action::PageUp => vec![kc(KeyCode::PageUp, KeyModifiers::NONE)],
        Action::PageDown => vec![kc(KeyCode::PageDown, KeyModifiers::NONE)],
        Action::Reset => vec![kc(KeyCode::Esc, KeyModifiers::NONE)],
        Action::PromptCancel => vec![kc(KeyCode::Esc, KeyModifiers::NONE)],
        _ => vec![],
    }
}

/// Build the key→action HashMap, detecting duplicate key mappings.
fn build_action_map(bindings: &[(Action, Vec<KeyCombo>)]) -> Result<HashMap<KeyCombo, Action>> {
    let mut map = HashMap::new();
    for (action, combos) in bindings {
        for combo in combos {
            if let Some(existing) = map.insert(*combo, *action) {
                if existing != *action {
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

    // Actions that only have hardcoded keys and no rebindable defaults
    // (e.g., Reset only has Esc hardcoded)
    for action in [Action::Reset, Action::PromptCancel] {
        map.entry(action).or_insert_with(|| {
            let hardcoded = hardcoded_keys(action);
            hardcoded.iter().map(format_key_combo).collect::<Vec<_>>().join("/")
        });
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

    let key_str = parts.last().unwrap();
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
            let ch = s.chars().next().unwrap();
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

    const DEFAULT_CONFIG: &str = include_str!("app/config/default_config.toml");

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
        assert_eq!(
            combo.modifiers,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        );
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
    fn parse_arrow_keys() {
        assert_eq!(parse_key_combo("Up").unwrap().code, KeyCode::Up);
        assert_eq!(parse_key_combo("Down").unwrap().code, KeyCode::Down);
        assert_eq!(parse_key_combo("Left").unwrap().code, KeyCode::Left);
        assert_eq!(parse_key_combo("Right").unwrap().code, KeyCode::Right);
    }

    #[test]
    fn parse_special_chars() {
        assert_eq!(parse_key_combo("/").unwrap().code, KeyCode::Char('/'));
        assert_eq!(parse_key_combo("~").unwrap().code, KeyCode::Char('~'));
        assert_eq!(parse_key_combo("^").unwrap().code, KeyCode::Char('^'));
        assert_eq!(parse_key_combo("$").unwrap().code, KeyCode::Char('$'));
        assert_eq!(parse_key_combo("?").unwrap().code, KeyCode::Char('?'));
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
    fn display_for_reset_shows_esc() {
        let kb = default_keybindings();
        let display = kb.display_for(Action::Reset);
        assert_eq!(display, "Esc");
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
        // SortByName default includes "N" which parses to Char('N') + SHIFT.
        // Direct match with SHIFT.
        assert_eq!(
            kb.normal_action(&KeyCode::Char('N'), &KeyModifiers::SHIFT),
            Some(Action::SortByName)
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

    #[test]
    fn key_spec_single_and_multiple() {
        let single = KeySpec::Single("q".to_string());
        let combos = parse_key_spec(&single).unwrap();
        assert_eq!(combos.len(), 1);

        let multiple = KeySpec::Multiple(vec!["h".to_string(), "b".to_string()]);
        let combos = parse_key_spec(&multiple).unwrap();
        assert_eq!(combos.len(), 2);
    }
}

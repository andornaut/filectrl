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

/// User-configurable keybindings from TOML `[keybindings]` section.
#[derive(Debug, Default, Deserialize)]
pub struct TomlKeybindings {
    quit: Option<KeySpec>,
    reset: Option<KeySpec>,
    toggle_help: Option<KeySpec>,
    clear_alerts: Option<KeySpec>,
    clear_progress: Option<KeySpec>,
    back: Option<KeySpec>,
    open: Option<KeySpec>,
    open_custom: Option<KeySpec>,
    open_new_window: Option<KeySpec>,
    open_terminal: Option<KeySpec>,
    go_home: Option<KeySpec>,
    refresh: Option<KeySpec>,
    select_next: Option<KeySpec>,
    select_previous: Option<KeySpec>,
    select_first: Option<KeySpec>,
    select_last: Option<KeySpec>,
    select_middle: Option<KeySpec>,
    page_up: Option<KeySpec>,
    page_down: Option<KeySpec>,
    toggle_mark: Option<KeySpec>,
    range_mark: Option<KeySpec>,
    copy: Option<KeySpec>,
    cut: Option<KeySpec>,
    paste: Option<KeySpec>,
    delete: Option<KeySpec>,
    rename: Option<KeySpec>,
    filter: Option<KeySpec>,
    sort_by_name: Option<KeySpec>,
    sort_by_modified: Option<KeySpec>,
    sort_by_size: Option<KeySpec>,
    prompt_submit: Option<KeySpec>,
    prompt_cancel: Option<KeySpec>,
    prompt_reset: Option<KeySpec>,
    prompt_select_all: Option<KeySpec>,
    prompt_copy: Option<KeySpec>,
    prompt_cut: Option<KeySpec>,
    prompt_paste: Option<KeySpec>,
}

/// Resolved keybindings with fast lookup in both directions.
#[derive(Debug)]
pub struct KeyBindings {
    normal: HashMap<KeyCombo, Action>,
    prompt: HashMap<KeyCombo, Action>,
    action_display: HashMap<Action, String>,
}

impl KeyBindings {
    pub fn new(overrides: Option<&TomlKeybindings>) -> Result<Self> {
        let mut normal_bindings = default_normal_bindings();
        let mut prompt_bindings = default_prompt_bindings();

        if let Some(toml) = overrides {
            apply_overrides(&mut normal_bindings, &mut prompt_bindings, toml)?;
        }

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

// Helper to create a KeyCombo concisely
const fn kc(code: KeyCode, modifiers: KeyModifiers) -> KeyCombo {
    KeyCombo::new(code, modifiers)
}

/// Hardcoded keys per action (arrow keys, Home/End, PageUp/PageDown, Esc).
/// These are always active regardless of config and are included in display strings.
fn hardcoded_keys(action: Action) -> Vec<KeyCombo> {
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

/// Default rebindable bindings for normal mode.
fn default_normal_bindings() -> Vec<(Action, Vec<KeyCombo>)> {
    use KeyCode::*;
    let none = KeyModifiers::NONE;
    let ctrl = KeyModifiers::CONTROL;
    let shift = KeyModifiers::SHIFT;

    vec![
        // Global
        (Action::Quit, vec![kc(Char('q'), none)]),
        (Action::ToggleHelp, vec![kc(Char('?'), none)]),
        (Action::ClearAlerts, vec![kc(Char('a'), none)]),
        (Action::ClearProgress, vec![kc(Char('p'), none)]),
        // Navigation (filesystem)
        (
            Action::Back,
            vec![kc(Char('h'), none), kc(Char('b'), none), kc(Backspace, none)],
        ),
        (
            Action::Open,
            vec![
                kc(Char('l'), none),
                kc(Char('f'), none),
                kc(Enter, none),
                kc(Char(' '), none),
            ],
        ),
        (Action::OpenCustom, vec![kc(Char('o'), none)]),
        (Action::OpenNewWindow, vec![kc(Char('w'), none)]),
        (Action::OpenTerminal, vec![kc(Char('t'), none)]),
        (Action::GoHome, vec![kc(Char('~'), none)]),
        (Action::Refresh, vec![kc(Char('r'), ctrl), kc(F(5), none)]),
        // Selection
        (Action::SelectNext, vec![kc(Char('j'), none)]),
        (Action::SelectPrevious, vec![kc(Char('k'), none)]),
        (
            Action::SelectFirst,
            vec![kc(Char('g'), none), kc(Char('^'), none)],
        ),
        (
            Action::SelectLast,
            vec![kc(Char('G'), shift), kc(Char('$'), none)],
        ),
        (Action::SelectMiddle, vec![kc(Char('z'), none)]),
        (
            Action::PageUp,
            vec![kc(Char('u'), ctrl), kc(Char('b'), ctrl)],
        ),
        (
            Action::PageDown,
            vec![kc(Char('d'), ctrl), kc(Char('f'), ctrl)],
        ),
        // Marks
        (Action::ToggleMark, vec![kc(Char('v'), none)]),
        (Action::RangeMark, vec![kc(Char('V'), shift)]),
        // Clipboard
        (Action::Copy, vec![kc(Char('c'), ctrl)]),
        (Action::Cut, vec![kc(Char('x'), ctrl)]),
        (Action::Paste, vec![kc(Char('v'), ctrl)]),
        // File operations
        (Action::Delete, vec![kc(Delete, none)]),
        (Action::Rename, vec![kc(Char('r'), none), kc(F(2), none)]),
        (Action::Filter, vec![kc(Char('/'), none)]),
        // Sort
        (
            Action::SortByName,
            vec![kc(Char('n'), none), kc(Char('N'), shift)],
        ),
        (
            Action::SortByModified,
            vec![kc(Char('m'), none), kc(Char('M'), shift)],
        ),
        (
            Action::SortBySize,
            vec![kc(Char('s'), none), kc(Char('S'), shift)],
        ),
    ]
}

/// Default rebindable bindings for prompt mode.
fn default_prompt_bindings() -> Vec<(Action, Vec<KeyCombo>)> {
    use KeyCode::*;
    let ctrl = KeyModifiers::CONTROL;
    let ctrl_shift = KeyModifiers::CONTROL.union(KeyModifiers::SHIFT);

    vec![
        (Action::PromptSubmit, vec![kc(Enter, KeyModifiers::NONE)]),
        (Action::PromptReset, vec![kc(Char('z'), ctrl)]),
        (Action::PromptSelectAll, vec![kc(Char('a'), ctrl_shift)]),
        (Action::PromptCopy, vec![kc(Char('c'), ctrl)]),
        (Action::PromptCut, vec![kc(Char('x'), ctrl)]),
        (Action::PromptPaste, vec![kc(Char('v'), ctrl)]),
    ]
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

/// Apply user TOML overrides to the default bindings.
fn apply_overrides(
    normal: &mut Vec<(Action, Vec<KeyCombo>)>,
    prompt: &mut Vec<(Action, Vec<KeyCombo>)>,
    toml: &TomlKeybindings,
) -> Result<()> {
    macro_rules! override_binding {
        ($bindings:expr, $field:expr, $action:expr) => {
            if let Some(spec) = &$field {
                let combos = parse_key_spec(spec)?;
                if let Some(entry) = $bindings.iter_mut().find(|(a, _)| *a == $action) {
                    entry.1 = combos;
                } else {
                    $bindings.push(($action, combos));
                }
            }
        };
    }

    // Normal mode overrides
    override_binding!(normal, toml.quit, Action::Quit);
    override_binding!(normal, toml.reset, Action::Reset);
    override_binding!(normal, toml.toggle_help, Action::ToggleHelp);
    override_binding!(normal, toml.clear_alerts, Action::ClearAlerts);
    override_binding!(normal, toml.clear_progress, Action::ClearProgress);
    override_binding!(normal, toml.back, Action::Back);
    override_binding!(normal, toml.open, Action::Open);
    override_binding!(normal, toml.open_custom, Action::OpenCustom);
    override_binding!(normal, toml.open_new_window, Action::OpenNewWindow);
    override_binding!(normal, toml.open_terminal, Action::OpenTerminal);
    override_binding!(normal, toml.go_home, Action::GoHome);
    override_binding!(normal, toml.refresh, Action::Refresh);
    override_binding!(normal, toml.select_next, Action::SelectNext);
    override_binding!(normal, toml.select_previous, Action::SelectPrevious);
    override_binding!(normal, toml.select_first, Action::SelectFirst);
    override_binding!(normal, toml.select_last, Action::SelectLast);
    override_binding!(normal, toml.select_middle, Action::SelectMiddle);
    override_binding!(normal, toml.page_up, Action::PageUp);
    override_binding!(normal, toml.page_down, Action::PageDown);
    override_binding!(normal, toml.toggle_mark, Action::ToggleMark);
    override_binding!(normal, toml.range_mark, Action::RangeMark);
    override_binding!(normal, toml.copy, Action::Copy);
    override_binding!(normal, toml.cut, Action::Cut);
    override_binding!(normal, toml.paste, Action::Paste);
    override_binding!(normal, toml.delete, Action::Delete);
    override_binding!(normal, toml.rename, Action::Rename);
    override_binding!(normal, toml.filter, Action::Filter);
    override_binding!(normal, toml.sort_by_name, Action::SortByName);
    override_binding!(normal, toml.sort_by_modified, Action::SortByModified);
    override_binding!(normal, toml.sort_by_size, Action::SortBySize);

    // Prompt mode overrides
    override_binding!(prompt, toml.prompt_submit, Action::PromptSubmit);
    override_binding!(prompt, toml.prompt_cancel, Action::PromptCancel);
    override_binding!(prompt, toml.prompt_reset, Action::PromptReset);
    override_binding!(prompt, toml.prompt_select_all, Action::PromptSelectAll);
    override_binding!(prompt, toml.prompt_copy, Action::PromptCopy);
    override_binding!(prompt, toml.prompt_cut, Action::PromptCut);
    override_binding!(prompt, toml.prompt_paste, Action::PromptPaste);

    Ok(())
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
    fn default_bindings_have_no_conflicts() {
        KeyBindings::new(None).unwrap();
    }

    #[test]
    fn duplicate_key_detected() {
        let toml = TomlKeybindings {
            quit: Some(KeySpec::Single("j".to_string())),
            select_next: Some(KeySpec::Single("j".to_string())),
            ..Default::default()
        };
        let result = KeyBindings::new(Some(&toml));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("j"), "Error should mention the key: {err}");
    }

    #[test]
    fn override_replaces_default() {
        let toml = TomlKeybindings {
            quit: Some(KeySpec::Single("x".to_string())),
            ..Default::default()
        };
        let kb = KeyBindings::new(Some(&toml)).unwrap();
        // 'x' should now be Quit
        assert_eq!(
            kb.normal_action(&KeyCode::Char('x'), &KeyModifiers::NONE),
            Some(Action::Quit)
        );
        // 'q' should no longer be Quit
        assert_eq!(
            kb.normal_action(&KeyCode::Char('q'), &KeyModifiers::NONE),
            None
        );
    }

    #[test]
    fn display_includes_hardcoded_keys() {
        let kb = KeyBindings::new(None).unwrap();
        let display = kb.display_for(Action::SelectNext);
        assert!(
            display.contains('↓'),
            "SelectNext display should include hardcoded ↓: {display}"
        );
        assert!(
            display.contains('j'),
            "SelectNext display should include rebindable j: {display}"
        );
    }

    #[test]
    fn display_for_reset_shows_esc() {
        let kb = KeyBindings::new(None).unwrap();
        let display = kb.display_for(Action::Reset);
        assert_eq!(display, "Esc");
    }

    #[test]
    fn uppercase_fallback_with_shift() {
        let kb = KeyBindings::new(None).unwrap();
        // SelectLast has default kc(Char('G'), SHIFT)
        // Terminal might send Char('G') with NONE — fallback should find it
        assert_eq!(
            kb.normal_action(&KeyCode::Char('G'), &KeyModifiers::NONE),
            Some(Action::SelectLast)
        );
    }

    #[test]
    fn uppercase_fallback_without_shift() {
        let kb = KeyBindings::new(None).unwrap();
        // SortByName has default kc(Char('N'), SHIFT)
        // Terminal might send Char('N') with SHIFT — direct match
        assert_eq!(
            kb.normal_action(&KeyCode::Char('N'), &KeyModifiers::SHIFT),
            Some(Action::SortByName)
        );
    }

    #[test]
    fn prompt_action_lookup() {
        let kb = KeyBindings::new(None).unwrap();
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

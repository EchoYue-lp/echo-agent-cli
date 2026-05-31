//! Customizable keymap for the TUI.
//!
//! Default bindings can be overridden by `~/.echo-agent/keymap.yaml`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// All actions the TUI can perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyAction {
    Quit,
    Send,
    NewLine,
    Cancel,
    HistoryUp,
    HistoryDown,
    CursorLeft,
    CursorRight,
    CursorHome,
    CursorEnd,
    DeleteBack,
    DeleteForward,
    ScrollUp,
    ScrollDown,
    ToggleSidebar,
    NextSidebarTab,
    ClearChat,
    CompletionNext,
    CompletionPrev,
    CompletionAccept,
    PopupClose,
    Approve,
    Deny,
    PickerUp,
    PickerDown,
    PickerSelect,
}

/// A single key binding: key + modifiers -> action.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyBinding {
    pub code: KeyCodeDef,
    #[serde(default)]
    pub modifiers: ModifiersDef,
}

/// Serializable KeyCode subset.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyCodeDef {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

/// Serializable modifier flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct ModifiersDef {
    #[serde(default)]
    pub control: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
}

impl KeyBinding {
    pub fn new(code: KeyCodeDef, modifiers: ModifiersDef) -> Self {
        Self { code, modifiers }
    }

    pub fn plain(code: KeyCodeDef) -> Self {
        Self {
            code,
            modifiers: ModifiersDef::default(),
        }
    }

    pub fn ctrl(c: char) -> Self {
        Self {
            code: KeyCodeDef::Char(c),
            modifiers: ModifiersDef {
                control: true,
                shift: false,
                alt: false,
            },
        }
    }

    /// Check if this binding matches a given crossterm KeyEvent.
    pub fn matches(&self, event: &KeyEvent) -> bool {
        let code_match = match &self.code {
            KeyCodeDef::Char(c) => event.code == KeyCode::Char(*c),
            KeyCodeDef::Enter => event.code == KeyCode::Enter,
            KeyCodeDef::Esc => event.code == KeyCode::Esc,
            KeyCodeDef::Tab => event.code == KeyCode::Tab,
            KeyCodeDef::BackTab => event.code == KeyCode::BackTab,
            KeyCodeDef::Backspace => event.code == KeyCode::Backspace,
            KeyCodeDef::Delete => event.code == KeyCode::Delete,
            KeyCodeDef::Left => event.code == KeyCode::Left,
            KeyCodeDef::Right => event.code == KeyCode::Right,
            KeyCodeDef::Up => event.code == KeyCode::Up,
            KeyCodeDef::Down => event.code == KeyCode::Down,
            KeyCodeDef::Home => event.code == KeyCode::Home,
            KeyCodeDef::End => event.code == KeyCode::End,
            KeyCodeDef::PageUp => event.code == KeyCode::PageUp,
            KeyCodeDef::PageDown => event.code == KeyCode::PageDown,
        };
        if !code_match {
            return false;
        }
        let mut expected = KeyModifiers::empty();
        if self.modifiers.control {
            expected |= KeyModifiers::CONTROL;
        }
        if self.modifiers.shift {
            expected |= KeyModifiers::SHIFT;
        }
        if self.modifiers.alt {
            expected |= KeyModifiers::ALT;
        }
        // Only check the modifiers we care about; ignore others.
        event.modifiers.contains(expected)
            && (!self.modifiers.control || event.modifiers.contains(KeyModifiers::CONTROL))
            && (!self.modifiers.shift || event.modifiers.contains(KeyModifiers::SHIFT))
            && (!self.modifiers.alt || event.modifiers.contains(KeyModifiers::ALT))
    }
}

/// The complete keymap: binding -> action.
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: Vec<(KeyBinding, KeyAction)>,
    /// Quick lookup by action for display purposes.
    action_labels: HashMap<KeyAction, String>,
}

impl Default for Keymap {
    fn default() -> Self {
        let mut bindings = vec![
            // Global
            (KeyBinding::ctrl('c'), KeyAction::Quit),
            (KeyBinding::ctrl('q'), KeyAction::Quit),
            (KeyBinding::ctrl('b'), KeyAction::ToggleSidebar),
            (KeyBinding::ctrl('l'), KeyAction::ClearChat),
            // Input
            (KeyBinding::plain(KeyCodeDef::Enter), KeyAction::Send),
            (KeyBinding::plain(KeyCodeDef::Esc), KeyAction::Cancel),
            (KeyBinding::plain(KeyCodeDef::Backspace), KeyAction::DeleteBack),
            (KeyBinding::plain(KeyCodeDef::Delete), KeyAction::DeleteForward),
            (KeyBinding::plain(KeyCodeDef::Left), KeyAction::CursorLeft),
            (KeyBinding::plain(KeyCodeDef::Right), KeyAction::CursorRight),
            (KeyBinding::plain(KeyCodeDef::Home), KeyAction::CursorHome),
            (KeyBinding::plain(KeyCodeDef::End), KeyAction::CursorEnd),
            (KeyBinding::plain(KeyCodeDef::Up), KeyAction::HistoryUp),
            (KeyBinding::plain(KeyCodeDef::Down), KeyAction::HistoryDown),
            (KeyBinding::plain(KeyCodeDef::PageUp), KeyAction::ScrollUp),
            (KeyBinding::plain(KeyCodeDef::PageDown), KeyAction::ScrollDown),
            (KeyBinding::plain(KeyCodeDef::Tab), KeyAction::NextSidebarTab),
            // Completion
            (KeyBinding::plain(KeyCodeDef::BackTab), KeyAction::CompletionPrev),
            // Approval
            (
                KeyBinding::plain(KeyCodeDef::Char('y')),
                KeyAction::Approve,
            ),
            (
                KeyBinding::plain(KeyCodeDef::Char('n')),
                KeyAction::Deny,
            ),
            // Picker
            (KeyBinding::plain(KeyCodeDef::Up), KeyAction::PickerUp),
            (KeyBinding::plain(KeyCodeDef::Down), KeyAction::PickerDown),
            (KeyBinding::plain(KeyCodeDef::Enter), KeyAction::PickerSelect),
        ];

        // NewLine is Shift+Enter which we handle specially
        bindings.push((
            KeyBinding::new(
                KeyCodeDef::Enter,
                ModifiersDef {
                    control: false,
                    shift: true,
                    alt: false,
                },
            ),
            KeyAction::NewLine,
        ));

        let action_labels = Self::build_labels(&bindings);

        Self {
            bindings,
            action_labels,
        }
    }
}

impl Keymap {
    fn build_labels(bindings: &[(KeyBinding, KeyAction)]) -> HashMap<KeyAction, String> {
        let mut map = HashMap::new();
        for (binding, action) in bindings {
            let label = format!(
                "{}{}",
                if binding.modifiers.control { "C-" } else { "" },
                match &binding.code {
                    KeyCodeDef::Char(c) => c.to_string(),
                    KeyCodeDef::Enter => "Enter".into(),
                    KeyCodeDef::Esc => "Esc".into(),
                    KeyCodeDef::Tab => "Tab".into(),
                    KeyCodeDef::BackTab => "S-Tab".into(),
                    KeyCodeDef::Backspace => "BS".into(),
                    KeyCodeDef::Delete => "Del".into(),
                    KeyCodeDef::Left => "Left".into(),
                    KeyCodeDef::Right => "Right".into(),
                    KeyCodeDef::Up => "Up".into(),
                    KeyCodeDef::Down => "Down".into(),
                    KeyCodeDef::Home => "Home".into(),
                    KeyCodeDef::End => "End".into(),
                    KeyCodeDef::PageUp => "PgUp".into(),
                    KeyCodeDef::PageDown => "PgDn".into(),
                }
            );
            map.entry(*action).or_insert(label);
        }
        map
    }

    /// Resolve a key event to an action.
    pub fn resolve(&self, event: &KeyEvent) -> Option<KeyAction> {
        self.bindings
            .iter()
            .find(|(b, _)| b.matches(event))
            .map(|(_, a)| *a)
    }

    /// Get the label for an action (for help display).
    pub fn label(&self, action: KeyAction) -> Option<&str> {
        self.action_labels.get(&action).map(|s| s.as_str())
    }

    /// Try loading keymap overrides from `~/.echo-agent/keymap.yaml`.
    pub fn load_overrides(&mut self) {
        let path = Self::config_path();
        let Some(path) = path else { return };
        if !path.exists() {
            return;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(overrides): Result<Vec<(KeyBinding, KeyAction)>, _> =
            serde_yaml::from_str(&content)
        else {
            return;
        };
        // Prepend overrides so they take priority.
        let mut new_bindings = overrides;
        new_bindings.extend(self.bindings.iter().cloned());
        self.action_labels = Self::build_labels(&new_bindings);
        self.bindings = new_bindings;
    }

    fn config_path() -> Option<PathBuf> {
        std::env::var("HOME")
            .ok()
            .map(|h| PathBuf::from(h).join(".echo-agent").join("keymap.yaml"))
    }
}

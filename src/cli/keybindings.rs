//! 增强的键盘快捷键绑定
//!
//! 提供完整的 Emacs 和 Vi 键绑定配置，支持从 YAML 文件加载自定义绑定。

use reedline::{
    EditCommand, KeyCode, KeyModifiers, Keybindings, ReedlineEvent, default_emacs_keybindings,
    default_vi_insert_keybindings,
};
use serde::Deserialize;
use std::path::Path;

/// 键绑定模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeybindingMode {
    /// Emacs 风格 (默认)
    Emacs,
    /// Vi 风格
    Vi,
}

// ── Custom keybindings YAML support ──────────────────────────────────────────

/// A single custom keybinding entry from `~/.eko/keybindings.yaml`.
#[derive(Debug, Clone, Deserialize)]
pub struct KeybindingEntry {
    /// Human-readable key description, e.g. `Ctrl+L`, `Alt+Enter`, `F5`.
    pub key: String,
    /// Action name, e.g. `Clear`, `InsertNewline`, `SearchHistory`,
    /// or `ExecuteHostCommand:clear`.
    pub action: String,
}

/// Wrapper for the YAML top-level key.
#[derive(Debug, Clone, Deserialize)]
struct KeybindingsFile {
    #[serde(default)]
    keybindings: Vec<KeybindingEntry>,
}

/// Load custom keybindings from a YAML file (if it exists).
///
/// Returns an empty list if the file does not exist or cannot be parsed.
pub fn load_custom_keybindings(path: &Path) -> Vec<KeybindingEntry> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    match serde_yaml::from_str::<KeybindingsFile>(&content) {
        Ok(file) => file.keybindings,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to parse keybindings YAML; using defaults"
            );
            Vec::new()
        }
    }
}

/// Parse a key string like `Ctrl+L` or `Alt+Enter` into reedline modifier + code.
fn parse_key(key: &str) -> Option<(KeyModifiers, KeyCode)> {
    let (mod_str, code_str) = if let Some(rest) = key.strip_prefix("Ctrl+") {
        (KeyModifiers::CONTROL, rest)
    } else if let Some(rest) = key.strip_prefix("Alt+") {
        (KeyModifiers::ALT, rest)
    } else if let Some(rest) = key.strip_prefix("Shift+") {
        (KeyModifiers::SHIFT, rest)
    } else {
        (KeyModifiers::NONE, key)
    };

    let code = match code_str {
        "Enter" => KeyCode::Enter,
        "Tab" => KeyCode::Tab,
        "Backspace" => KeyCode::Backspace,
        "Esc" | "Escape" => KeyCode::Esc,
        "Space" => KeyCode::Char(' '),
        "Delete" => KeyCode::Delete,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        s if s.starts_with('F') && s.chars().count() > 1 => {
            let n: u8 = s.strip_prefix('F')?.parse().ok()?;
            KeyCode::F(n)
        }
        s => {
            let mut chars = s.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(ch)
        }
    };

    Some((mod_str, code))
}

/// Parse an action string into a [`ReedlineEvent`].
fn parse_action(action: &str) -> Option<ReedlineEvent> {
    match action {
        "Clear" | "ClearLine" => Some(ReedlineEvent::Edit(vec![EditCommand::Clear])),
        "InsertNewline" => Some(ReedlineEvent::Edit(vec![EditCommand::InsertNewline])),
        "CutWordLeft" => Some(ReedlineEvent::Edit(vec![EditCommand::CutWordLeft])),
        "CutToEnd" => Some(ReedlineEvent::Edit(vec![EditCommand::CutToEnd])),
        "MoveToLineStart" => Some(ReedlineEvent::Edit(vec![EditCommand::MoveToLineStart {
            select: false,
        }])),
        "MoveToLineEnd" => Some(ReedlineEvent::Edit(vec![EditCommand::MoveToLineEnd {
            select: false,
        }])),
        "SearchHistory" => Some(ReedlineEvent::SearchHistory),
        action if action.starts_with("ExecuteHostCommand:") => {
            let cmd = action.strip_prefix("ExecuteHostCommand:")?;
            Some(ReedlineEvent::ExecuteHostCommand(cmd.to_string()))
        }
        _ => {
            tracing::warn!(%action, "Unknown keybinding action; ignoring");
            None
        }
    }
}

/// Apply custom keybinding entries on top of existing defaults.
///
/// Each entry overrides the default binding for that key combination.
/// Entries with unrecognized keys or actions are silently skipped (with a
/// `tracing::warn!`).
pub fn apply_custom_keybindings(kb: &mut Keybindings, entries: &[KeybindingEntry]) {
    for entry in entries {
        let Some((mods, code)) = parse_key(&entry.key) else {
            tracing::warn!(key = %entry.key, "Unrecognized key; skipping custom binding");
            continue;
        };
        let Some(event) = parse_action(&entry.action) else {
            continue;
        };
        kb.add_binding(mods, code, event);
    }
}

/// 构建增强的键绑定
///
/// 在默认绑定基础上增加:
/// - Ctrl+R: 历史搜索
/// - Alt+Enter: 插入换行 (多行输入)
/// - Ctrl+L: 清屏
/// - Ctrl+U: 清除当前行
/// - Alt+Backspace: 删除前一个单词
pub fn create_enhanced_keybindings(mode: KeybindingMode) -> Keybindings {
    match mode {
        KeybindingMode::Emacs => create_emacs_keybindings(),
        KeybindingMode::Vi => create_vi_keybindings(),
    }
}

fn create_emacs_keybindings() -> Keybindings {
    let mut kb = default_emacs_keybindings();

    // Ctrl+L: 清屏
    kb.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('l'),
        ReedlineEvent::ExecuteHostCommand("clear".to_string()),
    );

    // Ctrl+R: 反向历史搜索
    kb.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('r'),
        ReedlineEvent::SearchHistory,
    );

    // Ctrl+U: 清除当前行
    kb.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('u'),
        ReedlineEvent::Edit(vec![EditCommand::Clear]),
    );

    // Alt+Enter: 插入换行
    kb.add_binding(
        KeyModifiers::ALT,
        KeyCode::Enter,
        ReedlineEvent::Edit(vec![EditCommand::InsertNewline]),
    );

    // Alt+Backspace: 删除前一个单词 (使用 CutWordLeft)
    kb.add_binding(
        KeyModifiers::ALT,
        KeyCode::Backspace,
        ReedlineEvent::Edit(vec![EditCommand::CutWordLeft]),
    );

    // Ctrl+W: 删除前一个单词 (备用)
    kb.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('w'),
        ReedlineEvent::Edit(vec![EditCommand::CutWordLeft]),
    );

    // Ctrl+K: 删除到行尾
    kb.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('k'),
        ReedlineEvent::Edit(vec![EditCommand::CutToEnd]),
    );

    // Ctrl+A: 跳到行首
    kb.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('a'),
        ReedlineEvent::Edit(vec![EditCommand::MoveToLineStart { select: false }]),
    );

    // Ctrl+E: 跳到行尾
    kb.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('e'),
        ReedlineEvent::Edit(vec![EditCommand::MoveToLineEnd { select: false }]),
    );

    // Esc / Ctrl+C: 取消当前输入
    kb.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('c'),
        ReedlineEvent::Edit(vec![EditCommand::Clear]),
    );

    kb
}

fn create_vi_keybindings() -> Keybindings {
    let mut kb = default_vi_insert_keybindings();

    // 在 Insert 模式也追加常用快捷键
    kb.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('l'),
        ReedlineEvent::ExecuteHostCommand("clear".to_string()),
    );

    kb.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('r'),
        ReedlineEvent::SearchHistory,
    );

    kb
}

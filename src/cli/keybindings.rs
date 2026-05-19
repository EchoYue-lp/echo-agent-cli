//! 增强的键盘快捷键绑定
//!
//! 提供完整的 Emacs 和 Vi 键绑定配置。

use reedline::{
    default_emacs_keybindings, default_vi_insert_keybindings,
    EditCommand, KeyCode, KeyModifiers, Keybindings, ReedlineEvent,
};

/// 键绑定模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeybindingMode {
    /// Emacs 风格 (默认)
    Emacs,
    /// Vi 风格
    Vi,
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

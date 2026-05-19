//! 增强的 Reedline 编辑器工厂
//!
//! 封装编辑器创建流程，集成增强补全器、键绑定、菜单和历史。

use reedline::{
    default_vi_normal_keybindings, ColumnarMenu, Emacs, FileBackedHistory, MenuBuilder, Reedline,
    ReedlineMenu, Vi,
};

use super::completion::EnhancedCompleter;
use super::keybindings::{create_enhanced_keybindings, KeybindingMode};

/// 编辑器配置
pub struct EditorConfig {
    /// 提示符名称
    pub prompt: String,
    /// 历史文件路径
    pub history_file: String,
    /// 历史最大条目数
    pub history_size: usize,
    /// 键绑定模式
    pub keybinding_mode: KeybindingMode,
    /// 是否显示补全菜单
    pub show_completion_menu: bool,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            prompt: "echo".to_string(),
            history_file: "~/.echo-agent/history.txt".to_string(),
            history_size: 10000,
            keybinding_mode: KeybindingMode::Emacs,
            show_completion_menu: true,
        }
    }
}

/// 创建增强版的 Reedline 编辑器
pub fn create_enhanced_editor(config: &EditorConfig) -> anyhow::Result<Reedline> {
    // 扩展路径
    let history_path = shellexpand::tilde(&config.history_file);
    let history_path = std::path::Path::new(history_path.as_ref());

    // 创建历史目录
    if let Some(parent) = history_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    // 创建历史记录
    let history = FileBackedHistory::with_file(config.history_size, history_path.to_path_buf())?;

    // 创建补全器
    let completer = EnhancedCompleter::new();

    // 创建键绑定
    let keybindings = create_enhanced_keybindings(config.keybinding_mode);

    // 构建编辑器
    let mut builder = Reedline::create()
        .with_history(Box::new(history))
        .with_completer(Box::new(completer));

    // 添加补全菜单
    if config.show_completion_menu {
        let menu = ReedlineMenu::EngineCompleter(Box::new(
            ColumnarMenu::default().with_name("completion_menu"),
        ));
        builder = builder.with_menu(menu);
    }

    // 设置编辑模式
    let editor = match config.keybinding_mode {
        KeybindingMode::Emacs => builder.with_edit_mode(Box::new(Emacs::new(keybindings))),
        KeybindingMode::Vi => {
            // Vi 模式需要 insert 和 normal 两套键绑定
            let normal_kb = default_vi_normal_keybindings();
            builder.with_edit_mode(Box::new(Vi::new(keybindings, normal_kb)))
        }
    };

    Ok(editor)
}

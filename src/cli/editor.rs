//! 增强的 Reedline 编辑器工厂
//!
//! 封装编辑器创建流程，集成增强补全器、键绑定、菜单和历史。

use reedline::{
    ColumnarMenu, Emacs, FileBackedHistory, MenuBuilder, Reedline, ReedlineMenu, Vi,
    default_vi_normal_keybindings,
};

use super::completion::EnhancedCompleter;
use super::keybindings::{
    KeybindingMode, apply_custom_keybindings, create_enhanced_keybindings, load_custom_keybindings,
};

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
    /// 自定义键绑定文件路径 (`~/.eko/keybindings.yaml`)。
    /// 如果文件不存在或解析失败，则使用默认绑定。
    pub keybindings_path: Option<String>,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            prompt: "echo".to_string(),
            history_file: echo_agent_app_core::data_root::user_data_path("history.txt")
                .to_string_lossy()
                .into_owned(),
            history_size: 10000,
            keybinding_mode: KeybindingMode::Emacs,
            show_completion_menu: true,
            keybindings_path: Some(
                echo_agent_app_core::data_root::user_data_path("keybindings.yaml")
                    .to_string_lossy()
                    .into_owned(),
            ),
        }
    }
}

/// 创建增强版的 Reedline 编辑器
pub fn create_enhanced_editor(
    config: &EditorConfig,
    external_printer: reedline::ExternalPrinter<String>,
) -> anyhow::Result<Reedline> {
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

    // 创建键绑定（默认）
    let mut keybindings = create_enhanced_keybindings(config.keybinding_mode);

    // 加载并应用自定义键绑定
    if let Some(ref path_str) = config.keybindings_path {
        let expanded = shellexpand::tilde(path_str);
        let custom_path = std::path::Path::new(expanded.as_ref());
        let custom_entries = load_custom_keybindings(custom_path);
        if !custom_entries.is_empty() {
            tracing::info!(
                count = custom_entries.len(),
                path = %custom_path.display(),
                "Loaded custom keybindings"
            );
            apply_custom_keybindings(&mut keybindings, &custom_entries);
        }
    }

    // 构建编辑器
    let mut builder = Reedline::create()
        .with_external_printer(external_printer)
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

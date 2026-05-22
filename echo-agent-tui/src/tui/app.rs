//! TUI 应用核心
//!
//! 管理事件循环、布局和所有面板状态。

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};

use echo_agent::agent::{AgentEvent, CancellationToken};
use echo_agent::prelude::*;

use crate::agent_handle::AgentHandle;
use crate::output::ColorTheme;

use super::panels::chat::{ChatPanel, MessageRole};
use super::panels::context::{ContextInfo, ContextPanel};
use super::panels::input::InputPanel;
use super::panels::tools::ToolsPanel;
use super::status_bar::{ConnectionStatus, StatusBar};
use super::theme::TuiColors;

/// 面板焦点
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Chat,
    Input,
    Tools,
    Context,
}

/// TUI 应用状态
struct TuiApp {
    chat: ChatPanel,
    tools: ToolsPanel,
    context: ContextPanel,
    input: InputPanel,
    status: StatusBar,
    agent: AgentHandle,
    colors: TuiColors,
    focus: Focus,
    quit: bool,
    /// 是否正在等待 Agent 流式响应
    streaming: bool,
    /// Agent 事件接收器
    event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<AgentEvent>>,
    /// 取消令牌
    cancel_token: Option<CancellationToken>,
}

impl TuiApp {
    async fn new(agent: AgentHandle) -> Self {
        let theme = ColorTheme::dark();
        let colors = TuiColors::from_theme(&theme);

        let model = agent.read(|a| a.model_name().to_string()).await;

        // 收集 agent 元数据
        let (mcp_servers, skills, tools) = agent
            .read(|a| {
                let mcp: Vec<String> = a
                    .mcp_server_names()
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect();
                let skills: Vec<String> =
                    a.skill_names().into_iter().map(|s| s.to_string()).collect();
                let tools: Vec<String> =
                    a.tool_names().into_iter().map(|s| s.to_string()).collect();
                (mcp, skills, tools)
            })
            .await;

        let mut ctx = ContextPanel::new();
        ctx.update(ContextInfo {
            model: model.clone(),
            mcp_servers,
            skills,
            tools,
            theme_name: theme.name.to_string(),
            ..Default::default()
        });

        Self {
            chat: ChatPanel::new(),
            tools: ToolsPanel::new(),
            context: ctx,
            input: InputPanel::new(),
            status: StatusBar::new(model),
            agent,
            colors,
            focus: Focus::Input,
            quit: false,
            streaming: false,
            event_rx: None,
            cancel_token: None,
        }
    }

    /// 启动事件循环
    async fn run(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> anyhow::Result<()> {
        // 欢迎消息
        self.chat.add_message(
            MessageRole::System,
            "欢迎使用 Echo Agent TUI 模式！输入消息并按 Enter 开始对话。".into(),
        );

        loop {
            // 渲染
            terminal.draw(|f| self.render_ui(f))?;

            // 处理事件
            if self.streaming {
                self.process_streaming_events().await?;
            } else {
                self.process_idle_events().await?;
            }

            if self.quit {
                break;
            }
        }

        Ok(())
    }

    /// 空闲状态下处理事件
    async fn process_idle_events(&mut self) -> anyhow::Result<()> {
        // 等待键盘事件（60fps 轮询）
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            && (key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat)
        {
            self.handle_key_event(key);
        }
        Ok(())
    }

    /// 流式状态下同时处理键盘事件和 Agent 事件
    async fn process_streaming_events(&mut self) -> anyhow::Result<()> {
        // 使用短超时轮询键盘事件
        if event::poll(Duration::from_millis(16))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Esc | KeyCode::Char('c')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    // 取消当前流
                    if let Some(token) = self.cancel_token.take() {
                        token.cancel();
                    }
                }
                _ => {}
            }
        }

        // 消费所有可用的 Agent 事件（先取出 rx 再处理以解决借用冲突）
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(ref mut rx) = self.event_rx {
            loop {
                match rx.try_recv() {
                    Ok(event) => events.push(event),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if disconnected {
            self.event_rx = None;
            self.streaming = false;
            self.status.status = ConnectionStatus::Idle;
            self.chat.finish_streaming();
            self.status.help_text.clone_from(&String::new());
        }
        for event in events {
            self.handle_agent_event(event);
        }

        Ok(())
    }

    /// 处理键盘事件
    fn handle_key_event(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if !self.input.is_empty() {
                    self.input.clear();
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
            }
            KeyCode::Tab => self.cycle_focus(),
            KeyCode::BackTab => self.reverse_focus(),

            // 输入面板操作
            _ if self.focus == Focus::Input => match key.code {
                KeyCode::Enter if !self.input.is_empty() => {
                    self.submit_message();
                }
                KeyCode::Char(c) => self.input.insert_char(c),
                KeyCode::Backspace => self.input.delete_backward(),
                KeyCode::Delete => self.input.delete_forward(),
                KeyCode::Left => self.input.cursor_left(),
                KeyCode::Right => self.input.cursor_right(),
                KeyCode::Home => self.input.cursor_home(),
                KeyCode::End => self.input.cursor_end(),
                KeyCode::BackTab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.input.delete_word_backward();
                }
                _ => {}
            },

            // 对话面板滚动
            _ if self.focus == Focus::Chat => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.chat.scroll_up(3),
                KeyCode::Down | KeyCode::Char('j') => self.chat.scroll_down(3),
                KeyCode::PageUp => self.chat.scroll_up(10),
                KeyCode::PageDown => self.chat.scroll_down(10),
                KeyCode::Char('g') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.chat.scroll_up(1)
                }
                KeyCode::Char('g')
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        || key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    self.chat.scroll_to_bottom()
                }
                KeyCode::Char('i') => self.focus = Focus::Input,
                _ => {}
            },

            // 工具面板操作
            _ if self.focus == Focus::Tools => match key.code {
                KeyCode::Char('e') | KeyCode::Char(' ') => self.tools.toggle_expand(),
                KeyCode::Char('i') => self.focus = Focus::Input,
                _ => {}
            },

            _ => {}
        }
    }

    /// 处理 Agent 事件
    fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Token(token) => {
                self.chat.ensure_assistant_msg();
                self.chat.append_token(&token);
            }
            AgentEvent::ToolCall { name, args } => {
                let args_str = serde_json::to_string(&args).unwrap_or_default();
                self.tools.add_tool_call(&name, &args_str);
                self.status.help_text = format!("调用工具: {}", name);
            }
            AgentEvent::ToolResult { name, output } => {
                self.tools.set_tool_result(&name, &output);
                self.status.help_text.clear();
            }
            AgentEvent::ToolError { name, error } => {
                self.tools.set_tool_error(&name, &error);
                self.status.help_text.clear();
            }
            AgentEvent::ThinkEnd {
                prompt_tokens,
                completion_tokens,
            } => {
                self.context.update_tokens(prompt_tokens, completion_tokens);
                self.status.prompt_tokens = self.status.prompt_tokens.saturating_add(prompt_tokens);
                self.status.completion_tokens = self
                    .status
                    .completion_tokens
                    .saturating_add(completion_tokens);
            }
            AgentEvent::Error { source, message } => {
                self.chat.ensure_assistant_msg();
                self.chat
                    .append_token(&format!("\n[{}] {}", source, message));
                self.chat.finish_streaming();
                self.streaming = false;
                self.event_rx = None;
                self.cancel_token = None;
                self.status.status = ConnectionStatus::Idle;
                self.status.help_text = String::from("生成失败");
            }
            AgentEvent::FinalAnswer(_) => {
                self.chat.finish_streaming();
                self.streaming = false;
                self.event_rx = None;
                self.status.status = ConnectionStatus::Idle;
                self.status.help_text = String::new();
                self.status.message_count = self.chat.message_count();
            }
            AgentEvent::Cancelled if self.streaming => {
                self.chat.finish_streaming();
                self.streaming = false;
                self.event_rx = None;
                self.cancel_token = None;
                self.status.status = ConnectionStatus::Idle;
                self.status.help_text = String::from("已取消");
            }
            _ => {}
        }
    }

    /// 提交用户消息并启动 Agent 流
    fn submit_message(&mut self) {
        let message = self.input.take();
        self.chat.add_message(MessageRole::User, message.clone());

        // 准备状态
        self.streaming = true;
        self.status.status = ConnectionStatus::Streaming;
        self.status.help_text = String::from("生成中... (Esc 取消)");
        self.tools.clear();

        // 创建通道和取消令牌
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        self.event_rx = Some(rx);
        self.cancel_token = Some(cancel.clone());

        // 使用 inner() 逃生舱口：chat_stream_with_cancel 返回的流同时借用
        // agent 和 message，其生命周期无法用 write_async 的 HRTB 表达。
        let agent = self.agent.inner().clone();

        tokio::spawn(async move {
            let agent = agent.read().await;
            match agent.chat_stream_with_cancel(&message, cancel).await {
                Ok(mut stream) => {
                    while let Some(result) = stream.next().await {
                        match result {
                            Ok(event) => {
                                if tx.send(event).is_err() {
                                    break; // 接收端已断开
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(AgentEvent::Error {
                                    source: "llm_stream".into(),
                                    message: e.to_string(),
                                });
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(AgentEvent::Error {
                        source: "agent".into(),
                        message: e.to_string(),
                    });
                }
            }
        });
    }

    /// 切换焦点
    fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Input => Focus::Chat,
            Focus::Chat => Focus::Tools,
            Focus::Tools => Focus::Context,
            Focus::Context => Focus::Input,
        };
    }

    fn reverse_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Input => Focus::Context,
            Focus::Context => Focus::Tools,
            Focus::Tools => Focus::Chat,
            Focus::Chat => Focus::Input,
        };
    }

    /// 渲染完整 UI
    fn render_ui(&mut self, f: &mut Frame) {
        let total_area = f.area();

        // 状态栏（底部固定 1 行）
        let status_height = 1;
        let main_area = Rect {
            height: total_area.height.saturating_sub(status_height as u16),
            ..total_area
        };
        let status_area = Rect {
            y: main_area.height,
            height: status_height as u16,
            ..total_area
        };

        // 主区域分割：输入区（3行） + 上下分割
        let input_height = 3;
        let upper_area = Rect {
            height: main_area.height.saturating_sub(input_height),
            ..main_area
        };
        let input_area = Rect {
            y: upper_area.height,
            height: input_height,
            ..main_area
        };

        // 上部分割：对话区 70% | 侧边栏 30%
        let upper_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
            .split(upper_area);

        let chat_area = upper_chunks[0];

        // 侧边栏分割：工具 50% | 上下文 50%
        let side_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(upper_chunks[1]);

        let tools_area = side_chunks[0];
        let context_area = side_chunks[1];

        // 渲染各面板
        self.chat
            .render(f, chat_area, &self.colors, self.focus == Focus::Chat);
        self.tools
            .render(f, tools_area, &self.colors, self.focus == Focus::Tools);
        self.context
            .render(f, context_area, &self.colors, self.focus == Focus::Context);
        self.input
            .render(f, input_area, &self.colors, self.focus == Focus::Input);
        self.status.render(f, status_area, &self.colors);

        // 焦点指示器
        if !self.streaming {
            let focus_text = match self.focus {
                Focus::Input => " [输入] Tab 切换面板 | Ctrl+C 退出 ",
                Focus::Chat => " [对话] ↑↓/jk 滚动 | i 输入 | Tab 切换 ",
                Focus::Tools => " [工具] e 展开/折叠 | Tab 切换 ",
                Focus::Context => " [上下文] Tab 切换 ",
            };
            let hint_area = Rect {
                x: total_area.x,
                y: total_area.y + total_area.height.saturating_sub(1),
                width: total_area.width,
                height: 1,
            };
            let hint = Paragraph::new(Line::from(Span::styled(
                focus_text,
                ratatui::style::Style::default().fg(self.colors.muted),
            )));
            f.render_widget(hint, hint_area);
        }
    }
}

/// 运行 TUI 模式
pub async fn run_tui(agent: AgentHandle) -> anyhow::Result<()> {
    // 安装 panic hook 以恢复终端
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(info);
    }));

    // 初始化终端
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // 创建并运行应用
    let mut app = TuiApp::new(agent).await;
    let result = app.run(&mut terminal).await;

    // 恢复终端
    terminal.clear()?;
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;

    // 恢复 panic hook
    let _ = std::panic::take_hook();

    result
}

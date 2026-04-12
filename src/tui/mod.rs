mod chat;
mod command;
mod events;
mod input;
mod sidebar;
pub mod theme;

use std::io;
use std::time::{Duration, Instant};

use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    Frame, Terminal,
    layout::{Constraint, Direction, Layout},
};
use tokio::{sync::mpsc, task::JoinHandle};
use tui_textarea::TextArea;

use chat::SpinnerState;

/// Sentinel string sent on the user-input channel to signal cancellation.
pub const CANCEL_SENTINEL: &str = "__CANCEL__";

/// Status of a tool execution displayed in chat.
#[derive(Debug, Clone)]
pub enum ToolStatus {
    Running(Instant),
    Done(Duration),
    Failed(String),
}

/// A single message in the chat area.
#[derive(Debug, Clone)]
pub enum ChatMessage {
    User {
        text: String,
    },
    Assistant {
        text: String,
    },
    ToolCall {
        name: String,
        args: String,
        status: ToolStatus,
    },
    ToolResult {
        name: String,
        output: String,
    },
    System {
        text: String,
    },
}

/// Info about the current agent displayed in the sidebar.
#[derive(Debug, Clone, Default)]
pub struct AgentInfo {
    pub provider: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: Option<f64>,
}

/// Status of a connected channel.
#[derive(Debug, Clone)]
pub struct ChannelStatus {
    pub name: String,
    pub connected: bool,
}

/// A recent memory operation.
#[derive(Debug, Clone)]
pub struct MemoryOp {
    pub operation: String,
    pub summary: String,
    pub timestamp: Instant,
}

/// Status of a hardware peripheral.
#[derive(Debug, Clone)]
pub struct PeripheralStatus {
    pub name: String,
    pub state: String,
}

/// An active tool execution (shown in sidebar).
#[derive(Debug, Clone)]
pub struct ActiveTool {
    pub name: String,
    pub args: String,
    pub started: Instant,
}

/// Item in the command palette.
#[derive(Debug, Clone)]
pub struct PaletteItem {
    pub name: String,
    pub description: String,
}

#[allow(clippy::struct_excessive_bools)] // independent UI toggle flags
pub struct App {
    // Chat
    pub(crate) messages: Vec<ChatMessage>,
    pub(crate) scroll_offset: u16,
    pub(crate) auto_scroll: bool,

    // Streaming state
    pub(crate) pending_chunk: String,
    pub(crate) active_tools: Vec<ActiveTool>,

    // Sidebar
    pub(crate) sidebar_visible: bool,
    pub(crate) agent_info: AgentInfo,
    pub(crate) channel_status: Vec<ChannelStatus>,
    pub(crate) memory_activity: Vec<MemoryOp>,
    pub(crate) peripheral_status: Vec<PeripheralStatus>,

    // Input
    pub(crate) textarea: TextArea<'static>,

    // Command palette
    pub(crate) palette_open: bool,
    pub(crate) palette_query: String,
    pub(crate) palette_items: Vec<PaletteItem>,

    // Spinner (agent thinking indicator)
    pub(crate) spinner: Option<SpinnerState>,

    // Control
    pub(crate) should_quit: bool,
    pub(crate) tick: usize,
}

impl App {
    fn new() -> Self {
        Self {
            messages: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
            pending_chunk: String::new(),
            active_tools: Vec::new(),
            sidebar_visible: false,
            agent_info: AgentInfo::default(),
            channel_status: Vec::new(),
            memory_activity: Vec::new(),
            peripheral_status: Vec::new(),
            textarea: input::create_textarea(),
            palette_open: false,
            palette_query: String::new(),
            palette_items: Vec::new(),
            spinner: None,
            should_quit: false,
            tick: 0,
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let outer_chunks = if self.sidebar_visible {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(40), Constraint::Length(30)])
                .split(frame.area())
        } else {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(40)])
                .split(frame.area())
        };

        // Main area (chat + spinner + input)
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(self.layout_constraints())
            .split(outer_chunks[0]);

        // Chat area
        chat::render_chat_area(frame, main_chunks[0], &self.messages, self.scroll_offset);

        // Spinner line (only when agent is processing)
        if let Some(ref state) = self.spinner {
            chat::render_spinner_line(frame, main_chunks[1], state, self.tick);
        }

        // Input box
        let input_idx = if self.spinner.is_some() { 2 } else { 1 };
        input::render_input(frame, main_chunks[input_idx], &self.textarea);

        // Sidebar (when visible)
        if self.sidebar_visible {
            sidebar::render_sidebar(
                frame,
                outer_chunks[1],
                &self.agent_info,
                &self.active_tools,
                &self.channel_status,
                &self.memory_activity,
                &self.peripheral_status,
            );
        }

        // Command palette overlay
        if self.palette_open {
            command::render_command_palette(
                frame,
                frame.area(),
                &self.palette_query,
                &self.palette_items,
            );
        }
    }

    fn layout_constraints(&self) -> Vec<Constraint> {
        if self.spinner.is_some() {
            vec![
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(3),
            ]
        } else {
            vec![Constraint::Min(3), Constraint::Length(3)]
        }
    }

    fn handle_submit(&mut self, tx: &mpsc::Sender<String>) {
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let text = lines.join("\n").trim().to_string();
        self.textarea.select_all();
        self.textarea.cut();

        if text.is_empty() {
            return;
        }

        match text.as_str() {
            "/quit" => {
                self.should_quit = true;
            }
            "/clear" => {
                self.messages.clear();
                self.scroll_offset = 0;
            }
            "/help" => {
                self.push_system("Commands:".into());
                self.push_system("  /quit   - Exit the TUI".into());
                self.push_system("  /clear  - Clear output".into());
                self.push_system("  /help   - Show this help".into());
                self.push_system("  ESC     - Cancel in-progress request".into());
                self.push_system("  Shift+Enter - Insert newline".into());
                self.push_system("  Ctrl+B  - Toggle sidebar".into());
                self.push_system("  Ctrl+P  - Toggle command palette".into());
            }
            _ => {
                self.messages.push(ChatMessage::User { text: text.clone() });
                if self.auto_scroll {
                    self.scroll_offset = u16::MAX;
                }
                if tx.try_send(text).is_ok() {
                    self.spinner = Some(SpinnerState::new("pondering"));
                } else {
                    self.push_system("[send failed \u{2014} channel full]".into());
                }
            }
        }
    }

    fn push_system(&mut self, text: String) {
        self.messages.push(ChatMessage::System { text });
        if self.auto_scroll {
            self.scroll_offset = u16::MAX;
        }
    }

    fn push_assistant(&mut self, text: String) {
        self.messages.push(ChatMessage::Assistant { text });
        if self.auto_scroll {
            self.scroll_offset = u16::MAX;
        }
    }
}

/// Spawn the TUI event loop on a tokio blocking task.
///
/// - `tx`: channel for sending user input (and cancel signals) to the agent.
/// - `rx`: channel for receiving agent output lines.
///
/// Returns a `JoinHandle` that resolves when the TUI exits.
pub fn spawn_tui(tx: mpsc::Sender<String>, mut rx: mpsc::Receiver<String>) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        if let Err(e) = run_tui(tx, &mut rx) {
            eprintln!("TUI error: {e}");
        }
    })
}

/// Guard that restores terminal state on drop (including panics).
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
    }
}

fn run_tui(tx: mpsc::Sender<String>, rx: &mut mpsc::Receiver<String>) -> io::Result<()> {
    terminal::enable_raw_mode()?;
    let _guard = TerminalGuard; // ensures raw mode + alt screen are restored even on panic
    io::stdout().execute(EnterAlternateScreen)?;

    let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();

    loop {
        terminal.draw(|f| app.draw(f))?;

        // Poll for crossterm events with a 100ms timeout (tick rate)
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if handle_key_event(&mut app, &tx, key) {
                    break;
                }
            }
        }

        // Drain incoming agent output (non-blocking)
        while let Ok(line) = rx.try_recv() {
            app.spinner = None;
            app.push_assistant(line);
        }

        app.tick = app.tick.wrapping_add(1);

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

/// Returns `true` if the app should quit immediately.
fn handle_key_event(app: &mut App, tx: &mpsc::Sender<String>, key: KeyEvent) -> bool {
    // Command palette intercepts keys when open
    if app.palette_open {
        match key.code {
            KeyCode::Esc => {
                app.palette_open = false;
                app.palette_query.clear();
            }
            KeyCode::Char(c) => {
                app.palette_query.push(c);
            }
            KeyCode::Backspace => {
                app.palette_query.pop();
            }
            KeyCode::Enter => {
                // Execute selected palette item (stub: just close)
                app.palette_open = false;
                app.palette_query.clear();
            }
            _ => {}
        }
        return false;
    }

    match key.code {
        // Ctrl+B: toggle sidebar
        KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.sidebar_visible = !app.sidebar_visible;
            false
        }
        // Ctrl+P: toggle command palette
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.palette_open = !app.palette_open;
            app.palette_query.clear();
            false
        }
        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.handle_submit(tx);
            false
        }
        KeyCode::Esc => {
            if app.spinner.is_some() {
                app.spinner = None;
                if tx.try_send(CANCEL_SENTINEL.to_string()).is_ok() {
                    app.push_system("[cancelled]".into());
                } else {
                    app.push_system("[cancel failed \u{2014} channel full]".into());
                }
            }
            false
        }
        KeyCode::PageUp => {
            app.auto_scroll = false;
            app.scroll_offset = app.scroll_offset.saturating_sub(10);
            false
        }
        KeyCode::PageDown => {
            app.scroll_offset = app.scroll_offset.saturating_add(10);
            // Re-enable auto-scroll if scrolled past content
            let total = u16::try_from(app.messages.len()).unwrap_or(u16::MAX);
            if app.scroll_offset >= total {
                app.scroll_offset = u16::MAX;
                app.auto_scroll = true;
            }
            false
        }
        _ => {
            // Let tui-textarea handle the key event
            app.textarea.input(key);
            false
        }
    }
}

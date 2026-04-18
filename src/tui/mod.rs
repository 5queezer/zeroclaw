pub mod banner;
mod chat;
mod command;
mod events;
mod input;
mod sidebar;
mod statusbar;
pub mod theme;

use std::io;
use std::sync::Arc;
use std::time::Instant;

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

// `ChatMessage` and `ToolStatus` live under `crate::session` so non-TUI code
// paths (CLI `--list-sessions`, tests) can use them without enabling the `tui`
// feature. Re-exported here so existing `crate::tui::ChatMessage` usages keep
// compiling.
pub use crate::session::{ChatMessage, ToolStatus};

/// Sentinel string sent on the user-input channel to signal cancellation.
pub const CANCEL_SENTINEL: &str = "__CANCEL__";

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
    pub(crate) palette_selected: usize,

    // Spinner (agent thinking indicator)
    pub(crate) spinner: Option<SpinnerState>,

    // Session persistence
    pub(crate) session: Option<SessionHandle>,
    pub(crate) persist_retry_count: u8,

    // Status bar
    pub(crate) git_status: statusbar::GitStatus,
    pub(crate) permission_mode: Option<String>,
    pub(crate) context_window: Option<u32>,

    // Control
    pub(crate) should_quit: bool,
    pub(crate) tick: usize,
}

impl App {
    pub(crate) fn new() -> Self {
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
            palette_items: vec![
                PaletteItem {
                    name: "/quit".into(),
                    description: "Exit the TUI".into(),
                },
                PaletteItem {
                    name: "/clear".into(),
                    description: "Clear chat history".into(),
                },
                PaletteItem {
                    name: "/help".into(),
                    description: "Show help".into(),
                },
            ],
            palette_selected: 0,
            spinner: None,
            session: None,
            persist_retry_count: 0,
            git_status: statusbar::GitStatus::new(),
            permission_mode: None,
            context_window: None,
            should_quit: false,
            tick: 0,
        }
    }

    /// Start a new App bound to an existing session (messages start empty).
    pub(crate) fn with_session(session: SessionHandle) -> Self {
        let mut app = Self::new();
        app.session = Some(session);
        app
    }

    /// Start an App with a session and pre-populated messages (resume flow).
    pub(crate) fn with_resumed(session: SessionHandle, messages: Vec<ChatMessage>) -> Self {
        let mut app = Self::with_session(session);
        app.messages = messages;
        app
    }

    /// Persist a message to the attached session, if any.
    ///
    /// - On first failure, surfaces a `[persistence error: ...]` system message
    ///   in the UI (via `push_system_nopersist` to avoid recursion).
    /// - On subsequent failures, logs via `tracing::warn!` and suppresses
    ///   further UI noise until a successful append resets the counter.
    pub(crate) fn persist(&mut self, msg: &ChatMessage) {
        let Some(handle) = self.session.as_ref() else {
            return;
        };
        match handle.append(msg) {
            Ok(()) => self.persist_retry_count = 0,
            Err(e) if self.persist_retry_count == 0 => {
                self.persist_retry_count = 1;
                self.push_system_nopersist(format!("[persistence error: {e}]"));
            }
            Err(e) => {
                tracing::warn!(error = %e, "persistence error suppressed after repeated failure");
                self.persist_retry_count = 2;
            }
        }
    }

    /// Push a system message without attempting to persist it.
    ///
    /// Used by `persist()` itself to avoid recursion when reporting a
    /// persistence failure would try to re-persist the error message.
    fn push_system_nopersist(&mut self, text: String) {
        self.messages.push(ChatMessage::System { text });
        if self.auto_scroll {
            self.scroll_offset = u16::MAX;
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let outer_chunks = if self.sidebar_visible {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(1), Constraint::Length(40)])
                .split(frame.area())
        } else {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(1)])
                .split(frame.area())
        };

        // Main area (chat + spinner + input)
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(self.layout_constraints())
            .split(outer_chunks[0]);

        // Chat area
        chat::render_chat_area(
            frame,
            main_chunks[0],
            &self.messages,
            &self.pending_chunk,
            self.scroll_offset,
        );

        // Spinner line (only when agent is processing)
        if let Some(ref state) = self.spinner {
            chat::render_spinner_line(frame, main_chunks[1], state, self.tick);
        }

        // Input box
        let input_idx = main_chunks.len() - 2;
        input::render_input(frame, main_chunks[input_idx], &self.textarea);

        // Status bar (bottom line)
        let status_idx = main_chunks.len() - 1;
        let (branch, dirty) = self.git_status.snapshot();
        let pct = self.context_window.filter(|w| *w > 0).map(|w| {
            let used = self.agent_info.input_tokens + self.agent_info.output_tokens;
            let p = used.saturating_mul(100).saturating_div(u64::from(w));
            u8::try_from(p).unwrap_or(99)
        });
        let info = statusbar::StatusInfo {
            model: if self.agent_info.model.is_empty() {
                None
            } else {
                Some(self.agent_info.model.as_str())
            },
            branch,
            dirty,
            context_percent: pct,
            perm_mode: self.permission_mode.as_deref(),
            hint: "/help  Ctrl+P palette",
        };
        statusbar::render_status_bar(frame, main_chunks[status_idx], &info);

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
                self.tick,
            );
        }

        // Command palette overlay
        if self.palette_open {
            command::render_command_palette(
                frame,
                frame.area(),
                &self.palette_query,
                &self.palette_items,
                self.palette_selected,
            );
        }
    }

    fn layout_constraints(&self) -> Vec<Constraint> {
        if self.spinner.is_some() {
            vec![
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Length(1),
            ]
        } else {
            vec![
                Constraint::Min(3),
                Constraint::Length(3),
                Constraint::Length(1),
            ]
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

        if text == "/quit" {
            self.should_quit = true;
        } else if text == "/clear" {
            self.messages.clear();
            self.scroll_offset = 0;
        } else if text == "/help" {
            self.push_system("Commands:".into());
            self.push_system("  /quit   - Exit the TUI".into());
            self.push_system("  /clear  - Clear output".into());
            self.push_system("  /title <text> - Set explicit session title".into());
            self.push_system("  /help   - Show this help".into());
            self.push_system("  ESC     - Cancel in-progress request".into());
            self.push_system("  Shift+Enter - Insert newline".into());
            self.push_system("  Ctrl+B  - Toggle sidebar".into());
            self.push_system("  Ctrl+P  - Toggle command palette".into());
        } else if let Some(rest) = text.strip_prefix("/title ") {
            let title = rest.trim();
            if title.is_empty() {
                self.push_system("[usage: /title <new title>]".into());
            } else if let Some(h) = self.session.as_ref() {
                match h.set_title(title, true) {
                    Ok(()) => self.push_system(format!("[title set: {title}]")),
                    Err(e) => self.push_system(format!("[title set failed: {e}]")),
                }
            } else {
                self.push_system("[no session active]".into());
            }
        } else {
            let persist_text = text.clone();
            match tx.try_send(text.clone()) {
                Ok(()) => {
                    self.messages.push(ChatMessage::User { text });
                    if self.auto_scroll {
                        self.scroll_offset = u16::MAX;
                    }
                    self.spinner = Some(SpinnerState::new("pondering"));
                    self.persist(&ChatMessage::User { text: persist_text });
                }
                Err(_) => {
                    self.textarea.insert_str(&text);
                    self.push_system("[send failed \u{2014} channel full]".into());
                }
            }
        }
    }

    pub(crate) fn maybe_set_first_message_title(&mut self) {
        let Some(handle) = self.session.as_ref() else {
            return;
        };

        // Only fire on the very first completed turn.
        let assistant_count = self
            .messages
            .iter()
            .filter(|m| matches!(m, ChatMessage::Assistant { .. }))
            .count();
        if assistant_count != 1 {
            return;
        }

        let Some(ChatMessage::User { text }) = self
            .messages
            .iter()
            .find(|m| matches!(m, ChatMessage::User { .. }))
        else {
            return;
        };

        let title = derive_title_from(text);
        if title.is_empty() {
            return;
        }
        if let Err(e) = handle.set_title(&title, false) {
            tracing::warn!(error = %e, "first-message title fallback failed");
        }
    }

    fn push_system(&mut self, text: String) {
        self.messages.push(ChatMessage::System { text });
        if self.auto_scroll {
            self.scroll_offset = u16::MAX;
        }
    }

    fn push_assistant(&mut self, text: String) {
        let persist_text = text.clone();
        self.messages.push(ChatMessage::Assistant { text });
        if self.auto_scroll {
            self.scroll_offset = u16::MAX;
        }
        self.persist(&ChatMessage::Assistant { text: persist_text });
    }
}

/// Spawn the TUI event loop on a tokio blocking task.
///
/// - `tx`: channel for sending user input (and cancel signals) to the agent.
/// - `rx`: channel for receiving `TurnEvent`s from the agent.
/// - `session`: optional `SessionHandle` to persist messages to.
///
/// Returns a `JoinHandle` that resolves when the TUI exits.
pub fn spawn_tui(
    tx: mpsc::Sender<String>,
    rx: mpsc::Receiver<crate::agent::TurnEvent>,
    session: Option<SessionHandle>,
) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut app = match session {
            Some(h) => App::with_session(h),
            None => App::new(),
        };
        if let Err(e) = run_tui_with_app(tx, rx, &mut app) {
            eprintln!("TUI error: {e}");
        }
    })
}

/// Spawn the TUI event loop with a pre-populated message history.
///
/// Used by the resume boot path so the user sees prior conversation
/// on startup.
pub fn spawn_tui_resumed(
    tx: mpsc::Sender<String>,
    rx: mpsc::Receiver<crate::agent::TurnEvent>,
    session: SessionHandle,
    messages: Vec<ChatMessage>,
) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut app = App::with_resumed(session, messages);
        if let Err(e) = run_tui_with_app(tx, rx, &mut app) {
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

fn run_tui_with_app(
    tx: mpsc::Sender<String>,
    mut rx: mpsc::Receiver<crate::agent::TurnEvent>,
    app: &mut App,
) -> io::Result<()> {
    terminal::enable_raw_mode()?;
    let _guard = TerminalGuard; // ensures raw mode + alt screen are restored even on panic
    io::stdout().execute(EnterAlternateScreen)?;

    let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|f| app.draw(f))?;

        // Poll for crossterm events with a 100ms timeout (tick rate)
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if handle_key_event(app, &tx, key) {
                    break;
                }
            }
        }

        // Drain incoming agent events (non-blocking).
        while let Ok(event) = rx.try_recv() {
            crate::tui::events::handle_turn_event(app, event);
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
                app.palette_selected = 0;
            }
            KeyCode::Char(c) => {
                app.palette_query.push(c);
                app.palette_selected = 0;
            }
            KeyCode::Backspace => {
                app.palette_query.pop();
                app.palette_selected = 0;
            }
            KeyCode::Up => {
                app.palette_selected = app.palette_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                let filtered_len =
                    command::filter_items(&app.palette_query, &app.palette_items).len();
                if filtered_len > 0 {
                    app.palette_selected =
                        (app.palette_selected + 1).min(filtered_len.saturating_sub(1));
                }
            }
            KeyCode::Enter => {
                let filtered = command::filter_items(&app.palette_query, &app.palette_items);
                if let Some(item) = filtered.get(app.palette_selected) {
                    let name = item.name.clone();
                    app.palette_open = false;
                    app.palette_query.clear();
                    app.palette_selected = 0;
                    // Feed slash commands through handle_submit
                    if name.starts_with('/') {
                        app.textarea.select_all();
                        app.textarea.cut();
                        app.textarea.insert_str(&name);
                        app.handle_submit(tx);
                    }
                } else {
                    app.palette_open = false;
                    app.palette_query.clear();
                    app.palette_selected = 0;
                }
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

/// Derive a short session title from the first user message.
/// Returns the first line (trimmed, max 60 visible chars + "…" if truncated).
pub(crate) fn derive_title_from(first_user_msg: &str) -> String {
    let first_line = first_user_msg.lines().next().unwrap_or("").trim();
    let count = first_line.chars().count();
    if count <= 60 {
        return first_line.to_string();
    }
    let mut out: String = first_line.chars().take(60).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod title_tests {
    use super::*;

    #[test]
    fn derive_title_truncates_and_ellipsis() {
        let input = "this is a fairly long first line that should get truncated by the fallback helper because it exceeds sixty characters";
        let t = derive_title_from(input);
        // 60 visible chars + "…" (3 UTF-8 bytes).
        let visible: usize = t.chars().count();
        assert!(visible == 61, "visible chars = {visible}, title = {t:?}");
        assert!(t.ends_with('…'));
    }

    #[test]
    fn derive_title_short_unchanged() {
        assert_eq!(derive_title_from("hello"), "hello");
    }

    #[test]
    fn derive_title_trims_surrounding_whitespace() {
        assert_eq!(derive_title_from("   hello   "), "hello");
    }

    #[test]
    fn derive_title_first_line_only() {
        assert_eq!(derive_title_from("first line\nsecond line"), "first line");
    }

    #[test]
    fn derive_title_empty_string() {
        assert_eq!(derive_title_from(""), "");
    }
}

#[cfg(test)]
mod title_cmd_tests {
    use super::*;
    use crate::session::SessionStore;
    use std::path::Path;
    use std::sync::Arc;

    #[test]
    fn title_slash_sets_explicit_title() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(&dir.path().join("s.db")).unwrap());
        let meta = store.create(Path::new("/tmp"), None, None, None).unwrap();
        let h = SessionHandle::new(Arc::clone(&store), meta.id.clone());
        let mut app = App::with_session(h);
        app.textarea.insert_str("/title My New Title");
        let (tx, _rx) = tokio::sync::mpsc::channel::<String>(1);
        app.handle_submit(&tx);
        let loaded = store.load(&meta.id).unwrap();
        assert_eq!(loaded.meta.title.as_deref(), Some("My New Title"));
        assert!(loaded.meta.title_explicit);
    }
}

/// Thin wrapper around the `SessionStore` + current session ID.
///
/// Cheap to clone; multiple clones share the same `Arc`'d store. All methods
/// forward to the inner store using the bound session ID.
///
/// Thread safety: `SessionStore` wraps a `rusqlite::Connection`, which is
/// `Send` but not `Sync`. The TUI runs on a single `spawn_blocking` thread and
/// all `append`/`set_title`/etc. calls happen there, so the `!Sync` property
/// is not exercised — we do not share `&SessionStore` across threads.
#[derive(Clone)]
pub struct SessionHandle {
    store: Arc<crate::session::SessionStore>,
    id: crate::session::SessionId,
}

impl SessionHandle {
    pub fn new(store: Arc<crate::session::SessionStore>, id: crate::session::SessionId) -> Self {
        Self { store, id }
    }

    pub fn id(&self) -> &crate::session::SessionId {
        &self.id
    }

    pub fn store(&self) -> &Arc<crate::session::SessionStore> {
        &self.store
    }

    pub fn append(&self, msg: &ChatMessage) -> anyhow::Result<()> {
        self.store.append(&self.id, msg)
    }

    pub fn set_title(&self, title: &str, explicit: bool) -> anyhow::Result<()> {
        self.store.set_title(&self.id, title, explicit)
    }

    pub fn add_duration(&self, d: std::time::Duration) -> anyhow::Result<()> {
        self.store.add_duration(&self.id, d)
    }
}

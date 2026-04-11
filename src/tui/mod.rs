pub mod theme;
pub mod widgets;

use std::io;

use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    Frame, Terminal,
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders},
};
use tokio::{sync::mpsc, task::JoinHandle};
use tui_textarea::TextArea;

use widgets::SpinnerState;

/// Sentinel string sent on the user-input channel to signal cancellation.
pub const CANCEL_SENTINEL: &str = "__CANCEL__";

pub struct App {
    output: Vec<String>,
    scroll_offset: u16,
    auto_scroll: bool,
    spinner: Option<SpinnerState>,
    textarea: TextArea<'static>,
    should_quit: bool,
    tick: usize,
}

impl App {
    fn new() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border()),
        );
        textarea.set_style(theme::style());
        textarea.set_cursor_line_style(theme::style());
        textarea.set_placeholder_text("> ");
        textarea.set_placeholder_style(theme::dim());

        Self {
            output: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
            spinner: None,
            textarea,
            should_quit: false,
            tick: 0,
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(self.layout_constraints())
            .split(frame.area());

        // Output pane
        widgets::render_output_pane(frame, chunks[0], &self.output, self.scroll_offset);

        // Spinner line (only when agent is processing)
        if let Some(ref state) = self.spinner {
            widgets::render_spinner_line(frame, chunks[1], state, self.tick);
        }

        // Input box
        let input_idx = if self.spinner.is_some() { 2 } else { 1 };
        frame.render_widget(&self.textarea, chunks[input_idx]);
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
                self.output.clear();
                self.scroll_offset = 0;
            }
            "/help" => {
                self.push_output("Commands:".into());
                self.push_output("  /quit   - Exit the TUI".into());
                self.push_output("  /clear  - Clear output".into());
                self.push_output("  /help   - Show this help".into());
                self.push_output("  ESC     - Cancel in-progress request".into());
                self.push_output("  Shift+Enter - Insert newline".into());
            }
            _ => {
                self.push_output(format!("> {text}"));
                self.spinner = Some(SpinnerState::new("pondering"));
                // Non-blocking send; if the channel is full we drop the message
                let _ = tx.try_send(text);
            }
        }
    }

    fn push_output(&mut self, line: String) {
        self.output.push(line);
        if self.auto_scroll {
            self.scroll_to_bottom();
        }
    }

    fn scroll_to_bottom(&mut self) {
        let total = u16::try_from(self.output.len()).unwrap_or(u16::MAX);
        self.scroll_offset = total.saturating_sub(1);
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
    io::stdout().execute(EnterAlternateScreen)?;
    let _guard = TerminalGuard;

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
            app.push_output(line);
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
    match key.code {
        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.handle_submit(tx);
            false
        }
        KeyCode::Esc => {
            if app.spinner.is_some() {
                app.spinner = None;
                let _ = tx.try_send(CANCEL_SENTINEL.to_string());
                app.push_output("[cancelled]".into());
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
            // Re-enable auto-scroll if we're near the bottom
            let total = u16::try_from(app.output.len()).unwrap_or(u16::MAX);
            if app.scroll_offset >= total.saturating_sub(1) {
                app.scroll_offset = total.saturating_sub(1);
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

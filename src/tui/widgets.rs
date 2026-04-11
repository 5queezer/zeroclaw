use std::time::Instant;

use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::theme;

const SPINNER_FRAMES: &[&str] = &["\u{25DC}", "\u{25DD}", "\u{25DE}", "\u{25DF}"];

pub(crate) struct Spinner {
    frames: &'static [&'static str],
}

impl Spinner {
    pub(crate) const fn new() -> Self {
        Self {
            frames: SPINNER_FRAMES,
        }
    }

    pub(crate) fn frame(&self, tick: usize) -> &'static str {
        self.frames[tick % self.frames.len()]
    }
}

pub(crate) struct SpinnerState {
    start: Instant,
    pub(crate) label: String,
}

impl SpinnerState {
    pub(crate) fn new(label: impl Into<String>) -> Self {
        Self {
            start: Instant::now(),
            label: label.into(),
        }
    }

    pub(crate) fn elapsed_secs(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }
}

pub(crate) fn render_output_pane(
    frame: &mut Frame,
    area: Rect,
    output: &[String],
    scroll_offset: u16,
) {
    let block = Block::default()
        .title(" Hrafn ")
        .title_style(theme::bold())
        .borders(Borders::ALL)
        .border_style(theme::border());

    let lines: Vec<Line> = output
        .iter()
        .map(|s| Line::from(s.as_str()).style(theme::style()))
        .collect();

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset, 0))
        .style(theme::style());

    frame.render_widget(paragraph, area);
}

pub(crate) fn render_spinner_line(
    frame: &mut Frame,
    area: Rect,
    state: &SpinnerState,
    tick: usize,
) {
    let spinner = Spinner::new();
    let elapsed = state.elapsed_secs();
    let text = format!(
        "{} {}... ({:.1}s)",
        spinner.frame(tick),
        state.label,
        elapsed
    );

    let line = Line::from(vec![Span::styled(text, theme::dim())]);
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

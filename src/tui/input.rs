use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Borders},
};
use tui_textarea::TextArea;

use super::theme;

/// Create a new textarea widget with the standard Hrafn styling.
pub(crate) fn create_textarea() -> TextArea<'static> {
    let mut textarea = TextArea::default();
    textarea.set_block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border()),
    );
    textarea.set_style(theme::style());
    textarea.set_cursor_line_style(theme::style());
    textarea.set_placeholder_text("\u{276F} ");
    textarea.set_placeholder_style(theme::dim());
    textarea
}

/// Render the input textarea into the given area.
pub(crate) fn render_input(frame: &mut Frame, area: Rect, textarea: &TextArea<'_>) {
    frame.render_widget(textarea, area);
}

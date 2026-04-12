use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::PaletteItem;
use super::theme;

/// Render the command palette as a centered overlay.
pub(crate) fn render_command_palette(
    frame: &mut Frame,
    area: Rect,
    query: &str,
    items: &[PaletteItem],
) {
    // Center a 60x16 box (or smaller if the terminal is small)
    let width = area.width.min(60);
    let height = area.height.min(16);

    let [popup_area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    let [popup_area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(popup_area);

    // Clear the area behind the popup
    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(" Command Palette ")
        .title_style(theme::bold())
        .borders(Borders::ALL)
        .border_style(theme::border());

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(format!("> {query}_")).style(theme::style()));
    lines.push(Line::from(""));

    let filtered = filter_items(query, items);
    for item in filtered.iter().take(height.saturating_sub(4) as usize) {
        lines.push(
            Line::from(format!("  {} — {}", item.name, item.description)).style(theme::dim()),
        );
    }

    if filtered.is_empty() {
        lines.push(Line::from("  (no matches)").style(theme::dim()));
    }

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, popup_area);
}

/// Filter palette items by a simple case-insensitive substring match on name.
pub(crate) fn filter_items<'a>(query: &str, items: &'a [PaletteItem]) -> Vec<&'a PaletteItem> {
    if query.is_empty() {
        return items.iter().collect();
    }
    let q = query.to_lowercase();
    items
        .iter()
        .filter(|item| item.name.to_lowercase().contains(&q))
        .collect()
}

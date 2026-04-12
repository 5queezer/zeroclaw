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
    selected: usize,
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
        .border_style(theme::dim());

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(format!("> {query}_")).style(theme::style()));
    lines.push(Line::from(""));

    let filtered = filter_items(query, items);
    // Reserve 2 lines for query+blank at top and 1 line for hints at bottom
    let max_visible = height.saturating_sub(5) as usize;
    let start = selected.saturating_sub(max_visible.saturating_sub(1));
    for (i, item) in filtered.iter().enumerate().skip(start).take(max_visible) {
        let style = if i == selected {
            theme::bold()
        } else {
            theme::dim()
        };
        let marker = if i == selected { "> " } else { "  " };
        lines.push(
            Line::from(format!(
                "{marker}{} \u{2014} {}",
                item.name, item.description
            ))
            .style(style),
        );
    }

    if filtered.is_empty() {
        lines.push(Line::from("  (no matches)").style(theme::dim()));
    }

    // Keybinding hints at the bottom
    lines.push(Line::from(""));
    lines.push(
        Line::from("\u{2191}\u{2193} navigate  \u{23CE} select  esc close").style(theme::dim()),
    );

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

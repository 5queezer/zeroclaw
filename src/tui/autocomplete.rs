use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::theme;

#[derive(Debug, Clone, Copy)]
pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
}

pub const BUILTINS: &[SlashCommand] = &[
    SlashCommand {
        name: "/clear",
        description: "Clear the chat scrollback",
    },
    SlashCommand {
        name: "/help",
        description: "Show keybindings and commands",
    },
    SlashCommand {
        name: "/quit",
        description: "Exit the TUI",
    },
    SlashCommand {
        name: "/sessions",
        description: "Open the session picker",
    },
    SlashCommand {
        name: "/title",
        description: "Set an explicit session title",
    },
];

/// Returns true iff `buffer` is in the "command name" phase: starts with '/'
/// and contains no whitespace.
pub fn is_autocomplete_context(buffer: &str) -> bool {
    buffer.starts_with('/') && !buffer.chars().any(char::is_whitespace)
}

/// Case-insensitive prefix match on command name. `prefix` includes the leading '/'.
pub fn filter<'a>(prefix: &str, commands: &'a [SlashCommand]) -> Vec<&'a SlashCommand> {
    if prefix == "/" {
        return commands.iter().collect();
    }
    let lower = prefix.to_lowercase();
    commands
        .iter()
        .filter(|c| c.name.to_lowercase().starts_with(&lower))
        .collect()
}

/// Render the autocomplete popup anchored above the input area.
pub fn render_autocomplete(
    frame: &mut Frame,
    input_area: Rect,
    items: &[&SlashCommand],
    selected: usize,
) {
    if items.is_empty() {
        return;
    }

    // Height: one line per item + 2 for borders, capped at 7 (5 items visible).
    let desired_height = u16::try_from(items.len().min(5) + 2).unwrap_or(7);
    // Width: wide enough for the longest "name — description" plus padding.
    let max_line = items
        .iter()
        .map(|c| c.name.chars().count() + c.description.chars().count() + 4)
        .max()
        .unwrap_or(20);
    let desired_width = u16::try_from(max_line.min(60)).unwrap_or(60);

    let frame_area = frame.area();
    // Place popup just above the input area, left-aligned with it.
    let popup_top = input_area.y.saturating_sub(desired_height);
    let popup_left = input_area.x;
    let popup_width = desired_width.min(frame_area.width.saturating_sub(popup_left));
    let popup_height = desired_height.min(input_area.y.saturating_sub(frame_area.y));
    if popup_width == 0 || popup_height < 3 {
        return;
    }
    let area = Rect {
        x: popup_left,
        y: popup_top,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim())
        .title(" commands ")
        .title_style(theme::dim());

    let visible = items.len().min(usize::from(popup_height.saturating_sub(2)));
    let start = selected.saturating_sub(visible.saturating_sub(1));
    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, c)| {
            let style = if i == selected {
                theme::bold()
            } else {
                theme::dim()
            };
            let marker = if i == selected { "\u{276F} " } else { "  " };
            Line::from(vec![
                Span::styled(format!("{marker}{}  ", c.name), style),
                Span::styled(c.description, theme::dim()),
            ])
        })
        .collect();

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_autocomplete_context_recognizes_command_phase() {
        assert!(is_autocomplete_context("/"));
        assert!(is_autocomplete_context("/q"));
        assert!(is_autocomplete_context("/sessions"));
        assert!(!is_autocomplete_context(""));
        assert!(!is_autocomplete_context("hello"));
        assert!(!is_autocomplete_context("/title foo"));
        assert!(!is_autocomplete_context("/title "));
    }

    #[test]
    fn filter_prefix_match_case_insensitive() {
        let hits = filter("/q", BUILTINS);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "/quit");

        let hits = filter("/SE", BUILTINS);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "/sessions");

        let hits = filter("/nope", BUILTINS);
        assert!(hits.is_empty());
    }

    #[test]
    fn filter_slash_alone_returns_all() {
        let hits = filter("/", BUILTINS);
        assert_eq!(hits.len(), BUILTINS.len());
    }
}

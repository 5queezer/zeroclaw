use ratatui::style::{Color, Modifier, Style};

pub const PRIMARY: Color = Color::Rgb(0, 255, 70);
pub const BG: Color = Color::Reset;
pub const ACCENT: Color = Color::Rgb(100, 200, 255);
pub const WARN: Color = Color::Rgb(255, 200, 50);
pub const ERROR: Color = Color::Rgb(255, 80, 80);

pub const fn style() -> Style {
    Style::new().fg(PRIMARY).bg(BG)
}

pub const fn bold() -> Style {
    style().add_modifier(Modifier::BOLD)
}

pub const fn border() -> Style {
    Style::new().fg(PRIMARY)
}

pub const fn dim() -> Style {
    Style::new().fg(PRIMARY).add_modifier(Modifier::DIM)
}

/// Style for the sidebar section headers.
pub const fn sidebar_heading() -> Style {
    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
}

/// Style for tool call blocks in the chat area.
pub const fn tool_block() -> Style {
    Style::new().fg(ACCENT)
}

/// Style for system/warning messages.
pub const fn system() -> Style {
    Style::new().fg(WARN)
}

/// Style for error messages.
pub const fn error() -> Style {
    Style::new().fg(ERROR)
}

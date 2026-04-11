use ratatui::style::{Color, Modifier, Style};

pub const PRIMARY: Color = Color::Rgb(0, 255, 70);
pub const BG: Color = Color::Reset;

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

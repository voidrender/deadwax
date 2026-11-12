use ratatui::style::{Color, Modifier, Style};

/// The one accent color: amber.
pub const ACCENT: Color = Color::Rgb(0xE8, 0xA5, 0x4B);
/// Borders, rules, and the empty part of gauges.
pub const DIM: Color = Color::DarkGray;
/// Secondary text such as artist and album.
pub const MUTED: Color = Color::Gray;

pub fn accent() -> Style {
    Style::new().fg(ACCENT)
}

pub fn accent_bold() -> Style {
    accent().add_modifier(Modifier::BOLD)
}

pub fn border() -> Style {
    Style::new().fg(DIM)
}

pub fn dim() -> Style {
    Style::new().fg(DIM)
}

pub fn muted() -> Style {
    Style::new().fg(MUTED)
}

/// Highlight for the selected row or active tab.
pub fn selected() -> Style {
    accent().add_modifier(Modifier::REVERSED | Modifier::BOLD)
}

//! The settings popup.

use super::theme;
use super::widgets::{centered, display_width, truncate};
use crate::app::App;
use crate::app::settings::{ROWS, Row, protocol_label};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

const WIDTH: u16 = 52;
/// Borders, padding, the rows, a blank row and the note row.
const HEIGHT: u16 = 2 + 2 + 4 + 1 + 1;

pub fn draw(frame: &mut Frame, area: Rect, app: &App, sel: usize) {
    let rect = centered(area, WIDTH.min(area.width), HEIGHT.min(area.height));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::accent())
        .padding(Padding::new(1, 1, 1, 1))
        .title(Span::styled(" Settings ", theme::accent_bold()))
        .title_bottom(
            Line::from(vec![
                Span::styled(" Enter ", theme::muted()),
                Span::styled("change  ", theme::dim()),
                Span::styled("S ", theme::muted()),
                Span::styled("close ", theme::dim()),
            ])
            .right_aligned(),
        );
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    if inner.is_empty() {
        return;
    }

    let width = usize::from(inner.width);
    let mut lines: Vec<Line> = ROWS
        .iter()
        .enumerate()
        .map(|(i, &row)| row_line(app, row, i == sel, width))
        .collect();
    lines.push(Line::raw(""));
    if app.image_protocol_pending() {
        lines.push(Line::styled(
            " The album art change applies on the next start.",
            theme::muted(),
        ));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn row_line(app: &App, row: Row, selected: bool, width: usize) -> Line<'static> {
    let s = &app.settings;
    let (value, on) = match row {
        Row::Discord => (on_off(s.discord), s.discord),
        // Dimmed with presence, since it has no effect while presence is off.
        Row::DiscordStatus => (s.discord_status.label(), s.discord),
        Row::Notifications => (on_off(s.notifications), s.notifications),
        Row::ImageProtocol => (protocol_label(s.image_protocol), true),
    };
    // The label on the left and the value on the right, with the label
    // cut short first when the popup is narrow.
    let value = format!("{value} ");
    let room = width.saturating_sub(display_width(&value));
    let label = truncate(&format!(" {}", row.label()), room);
    let gap = " ".repeat(room.saturating_sub(display_width(&label)));
    let (label_style, value_style) = if selected {
        (theme::selected(), theme::selected())
    } else if on {
        (ratatui::style::Style::new(), theme::accent())
    } else {
        (ratatui::style::Style::new(), theme::dim())
    };
    Line::from(vec![
        Span::styled(format!("{label}{gap}"), label_style),
        Span::styled(value, value_style),
    ])
}

fn on_off(on: bool) -> &'static str {
    if on { "On" } else { "Off" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Popup;
    use crate::ui::art::ProtocolChoice;
    use crate::ui::testing::buffer_text;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| crate::ui::draw(f, app)).unwrap();
        buffer_text(term.backend().buffer())
    }

    #[test]
    fn shows_each_setting_and_its_value() {
        let mut app = App::logged_in("s");
        app.settings.notifications = false;
        app.popup = Some(Popup::Settings { sel: 0 });
        let text = render(&mut app, 80, 24);
        let line = |label: &str| {
            text.lines()
                .find(|l| l.contains(label))
                .unwrap()
                .to_string()
        };
        assert!(line("Discord presence").contains("On"), "{text}");
        assert!(line("Discord status").contains("Artist"), "{text}");
        assert!(line("Notifications").contains("Off"), "{text}");
        assert!(line("Album art").contains("Auto"), "{text}");
        assert!(!text.contains("next start"), "{text}");
    }

    #[test]
    fn a_changed_album_art_setting_says_it_needs_a_restart() {
        let mut app = App::logged_in("s");
        app.settings.image_protocol = ProtocolChoice::Halfblocks;
        app.popup = Some(Popup::Settings { sel: 3 });
        let text = render(&mut app, 80, 24);
        assert!(text.contains("Half-blocks"), "{text}");
        assert!(text.contains("next start"), "{text}");
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        let mut app = App::logged_in("s");
        app.popup = Some(Popup::Settings { sel: 1 });
        for (w, h) in [(0, 0), (1, 1), (10, 4), (30, 6)] {
            render(&mut app, w, h);
        }
    }
}

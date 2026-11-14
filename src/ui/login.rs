use super::theme;
use super::widgets::centered;
use crate::app::login::{LoginField, LoginState};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

const WIDTH: u16 = 58;
const HEIGHT: u16 = 12;
const LABEL_WIDTH: usize = 10;

/// The wordmark above the login box. Solid blocks take the accent color and
/// the outline glyphs form a dim shadow.
const LOGO: [&str; 6] = [
    "██████╗ ███████╗ █████╗ ██████╗ ██╗    ██╗ █████╗ ██╗  ██╗",
    "██╔══██╗██╔════╝██╔══██╗██╔══██╗██║    ██║██╔══██╗╚██╗██╔╝",
    "██║  ██║█████╗  ███████║██║  ██║██║ █╗ ██║███████║ ╚███╔╝ ",
    "██║  ██║██╔══╝  ██╔══██║██║  ██║██║███╗██║██╔══██║ ██╔██╗ ",
    "██████╔╝███████╗██║  ██║██████╔╝╚███╔███╔╝██║  ██║██╔╝ ██╗",
    "╚═════╝ ╚══════╝╚═╝  ╚═╝╚═════╝  ╚══╝╚══╝ ╚═╝  ╚═╝╚═╝  ╚═╝",
];
const LOGO_WIDTH: u16 = 58;

/// A two-row wordmark for terminals too narrow for [`LOGO`].
const SMALL_LOGO: [&str; 2] = [
    "█▀▄ █▀▀ ▄▀█ █▀▄ █ █ █ ▄▀█ ▀▄▀",
    "█▄▀ ██▄ █▀█ █▄▀ ▀▄▀▄▀ █▀█ █ █",
];
const SMALL_LOGO_WIDTH: u16 = 29;

/// The blank row between a wordmark and the box.
const LOGO_EXTRA_ROWS: u16 = 1;

/// Picks the largest wordmark that fits above the box, if any.
fn pick_logo(area: Rect) -> Option<(&'static [&'static str], u16)> {
    [(&LOGO[..], LOGO_WIDTH), (&SMALL_LOGO[..], SMALL_LOGO_WIDTH)]
        .into_iter()
        .find(|&(rows, width)| {
            let height = u16::try_from(rows.len()).unwrap_or(u16::MAX) + LOGO_EXTRA_ROWS;
            area.width >= width && area.height >= HEIGHT + height
        })
}

fn logo_lines(rows: &[&str]) -> Vec<Line<'static>> {
    rows.iter()
        .map(|row| {
            Line::from(
                row.chars()
                    .map(|c| {
                        let style = if matches!(c, '█' | '▀' | '▄') {
                            theme::accent()
                        } else {
                            theme::dim()
                        };
                        Span::styled(c.to_string(), style)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

fn field_line(label: &str, value: &str, focused: bool) -> Line<'static> {
    let label_style = if focused {
        theme::accent_bold()
    } else {
        theme::muted()
    };
    let mut spans = vec![
        Span::styled(format!(" {label:<LABEL_WIDTH$}"), label_style),
        Span::raw(value.to_string()),
    ];
    if focused {
        spans.push(Span::styled("▏", theme::accent()));
    }
    Line::from(spans)
}

pub fn draw(frame: &mut Frame, area: Rect, login: &LoginState) {
    let masked = "•".repeat(login.password.chars().count());
    let focus = |f| login.focus == f;

    let notice = if login.busy {
        Line::styled(" Connecting…", theme::muted())
    } else if let Some(err) = &login.error {
        Line::styled(format!(" {err}"), Style::new().fg(Color::Red))
    } else if let Some(warning) = &login.warning {
        Line::styled(format!(" {warning}"), theme::muted())
    } else {
        Line::raw("")
    };

    let lines = vec![
        Line::raw(""),
        field_line("Server", &login.server, focus(LoginField::Server)),
        field_line("Username", &login.username, focus(LoginField::Username)),
        field_line("Password", &masked, focus(LoginField::Password)),
        Line::raw(""),
        notice,
        Line::raw(""),
        Line::from(vec![
            Span::styled(" Tab", theme::accent_bold()),
            Span::styled(" next   ", theme::muted()),
            Span::styled("Enter", theme::accent_bold()),
            Span::styled(" log in   ", theme::muted()),
            Span::styled("Ctrl+C", theme::accent_bold()),
            Span::styled(" quit", theme::muted()),
        ]),
    ];
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border());
    // Without room for a logo, the box carries the name in its title.
    let Some((rows, logo_width)) = pick_logo(area) else {
        let block = block.title(Span::styled(" Deadwax ", theme::accent_bold()));
        let rect = centered(area, WIDTH.min(area.width), HEIGHT.min(area.height));
        frame.render_widget(Paragraph::new(lines).block(block), rect);
        return;
    };
    let logo_height = u16::try_from(rows.len()).unwrap_or(u16::MAX) + LOGO_EXTRA_ROWS;
    let whole = centered(area, WIDTH.min(area.width), HEIGHT + logo_height);
    let [logo, form] =
        Layout::vertical([Constraint::Length(logo_height), Constraint::Length(HEIGHT)])
            .areas(whole);
    let logo = centered(logo, logo_width, logo_height);
    frame.render_widget(Paragraph::new(logo_lines(rows)), logo);
    frame.render_widget(Paragraph::new(lines).block(block), form);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::testing::buffer_text;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(login: &LoginState, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, f.area(), login)).unwrap();
        buffer_text(term.backend().buffer())
    }

    #[test]
    fn password_is_masked() {
        let login = LoginState {
            server: "https://music.example".into(),
            username: "alice".into(),
            password: "sesame".into(),
            focus: LoginField::Password,
            ..Default::default()
        };
        let text = render(&login, 80, 24);
        assert!(!text.contains("sesame"), "{text}");
        assert!(text.contains("••••••"), "{text}");
        assert!(text.contains("alice"), "{text}");
    }

    #[test]
    fn shows_connecting_hint_and_errors() {
        let mut login = LoginState {
            busy: true,
            ..Default::default()
        };
        assert!(render(&login, 80, 24).contains("Connecting…"));
        login.busy = false;
        login.error = Some("Wrong username or password".into());
        assert!(render(&login, 80, 24).contains("Wrong username or password"));
    }

    #[test]
    fn logo_shows_when_it_fits() {
        let text = render(&LoginState::default(), 80, 30);
        assert!(text.contains("██████╗ ███████╗"), "{text}");
        assert!(!text.contains(" Deadwax "), "{text}");
    }

    #[test]
    fn small_terminals_get_the_titled_box_only() {
        for (w, h) in [(28, 30), (80, 14)] {
            let text = render(&LoginState::default(), w, h);
            assert!(!text.contains('█'), "{w}x{h}: {text}");
            assert!(text.contains(" Deadwax "), "{w}x{h}: {text}");
        }
    }

    #[test]
    fn narrow_terminals_get_the_small_logo() {
        for (w, h) in [(53, 30), (80, 18)] {
            let text = render(&LoginState::default(), w, h);
            assert!(text.contains("█▀▄ █▀▀ ▄▀█"), "{w}x{h}: {text}");
            assert!(!text.contains("██████╗"), "{w}x{h}: {text}");
        }
    }

    #[test]
    fn logo_rows_are_one_width() {
        for row in LOGO {
            assert_eq!(row.chars().count(), usize::from(LOGO_WIDTH), "{row:?}");
        }
        for row in SMALL_LOGO {
            assert_eq!(
                row.chars().count(),
                usize::from(SMALL_LOGO_WIDTH),
                "{row:?}"
            );
        }
    }

    #[test]
    fn renders_tiny_terminal() {
        for (w, h) in [(20, 5), (1, 1), (0, 0), (7, 3)] {
            render(&LoginState::default(), w, h);
        }
    }
}

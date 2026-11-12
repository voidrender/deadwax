use super::theme;
use super::widgets::centered;
use crate::app::login::{LoginField, LoginState};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

const WIDTH: u16 = 54;
const HEIGHT: u16 = 12;
const LABEL_WIDTH: usize = 10;

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
        .border_style(theme::border())
        .title(Span::styled(" Senritsu ", theme::accent_bold()));
    let rect = centered(area, WIDTH.min(area.width), HEIGHT.min(area.height));
    frame.render_widget(Paragraph::new(lines).block(block), rect);
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
    fn renders_tiny_terminal() {
        for (w, h) in [(20, 5), (1, 1), (0, 0), (7, 3)] {
            render(&LoginState::default(), w, h);
        }
    }
}

pub mod help;
pub mod library;
pub mod login;
pub mod playlists;
pub mod queue;
pub mod search;
pub mod strip;
pub mod theme;
pub mod widgets;

use crate::app::{App, Popup, Screen, View};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

pub fn draw(frame: &mut Frame, app: &mut App) {
    match app.screen {
        Screen::Main => draw_main(frame, app),
        Screen::Login => login::draw(frame, frame.area(), &app.login),
    }
    let area = frame.area();
    if app.help {
        help::draw(frame, area);
    }
    if let Some(popup) = &app.popup {
        draw_popup(frame, area, popup);
    }
}

fn draw_main(frame: &mut Frame, app: &mut App) {
    let [main, strip_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(strip::HEIGHT)])
            .areas(frame.area());
    let title = if app.now_playing {
        "Now Playing"
    } else {
        app.view.title()
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border())
        .title(Span::styled(format!(" {title} "), theme::accent_bold()));
    let inner = block.inner(main);
    frame.render_widget(block, main);
    if !app.now_playing && app.view == View::Library {
        library::draw(frame, inner, &app.library);
    } else if !app.now_playing && app.view == View::Playlists {
        playlists::draw(frame, inner, &app.playlists);
    } else if !app.now_playing && app.view == View::Search {
        search::draw(frame, inner, &app.search);
    } else if !app.now_playing && app.view == View::Queue {
        queue::draw(frame, inner, app);
    }
    strip::draw(frame, strip_area, app);
}

fn draw_popup(frame: &mut Frame, area: Rect, popup: &Popup) {
    match popup {
        Popup::Confirm { message, .. } => {
            let hint = Line::from(vec![
                Span::styled("y", theme::accent_bold()),
                Span::styled(" yes   ", theme::muted()),
                Span::styled("n", theme::accent_bold()),
                Span::styled(" no", theme::muted()),
            ]);
            let width = message.chars().count().max(24) + 4;
            let rect = widgets::centered(area, u16::try_from(width).unwrap_or(u16::MAX), 5);
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(theme::accent());
            let body = vec![Line::raw(message.as_str()), Line::raw(""), hint];
            frame.render_widget(Clear, rect);
            frame.render_widget(
                Paragraph::new(body)
                    .block(block)
                    .centered()
                    .wrap(Wrap { trim: true }),
                rect,
            );
        }
        Popup::TextInput { title, value, .. } => {
            playlists::draw_text_input(frame, area, title, value)
        }
        Popup::PlaylistPicker(picker) => playlists::draw_picker(frame, area, picker),
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use ratatui::buffer::Buffer;

    /// The buffer's symbols, one line per row, for text assertions.
    pub(crate) fn buffer_text(buf: &Buffer) -> String {
        let area = buf.area;
        let mut out = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::testing::buffer_text;
    use super::*;
    use crate::player::queue::QueueItem;
    use crate::player::{PlayerSnapshot, Status};
    use crate::subsonic::Song;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        buffer_text(term.backend().buffer())
    }

    fn song(title: &str, artist: &str) -> Song {
        Song {
            id: "s1".into(),
            title: title.into(),
            artist: artist.into(),
            artist_id: None,
            album: "Music Has the Right to Children".into(),
            album_id: None,
            track: None,
            year: None,
            duration: 151,
            cover_art: None,
            suffix: None,
            bit_rate: None,
            starred: true,
        }
    }

    fn playing_app() -> App {
        let mut app = App::new();
        app.player = PlayerSnapshot {
            items: vec![QueueItem {
                uid: 1,
                song: song("Roygbiv", "Boards of Canada"),
            }],
            current: Some(0),
            status: Status::Playing,
            position: 72.0,
            duration: 151.0,
            volume: 80,
            ..Default::default()
        };
        app
    }

    #[test]
    fn strip_shows_track_and_times() {
        let mut app = playing_app();
        let text = render(&mut app, 100, 30);
        for want in ["Roygbiv", "Boards of Canada", "1:12", "2:31", "vol 80%"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    #[test]
    fn strip_shows_view_tabs_until_a_status_message_replaces_them() {
        let mut app = playing_app();
        let text = render(&mut app, 100, 30);
        assert!(text.contains("Playlists"), "{text}");

        app.set_status("Saved");
        let text = render(&mut app, 100, 30);
        assert!(text.contains("Saved"), "{text}");
        assert!(!text.contains("Playlists"), "{text}");
    }

    #[test]
    fn empty_player_renders_shell() {
        let mut app = App::new();
        let text = render(&mut app, 80, 24);
        assert!(text.contains("Library"), "{text}");
        assert!(text.contains("0:00"), "{text}");
    }

    #[test]
    fn help_overlay_lists_keys() {
        let mut app = App::new();
        app.help = true;
        let text = render(&mut app, 100, 30);
        assert!(text.contains("Toggle shuffle"), "{text}");
    }

    #[test]
    fn confirm_popup_renders_message() {
        let mut app = App::new();
        app.popup = Some(crate::app::Popup::Confirm {
            message: "Really quit?".into(),
            on_yes: Box::new(crate::app::Effect::Quit),
        });
        let text = render(&mut app, 100, 30);
        assert!(text.contains("Really quit?"), "{text}");
    }

    #[test]
    fn renders_tiny_terminal_with_playlist_popups() {
        use crate::app::playlists::{PickerState, TextPurpose};
        for (w, h) in [(20, 5), (1, 1), (0, 0), (7, 3), (3, 20)] {
            let mut app = App::new();
            app.view = View::Playlists;
            app.popup = Some(Popup::TextInput {
                title: "New playlist".into(),
                value: "a very long playlist name indeed".into(),
                on_submit: TextPurpose::NewPlaylist { song_ids: vec![] },
            });
            render(&mut app, w, h);
            app.popup = Some(Popup::PlaylistPicker(PickerState {
                song_ids: vec!["a".into()],
                songs: vec![],
                lists: crate::app::library::Loadable::Loading,
                sel: 3,
            }));
            render(&mut app, w, h);
        }
    }

    #[test]
    fn renders_tiny_terminal() {
        for (w, h) in [(20, 5), (1, 1), (0, 0), (7, 3), (3, 20)] {
            let mut app = playing_app();
            app.help = true;
            app.popup = Some(crate::app::Popup::Confirm {
                message: "Really quit?".into(),
                on_yes: Box::new(crate::app::Effect::Quit),
            });
            render(&mut app, w, h);
        }
    }
}

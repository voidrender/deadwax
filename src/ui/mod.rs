pub mod art;
pub mod help;
pub mod info;
pub mod library;
pub mod login;
pub mod now_playing;
pub mod playlists;
pub mod queue;
pub mod search;
pub mod settings;
pub mod strip;
pub mod theme;
pub mod widgets;

use crate::app::mouse::{HitMap, Target};
use crate::app::{App, Popup, Screen, View};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

pub fn draw(frame: &mut Frame, app: &mut App) {
    // Taken out of the app so the views can read the app while they add
    // to it.
    let mut hits = std::mem::take(&mut app.hits);
    hits.clear();
    app.art.begin_frame(app.help || app.popup.is_some());
    match app.screen {
        Screen::Main => draw_main(frame, app, &mut hits),
        Screen::Login => login::draw(frame, frame.area(), &app.login, &mut hits),
    }
    let area = frame.area();
    if app.help {
        help::draw(frame, area);
    }
    match &app.popup {
        Some(Popup::Info(info)) => {
            let info = info.clone();
            info::draw(frame, area, &info, &mut app.art);
        }
        Some(Popup::Settings { sel }) => settings::draw(frame, area, app, *sel, &mut hits),
        Some(popup) => draw_popup(frame, area, popup, &mut hits),
        None => {}
    }
    app.art.end_frame();
    app.hits = hits;
}

fn draw_main(frame: &mut Frame, app: &mut App, hits: &mut HitMap) {
    // Full-screen Now Playing shows everything the strip would, so the
    // strip makes way for it.
    let strip_height = if app.now_playing { 0 } else { strip::HEIGHT };
    let [bar, main, strip_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(strip_height),
    ])
    .areas(frame.area());
    draw_top_bar(frame, bar, app, hits);
    if app.now_playing {
        now_playing::draw(frame, main, app, hits);
    } else {
        match app.view {
            View::Library => library::draw(frame, main, &app.library, app.playing_id(), hits),
            View::Playlists => {
                playlists::draw(frame, main, &app.playlists, app.playing_id(), hits);
            }
            View::Search => search::draw(frame, main, &app.search, app.playing_id(), hits),
            View::Queue => queue::draw(frame, main, app, hits),
        }
        strip::draw(frame, strip_area, app, hits);
    }
}

/// The views and their keys on the left, the help key on the right. Plain
/// text with the active view in the accent color, so the bar stays light.
fn draw_top_bar(frame: &mut Frame, area: Rect, app: &App, hits: &mut HitMap) {
    let help = Line::from(vec![
        Span::styled("? ", theme::muted()),
        Span::styled("keys ", theme::dim()),
    ]);
    let [tabs, help_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(u16::try_from(help.width()).unwrap_or(u16::MAX)),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(tabs_line(app.view, app.now_playing)), tabs);
    frame.render_widget(Paragraph::new(help).right_aligned(), help_area);
    hits.add(help_area, Target::Help);
    // Each tab's number and title, matching the spans of `tabs_line`.
    let mut x = tabs.x.saturating_add(TAB_LEAD);
    for (n, view) in (1..).zip(View::ALL) {
        let width = u16::try_from(format!("{n} {}", view.title()).len()).unwrap_or(u16::MAX);
        let rect = Rect::new(x, tabs.y, width, 1).intersection(tabs);
        hits.add(rect, Target::Tab(view));
        x = x.saturating_add(width).saturating_add(TAB_GAP);
    }
}

/// Columns before the first tab and between tabs.
const TAB_LEAD: u16 = 1;
const TAB_GAP: u16 = 3;

fn tabs_line(active: View, now_playing: bool) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(TAB_LEAD.into()))];
    for (n, view) in (1..).zip(View::ALL) {
        if n > 1 {
            spans.push(Span::raw(" ".repeat(TAB_GAP.into())));
        }
        let (key, title) = if view == active && !now_playing {
            (theme::accent(), theme::accent_bold())
        } else {
            (theme::dim(), theme::muted())
        };
        spans.push(Span::styled(format!("{n} "), key));
        spans.push(Span::styled(view.title(), title));
    }
    Line::from(spans)
}

fn draw_popup(frame: &mut Frame, area: Rect, popup: &Popup, hits: &mut HitMap) {
    match popup {
        Popup::Confirm { message, .. } => {
            let hint = Line::from(vec![
                Span::styled("y", theme::accent_bold()),
                Span::styled(" yes   ", theme::muted()),
                Span::styled("n", theme::accent_bold()),
                Span::styled(" no", theme::muted()),
            ]);
            let width = widgets::display_width(message).max(24) + 4;
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
            playlists::draw_text_input(frame, area, title, value);
        }
        Popup::PlaylistPicker(picker) => playlists::draw_picker(frame, area, picker, hits),
        // Drawn by `info::draw` and `settings::draw`, which need more than
        // the popup.
        Popup::Info(_) | Popup::Settings { .. } => {}
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use ratatui::buffer::Buffer;

    /// The buffer's symbols, one line per row, for text assertions.
    /// Cells hidden behind a wide character are skipped, so each line reads
    /// as it appears on screen.
    pub(crate) fn buffer_text(buf: &Buffer) -> String {
        use unicode_width::UnicodeWidthStr;
        let area = buf.area;
        let mut out = String::new();
        for y in area.top()..area.bottom() {
            let mut x = area.left();
            while x < area.right() {
                let symbol = buf[(x, y)].symbol();
                out.push_str(symbol);
                x += u16::try_from(symbol.width().max(1)).unwrap_or(1);
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
            ..Default::default()
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
    fn top_bar_shows_the_views_and_the_strip_shows_status() {
        let mut app = playing_app();
        let text = render(&mut app, 100, 30);
        let top = text.lines().next().unwrap();
        for want in ["1 Library", "2 Playlists", "3 Search", "4 Queue", "? keys"] {
            assert!(top.contains(want), "missing {want:?} in {top:?}");
        }
        assert!(!top.contains("Now Playing"), "{top:?}");
        assert!(
            text.lines().last().unwrap().contains("z Now Playing"),
            "{text}"
        );

        app.set_status("Saved");
        let text = render(&mut app, 100, 30);
        let last = text.lines().last().unwrap();
        assert!(last.contains("Saved") && last.contains("vol 80%"), "{text}");
        assert!(!last.contains("z Now Playing"), "{text}");
    }

    #[test]
    fn views_have_no_outer_frame() {
        let mut app = playing_app();
        let text = render(&mut app, 100, 30);
        // The first pane border sits right under the top bar, at column 0.
        assert!(
            text.lines().nth(1).unwrap().starts_with("╭ Artists"),
            "{text}"
        );
    }

    #[test]
    fn now_playing_hides_the_strip_but_keeps_its_information() {
        let mut app = playing_app();
        app.now_playing = true;
        app.set_status("Saved");
        let text = render(&mut app, 100, 30);
        for want in ["Roygbiv", "1:12", "vol 80%", "Saved"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        // Only the details show the volume, so the strip is gone.
        assert_eq!(text.matches("vol 80%").count(), 1, "{text}");
        // The status message takes the place of the close hint.
        assert!(!text.contains("z close"), "{text}");

        app.status = None;
        let text = render(&mut app, 100, 30);
        assert!(text.contains("z close"), "{text}");
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
    fn confirm_popup_sizes_wide_messages_by_display_width() {
        let message = "プレイリスト旋律を削除しますか";
        let mut app = App::new();
        app.popup = Some(crate::app::Popup::Confirm {
            message: message.into(),
            on_yes: Box::new(crate::app::Effect::Quit),
        });
        let text = render(&mut app, 100, 30);
        assert!(text.contains(message), "{text}");
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
    fn renders_cover_art_in_strip_and_now_playing_at_any_size() {
        let mut app = playing_app();
        app.player.items[0].song.cover_art = Some("al-1".into());
        app.handle_event(crate::app::AppEvent::Snapshot(app.player.clone()));
        let image = image::RgbImage::from_fn(32, 32, |x, y| {
            image::Rgb([
                u8::try_from(x * 8).unwrap(),
                40,
                u8::try_from(y * 8).unwrap(),
            ])
        });
        app.handle_event(crate::app::AppEvent::CoverReady {
            cover_id: "al-1".into(),
            image: image.into(),
        });
        let text = render(&mut app, 100, 30);
        assert!(text.contains(['▀', '▄']), "strip art missing:\n{text}");
        for now_playing in [false, true] {
            app.now_playing = now_playing;
            for (w, h) in [
                (20, 5),
                (1, 1),
                (0, 0),
                (7, 3),
                (3, 20),
                (62, 21),
                (100, 30),
            ] {
                render(&mut app, w, h);
            }
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

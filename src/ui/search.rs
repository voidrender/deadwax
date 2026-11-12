use super::library::fit;
use super::theme;
use super::widgets::{format_duration, pane, tail};
use crate::app::library::Loadable;
use crate::app::search::{SearchRow, SearchState};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph, Wrap};

pub fn draw(frame: &mut Frame, area: Rect, search: &SearchState) {
    let [input_area, list_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);
    draw_input(frame, input_area, search);
    draw_results(frame, list_area, search);
}

fn draw_input(frame: &mut Frame, area: Rect, search: &SearchState) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if search.editing {
            theme::accent()
        } else {
            theme::border()
        });
    let mut spans = vec![Span::styled("/ ", theme::accent_bold())];
    if search.input.is_empty() && !search.editing {
        spans.push(Span::styled("Press / to search", theme::dim()));
    } else {
        // Prefix "/ " and cursor take three columns inside the borders.
        let room = usize::from(area.width).saturating_sub(5);
        spans.push(Span::raw(tail(&search.input, room)));
    }
    if search.editing {
        spans.push(Span::styled("▏", theme::accent()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
}

fn draw_results(frame: &mut Frame, area: Rect, search: &SearchState) {
    let block = pane("Results", !search.editing);
    let area = {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        inner
    };
    if area.is_empty() {
        return;
    }
    let mut message = |text: &str, style: Style| {
        frame.render_widget(
            Paragraph::new(Line::styled(text.to_string(), style)).wrap(Wrap { trim: true }),
            area,
        );
    };
    let results = match &search.results {
        Loadable::NotLoaded => return message("Type to search your library", theme::dim()),
        Loadable::Loading => return message("Searching…", theme::dim()),
        Loadable::Failed(msg) => return message(msg, theme::muted()),
        Loadable::Loaded(r) => r,
    };
    let rows = search.rows();
    if rows.is_empty() {
        return message("No results", theme::dim());
    }
    let height = usize::from(area.height);
    let width = usize::from(area.width);
    let sel = search.sel.min(rows.len() - 1);
    let offset = (sel + 1).saturating_sub(height);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .filter_map(|(i, row)| {
            let (text, style) = match *row {
                SearchRow::Header(name) => (name.to_string(), theme::accent_bold()),
                SearchRow::Artist(n) => {
                    let a = results.artists.get(n)?;
                    (fit(&format!("  {}", a.name), "", width), Style::new())
                }
                SearchRow::Album(n) => {
                    let a = results.albums.get(n)?;
                    (
                        fit(&format!("  {}", a.name), &a.artist, width),
                        Style::new(),
                    )
                }
                SearchRow::Song(n) => {
                    let s = results.songs.get(n)?;
                    let mark = if s.starred { "♥ " } else { "  " };
                    let right = format!("{}  {}", s.artist, format_duration(f64::from(s.duration)));
                    (
                        fit(&format!("{mark}{}", s.title), &right, width),
                        Style::new(),
                    )
                }
            };
            let style = if i == sel && !search.editing && !matches!(row, SearchRow::Header(_)) {
                theme::selected()
            } else {
                style
            };
            Some(Line::styled(text, style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subsonic::{Album, Artist, SearchResults, Song};
    use crate::ui::testing::buffer_text;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(s: &SearchState, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, f.area(), s)).unwrap();
        buffer_text(term.backend().buffer())
    }

    fn loaded() -> SearchState {
        let song = Song {
            id: "s".into(),
            title: "Roygbiv".into(),
            artist: "Boards of Canada".into(),
            artist_id: None,
            album: "MHTRTC".into(),
            album_id: None,
            track: None,
            year: None,
            duration: 151,
            cover_art: None,
            suffix: None,
            bit_rate: None,
            starred: true,
        };
        let album = Album {
            id: "b".into(),
            name: "Geogaddi".into(),
            artist: "Boards of Canada".into(),
            artist_id: Some("a".into()),
            year: None,
            song_count: 0,
            duration: 0,
            cover_art: None,
            music_brainz_id: None,
        };
        let artist = Artist {
            id: "a".into(),
            name: "Boards of Canada".into(),
            album_count: 2,
            cover_art: None,
        };
        SearchState {
            input: "boards".into(),
            results: Loadable::Loaded(SearchResults {
                artists: vec![artist],
                albums: vec![album],
                songs: vec![song],
            }),
            sel: 1,
            ..SearchState::default()
        }
    }

    #[test]
    fn shows_input_and_sections() {
        let text = render(&loaded(), 60, 14);
        for want in [
            "/ boards",
            "Artists",
            "Albums",
            "Songs",
            "Geogaddi",
            "♥ Roygbiv",
            "2:31",
        ] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    #[test]
    fn shows_state_messages() {
        let mut s = SearchState::default();
        assert!(render(&s, 40, 8).contains("Press / to search"));
        s.results = Loadable::Loading;
        assert!(render(&s, 40, 8).contains("Searching…"));
        s.results = Loadable::Loaded(SearchResults::default());
        assert!(render(&s, 40, 8).contains("No results"));
        s.results = Loadable::Failed("boom".into());
        assert!(render(&s, 40, 8).contains("boom"));
    }

    #[test]
    fn renders_tiny_areas_and_bad_selection() {
        let mut s = loaded();
        s.sel = 99;
        s.editing = true;
        for (w, h) in [(0, 0), (1, 1), (5, 3), (3, 20), (40, 4), (40, 2)] {
            render(&s, w, h);
        }
    }

    #[test]
    fn long_wide_input_shows_its_end_and_the_cursor() {
        let s = SearchState {
            input: "旋律旋律旋律旋律旋律旋律旋律旋律旋律旋律旋律旋律終".into(),
            editing: true,
            ..SearchState::default()
        };
        let text = render(&s, 30, 8);
        assert!(text.contains("終▏"), "{text}");
    }
}

use super::library::{Body, body_of, draw_rows, is_playing_at, track_row};
use super::theme;
use super::widgets::{centered, pane, tail};
use crate::app::library::Loadable;
use crate::app::mouse::{HitMap, List, Target};
use crate::app::playlists::{PickerState, PlColumn, PlaylistsState, songs_label};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

/// Below this width only the focused column is shown.
const WIDE: u16 = 60;
/// Widest the popups grow.
const POPUP_WIDTH: u16 = 44;
/// Most picker rows shown at once.
const PICKER_ROWS: u16 = 10;

/// Draws the playlist columns, marking the song the player is on.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    pl: &PlaylistsState,
    playing: Option<&str>,
    hits: &mut HitMap,
) {
    let cols: &[PlColumn] = if area.width >= WIDE {
        &[PlColumn::Lists, PlColumn::Tracks]
    } else if pl.focus == PlColumn::Lists {
        &[PlColumn::Lists]
    } else {
        &[PlColumn::Tracks]
    };
    let weights: Vec<Constraint> = cols
        .iter()
        .map(|c| match c {
            PlColumn::Lists => Constraint::Fill(2),
            PlColumn::Tracks => Constraint::Fill(3),
        })
        .collect();
    let rects = Layout::horizontal(weights).split(area);
    for (col, rect) in cols.iter().zip(rects.iter()) {
        draw_column(frame, *rect, pl, *col, playing, hits);
    }
}

fn draw_column(
    frame: &mut Frame,
    area: Rect,
    pl: &PlaylistsState,
    col: PlColumn,
    playing: Option<&str>,
    hits: &mut HitMap,
) {
    let list = List::Playlists(col);
    hits.add(area, Target::Pane(list));
    let focused = pl.focus == col;
    let title = match col {
        PlColumn::Lists => format!("Playlists · {}", pl.list_sort.label()),
        PlColumn::Tracks => "Tracks".to_string(),
    };
    let block = pane(&title, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }
    let body = match col {
        PlColumn::Lists => body_of(&pl.lists, "No playlists. Press n to make one", |p| {
            (p.name.clone(), p.song_count.to_string())
        }),
        PlColumn::Tracks => body_of(&pl.tracks, "No songs", |s| track_row(s, playing)),
    };
    let is_playing = |i: usize| match col {
        PlColumn::Tracks => is_playing_at(pl.tracks.loaded(), i, playing),
        PlColumn::Lists => false,
    };
    match body {
        Body::Rows(rows) => draw_rows(
            frame,
            inner,
            &rows,
            pl.sel[col.index()],
            focused,
            list,
            is_playing,
            hits,
        ),
        Body::Message(lines) => {
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
        }
    }
}

fn popup_block(title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::accent())
        .title(Span::styled(format!(" {title} "), theme::accent_bold()))
}

pub fn draw_text_input(frame: &mut Frame, area: Rect, title: &str, value: &str) {
    let rect = centered(area, POPUP_WIDTH.min(area.width), 5.min(area.height));
    let room = usize::from(rect.width.saturating_sub(4));
    // Show the end of the text so the cursor stays visible.
    let shown = tail(value, room.saturating_sub(1));
    let body = vec![
        Line::from(vec![Span::raw(shown), Span::styled("█", theme::accent())]),
        Line::raw(""),
        Line::from(vec![
            Span::styled("Enter", theme::accent_bold()),
            Span::styled(" save   ", theme::muted()),
            Span::styled("Esc", theme::accent_bold()),
            Span::styled(" cancel", theme::muted()),
        ]),
    ];
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(body).block(popup_block(title)), rect);
}

pub fn draw_picker(frame: &mut Frame, area: Rect, picker: &PickerState, hits: &mut HitMap) {
    let mut rows = vec![("+ New playlist".to_string(), String::new())];
    match &picker.lists {
        Loadable::Loaded(lists) => {
            rows.extend(
                lists
                    .iter()
                    .map(|p| (p.name.clone(), p.song_count.to_string())),
            );
        }
        Loadable::Failed(msg) => rows.push((format!("Couldn't load: {msg}"), String::new())),
        Loadable::NotLoaded | Loadable::Loading => {
            rows.push(("Loading…".to_string(), String::new()));
        }
    }
    let shown = u16::try_from(rows.len())
        .unwrap_or(u16::MAX)
        .min(PICKER_ROWS);
    let rect = centered(
        area,
        POPUP_WIDTH.min(area.width),
        (shown + 2).min(area.height),
    );
    let block = popup_block(&format!("Add {} to", songs_label(picker.song_ids.len())));
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    if !inner.is_empty() {
        draw_rows(
            frame,
            inner,
            &rows,
            picker.sel,
            true,
            List::Picker,
            |_| false,
            hits,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subsonic::{Playlist, Song};
    use crate::ui::testing::buffer_text;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(f: impl FnOnce(&mut Frame), w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(f).unwrap();
        buffer_text(term.backend().buffer())
    }

    fn playlist(name: &str) -> Playlist {
        Playlist {
            id: name.into(),
            name: name.into(),
            song_count: 7,
            duration: 0,
            owner: None,
            ..Default::default()
        }
    }

    fn song(title: &str) -> Song {
        Song {
            id: title.into(),
            title: title.into(),
            artist: "A".into(),
            artist_id: None,
            album: "B".into(),
            album_id: None,
            track: None,
            year: None,
            duration: 125,
            cover_art: None,
            suffix: None,
            bit_rate: None,
            starred: true,
            ..Default::default()
        }
    }

    #[test]
    fn shows_playlists_and_tracks() {
        let mut pl = PlaylistsState::default();
        pl.lists = Loadable::Loaded(vec![playlist("Road Trip")]);
        pl.tracks = Loadable::Loaded(vec![song("Roygbiv")]);
        let text = render(
            |f| draw(f, f.area(), &pl, None, &mut HitMap::default()),
            90,
            8,
        );
        for want in ["Playlists", "Road Trip", "Roygbiv", "2:05", "♥"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    #[test]
    fn picker_lists_new_playlist_first() {
        let picker = PickerState {
            song_ids: vec!["a".into(), "b".into()],
            songs: vec![],
            lists: Loadable::Loaded(vec![playlist("Mix")]),
            sel: 0,
        };
        let text = render(
            |f| draw_picker(f, f.area(), &picker, &mut HitMap::default()),
            60,
            12,
        );
        assert!(text.contains("Add 2 songs to"), "{text}");
        let new = text.find("+ New playlist").expect("new row");
        let mix = text.find("Mix").expect("mix row");
        assert!(new < mix, "{text}");
    }

    #[test]
    fn text_input_shows_value_and_title() {
        let text = render(
            |f| draw_text_input(f, f.area(), "Rename playlist", "Mix"),
            60,
            12,
        );
        assert!(
            text.contains("Rename playlist") && text.contains("Mix"),
            "{text}"
        );
    }

    #[test]
    fn text_input_keeps_the_cursor_visible_with_wide_text() {
        let value =
            "旋律旋律旋律旋律旋律旋律旋律旋律旋律旋律旋律旋律旋律旋律旋律旋律旋律旋律旋律旋律終";
        let text = render(|f| draw_text_input(f, f.area(), "Rename", value), 60, 12);
        assert!(text.contains("終█"), "{text}");
    }
}

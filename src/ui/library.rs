use super::theme;
use super::widgets::{display_width, format_duration, truncate};
use crate::app::library::{ArtistRow, Column, LibraryState, Loadable};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph, Wrap};

/// Below this width only the focused column and its parent are shown.
const WIDE: u16 = 80;

pub fn draw(frame: &mut Frame, area: Rect, lib: &LibraryState) {
    let cols: &[Column] = if area.width >= WIDE {
        &[Column::Artists, Column::Albums, Column::Tracks]
    } else {
        match lib.focus {
            Column::Artists | Column::Albums => &[Column::Artists, Column::Albums],
            Column::Tracks => &[Column::Albums, Column::Tracks],
        }
    };
    let weights: Vec<Constraint> = cols
        .iter()
        .map(|c| match c {
            Column::Tracks => Constraint::Fill(3),
            _ => Constraint::Fill(2),
        })
        .collect();
    let rects = Layout::horizontal(weights).split(area);
    for (col, rect) in cols.iter().zip(rects.iter()) {
        draw_column(frame, *rect, lib, *col);
    }
}

fn draw_column(frame: &mut Frame, area: Rect, lib: &LibraryState, col: Column) {
    let focused = lib.focus == col;
    let title = match col {
        Column::Artists => format!("Artists · {}", lib.artist_sort.label()),
        Column::Albums => format!("Albums · {}", lib.album_sort.label()),
        Column::Tracks => format!("Tracks · {}", lib.track_sort.label()),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            theme::accent()
        } else {
            theme::border()
        })
        .title(Span::styled(
            format!(" {title} "),
            if focused {
                theme::accent_bold()
            } else {
                theme::muted()
            },
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }
    let sel = lib.sel[col.index()];
    let body = match col {
        Column::Artists => body_of(&lib.artists, "No artists", |r| match r {
            ArtistRow::Favorites => ("★ Favorites".to_string(), String::new()),
            ArtistRow::Artist(a) => (a.name.clone(), String::new()),
        }),
        Column::Albums => body_of(&lib.albums, "No albums", |a| {
            (
                a.name.clone(),
                a.year.map(|y| y.to_string()).unwrap_or_default(),
            )
        }),
        Column::Tracks => body_of(&lib.tracks, "No songs", |s| {
            let mark = if s.starred { "♥ " } else { "  " };
            (
                format!("{mark}{}", s.title),
                format_duration(f64::from(s.duration)),
            )
        }),
    };
    match body {
        Body::Rows(rows) => draw_rows(frame, inner, &rows, sel, focused),
        Body::Message(lines) => {
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
        }
    }
}

pub(super) enum Body {
    /// Left text and right-aligned text per row.
    Rows(Vec<(String, String)>),
    Message(Vec<Line<'static>>),
}

pub(super) fn body_of<T>(
    data: &Loadable<Vec<T>>,
    empty: &str,
    row: impl Fn(&T) -> (String, String),
) -> Body {
    match data {
        Loadable::NotLoaded => Body::Message(Vec::new()),
        Loadable::Loading => Body::Message(vec![Line::styled("Loading…", theme::dim())]),
        Loadable::Failed(msg) => Body::Message(vec![
            Line::styled(msg.clone(), theme::muted()),
            Line::styled("Press l to retry", theme::dim()),
        ]),
        Loadable::Loaded(items) if items.is_empty() => {
            Body::Message(vec![Line::styled(empty.to_string(), theme::dim())])
        }
        Loadable::Loaded(items) => Body::Rows(items.iter().map(row).collect()),
    }
}

pub(super) fn draw_rows(
    frame: &mut Frame,
    area: Rect,
    rows: &[(String, String)],
    sel: usize,
    focused: bool,
) {
    let height = usize::from(area.height);
    let width = usize::from(area.width);
    let sel = sel.min(rows.len().saturating_sub(1));
    let offset = (sel + 1).saturating_sub(height);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, (left, right))| {
            let style = match (i == sel, focused) {
                (true, true) => theme::selected(),
                (true, false) => theme::accent_bold(),
                (false, _) => ratatui::style::Style::new(),
            };
            Line::styled(fit(left, right, width), style)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Pads `left` and `right` to exactly `width` columns, truncating `left`
/// with an ellipsis when they do not fit.
pub(super) fn fit(left: &str, right: &str, width: usize) -> String {
    let right_len = display_width(right);
    let room = if right_len == 0 {
        width
    } else {
        width.saturating_sub(right_len + 1)
    };
    let mut out = truncate(left, room);
    let used = display_width(&out);
    out.push_str(&" ".repeat(width.saturating_sub(used + right_len)));
    if right_len + used <= width {
        out.push_str(right);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::library::ArtistRow;
    use crate::subsonic::{Album, Artist};
    use crate::ui::testing::buffer_text;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(lib: &LibraryState, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, f.area(), lib)).unwrap();
        buffer_text(term.backend().buffer())
    }

    fn state() -> LibraryState {
        let mut lib = LibraryState::default();
        lib.artists = Loadable::Loaded(vec![
            ArtistRow::Favorites,
            ArtistRow::Artist(Artist {
                id: "a".into(),
                name: "Boards of Canada".into(),
                album_count: 1,
                cover_art: None,
            }),
        ]);
        lib.albums = Loadable::Loaded(vec![Album {
            id: "x".into(),
            name: "Geogaddi".into(),
            artist: String::new(),
            artist_id: None,
            year: Some(2002),
            song_count: 0,
            duration: 0,
            cover_art: None,
            music_brainz_id: None,
        }]);
        lib
    }

    #[test]
    fn wide_shows_three_columns() {
        let text = render(&state(), 100, 10);
        for want in [
            "Artists · name",
            "Albums · newest",
            "Tracks · album order",
            "Boards of Canada",
            "Geogaddi",
            "2002",
        ] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    #[test]
    fn narrow_width_shows_two_columns() {
        let mut lib = state();
        lib.focus = Column::Tracks;
        let text = render(&lib, 60, 10);
        assert!(text.contains("Albums") && text.contains("Tracks"), "{text}");
        assert!(!text.contains("Artists"), "{text}");
        lib.focus = Column::Albums;
        let text = render(&lib, 60, 10);
        assert!(
            text.contains("Artists") && text.contains("Albums"),
            "{text}"
        );
        assert!(!text.contains("Tracks"), "{text}");
    }

    #[test]
    fn failed_load_shows_error_and_retry_hint() {
        let mut lib = LibraryState::default();
        lib.artists = Loadable::Failed("network error: timeout".into());
        let text = render(&lib, 100, 10);
        assert!(text.contains("timeout") && text.contains("retry"), "{text}");
    }

    #[test]
    fn renders_at_any_size() {
        let mut lib = state();
        lib.sel = [9, 9, 9];
        for focus in [Column::Artists, Column::Albums, Column::Tracks] {
            lib.focus = focus;
            for (w, h) in [(0, 0), (1, 1), (3, 2), (5, 20), (79, 3), (80, 1), (200, 4)] {
                render(&lib, w, h);
            }
        }
    }

    #[test]
    fn fit_measures_display_width_for_wide_titles() {
        use unicode_width::UnicodeWidthStr;
        let row = fit("旋律のテスト曲名", "2:31", 20);
        assert_eq!(row.width(), 20, "{row:?}");
        assert!(row.ends_with("2:31"), "{row:?}");
        assert!(row.contains('…'), "{row:?}");
        // An odd room never splits a wide char: 21 leaves 16 for the title.
        let row = fit("旋律のテスト曲名ですよ", "2:31", 21);
        assert_eq!(row.width(), 21, "{row:?}");
        assert!(row.ends_with("2:31"), "{row:?}");
        // Short wide titles pad by display width.
        let row = fit("旋律", "2:31", 20);
        assert_eq!(row.width(), 20, "{row:?}");
    }

    #[test]
    fn wide_title_row_keeps_duration_on_screen() {
        let rows = vec![("旋律のテスト曲名".to_string(), "2:31".to_string())];
        let mut term = Terminal::new(TestBackend::new(20, 1)).unwrap();
        term.draw(|f| draw_rows(f, f.area(), &rows, 0, false))
            .unwrap();
        let text = buffer_text(term.backend().buffer());
        assert!(text.trim_end().ends_with("2:31"), "{text}");
    }
}

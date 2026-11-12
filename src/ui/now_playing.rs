//! Full-screen Now Playing: large art on the left, track details on the right.

use super::art::Slot;
use super::{strip, theme};
use crate::app::App;
use crate::player::PlayerSnapshot;
use crate::player::queue::RepeatMode;
use crate::subsonic::Song;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Below this size the view shows text only.
pub const MIN_WIDTH: u16 = 60;
pub const MIN_HEIGHT: u16 = 16;
/// Share of the width the art may take.
const ART_PERCENT: u32 = 45;
/// Widest the art gets, in columns, so it stops growing on large terminals.
/// With the usual 1:2 cell this is about 30 rows tall.
const MAX_ART_WIDTH: u32 = 60;
/// Columns left of the art and between art and text.
const MARGIN: u16 = 2;

pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        draw_details(frame, area, &app.player);
        return;
    }
    let font = app.art.picker.font_size();
    let art = art_rect(area, u32::from(font.width), u32::from(font.height));
    app.art.render(frame, art, Slot::Large);
    let left = art.right() + MARGIN;
    let text = Rect {
        x: left,
        width: area.right().saturating_sub(left + 1),
        ..area
    };
    draw_details(frame, text, &app.player);
}

/// The largest area, at most `ART_PERCENT` of the width and `MAX_ART_WIDTH`
/// columns, that is square in pixels for the given cell size, centered
/// vertically.
fn art_rect(area: Rect, cell_w: u32, cell_h: u32) -> Rect {
    let (cell_w, cell_h) = (cell_w.max(1), cell_h.max(1));
    let max_w = (u32::from(area.width) * ART_PERCENT / 100).min(MAX_ART_WIDTH);
    let max_h = u32::from(area.height.saturating_sub(2));
    let mut w = max_w;
    let mut h = w * cell_w / cell_h;
    if h > max_h {
        h = max_h;
        w = (h * cell_h / cell_w).min(max_w);
    }
    // Both fit: w is at most the area width and h at most its height.
    let w = u16::try_from(w).unwrap_or(area.width);
    let h = u16::try_from(h).unwrap_or(area.height);
    Rect::new(area.x + MARGIN, area.y + (area.height - h) / 2, w, h)
}

fn draw_details(frame: &mut Frame, area: Rect, snap: &PlayerSnapshot) {
    let lines = detail_lines(snap, area.width);
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let top = area.height.saturating_sub(height) / 2;
    let rect = Rect {
        y: area.y + top,
        height: area.height - top,
        ..area
    };
    frame.render_widget(Paragraph::new(lines), rect);
}

fn current(snap: &PlayerSnapshot) -> Option<&Song> {
    snap.current
        .and_then(|i| snap.items.get(i))
        .map(|item| &item.song)
}

/// What plays after the current track, following the engine's rules.
fn next(snap: &PlayerSnapshot) -> Option<&Song> {
    let pos = snap.current?;
    let item = match snap.repeat {
        RepeatMode::One => snap.items.get(pos),
        RepeatMode::All => snap.items.get(pos + 1).or_else(|| snap.items.first()),
        RepeatMode::Off => snap.items.get(pos + 1),
    };
    item.map(|i| &i.song)
}

fn joined(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ")
}

fn detail_lines(snap: &PlayerSnapshot, width: u16) -> Vec<Line<'static>> {
    let Some(song) = current(snap) else {
        return vec![Line::styled("Nothing playing", theme::dim())];
    };
    let mut lines = vec![Line::styled(song.title.clone(), theme::accent_bold())];
    if !song.artist.is_empty() {
        lines.push(Line::raw(song.artist.clone()));
    }
    let album = joined(&[
        song.album.clone(),
        song.year.map(|y| y.to_string()).unwrap_or_default(),
    ]);
    if !album.is_empty() {
        lines.push(Line::styled(album, theme::muted()));
    }
    if let Some(n) = song.track {
        lines.push(Line::styled(format!("track {n}"), theme::muted()));
    }
    let format = joined(&[
        song.suffix
            .as_deref()
            .map(str::to_uppercase)
            .unwrap_or_default(),
        song.bit_rate
            .map(|b| format!("{b} kbps"))
            .unwrap_or_default(),
    ]);
    if !format.is_empty() {
        lines.push(Line::styled(format, theme::muted()));
    }
    if song.starred {
        lines.push(Line::styled("♥ favorited", theme::accent()));
    }
    lines.push(Line::raw(""));
    lines.push(strip::progress_line(snap, width));
    if let Some(next) = next(snap) {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("Next: ", theme::dim()),
            Span::styled(next.title.clone(), theme::muted()),
        ]));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_grows_with_the_area_up_to_a_cap() {
        // A 100x40 area gives 45 columns, square for 1:2 cells.
        let small = art_rect(Rect::new(0, 0, 100, 40), 8, 16);
        assert_eq!((small.width, small.height), (45, 22));
        // A huge area stops at the cap and stays square.
        let big = art_rect(Rect::new(0, 0, 400, 120), 8, 16);
        assert_eq!((big.width, big.height), (60, 30));
        assert_eq!(big.y, (120 - 30) / 2);
        // A short area is limited by height first.
        let short = art_rect(Rect::new(0, 0, 400, 22), 8, 16);
        assert_eq!((short.width, short.height), (40, 20));
    }
    use crate::player::queue::QueueItem;
    use crate::player::{PlayerSnapshot, Status};
    use crate::subsonic::Song;
    use crate::ui::testing::buffer_text;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn song(id: &str, title: &str, cover: Option<&str>) -> Song {
        Song {
            id: id.into(),
            title: title.into(),
            artist: "Boards of Canada".into(),
            artist_id: None,
            album: "Music Has the Right to Children".into(),
            album_id: None,
            track: Some(3),
            year: Some(1998),
            duration: 151,
            cover_art: cover.map(Into::into),
            suffix: Some("flac".into()),
            bit_rate: Some(1000),
            starred: true,
        }
    }

    fn app_with(cover: Option<&str>) -> App {
        let mut app = App::new();
        app.now_playing = true;
        app.player = PlayerSnapshot {
            items: vec![
                QueueItem {
                    uid: 1,
                    song: song("s1", "Roygbiv", cover),
                },
                QueueItem {
                    uid: 2,
                    song: song("s2", "Turquoise Hexagon Sun", cover),
                },
            ],
            current: Some(0),
            status: Status::Playing,
            position: 72.0,
            duration: 151.0,
            volume: 80,
            ..Default::default()
        };
        app
    }

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, f.area(), app)).unwrap();
        buffer_text(term.backend().buffer())
    }

    fn cover_image() -> image::DynamicImage {
        image::RgbImage::from_fn(64, 64, |x, y| {
            image::Rgb([
                u8::try_from(x * 4).unwrap(),
                u8::try_from(y * 4).unwrap(),
                128,
            ])
        })
        .into()
    }

    #[test]
    fn now_playing_shows_details() {
        let mut app = app_with(Some("al-1"));
        app.art.expect("al-1".into());
        app.art.set_image("al-1", cover_image());
        let text = render(&mut app, 100, 30);
        for want in [
            "Roygbiv",
            "Boards of Canada",
            "Music Has the Right to Children · 1998",
            "track 3",
            "FLAC · 1000 kbps",
            "♥ favorited",
            "1:12",
            "2:31",
            "Next: Turquoise Hexagon Sun",
        ] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        assert!(text.contains(['▀', '▄']), "no half-block art in\n{text}");
        assert!(!text.contains('♪'), "{text}");
    }

    #[test]
    fn now_playing_without_cover_shows_placeholder() {
        let mut app = app_with(None);
        let text = render(&mut app, 100, 30);
        assert!(text.contains('♪'), "{text}");
        assert!(text.contains("Roygbiv"), "{text}");
    }

    #[test]
    fn small_now_playing_shows_text_only() {
        let mut app = app_with(Some("al-1"));
        app.art.expect("al-1".into());
        app.art.set_image("al-1", cover_image());
        let text = render(&mut app, 59, 20);
        assert!(text.contains("Roygbiv"), "{text}");
        assert!(!text.contains(['♪', '▀', '▄']), "{text}");
    }

    #[test]
    fn renders_at_any_size_without_panicking() {
        let mut app = app_with(Some("al-1"));
        app.art.expect("al-1".into());
        app.art.set_image("al-1", cover_image());
        for (w, h) in [
            (0, 0),
            (1, 1),
            (60, 16),
            (61, 17),
            (200, 16),
            (60, 100),
            (7, 3),
        ] {
            render(&mut app, w, h);
        }
        let mut empty = App::new();
        empty.now_playing = true;
        assert!(render(&mut empty, 80, 20).contains("Nothing playing"));
    }
}

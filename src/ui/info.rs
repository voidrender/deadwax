//! The info popup: cover on the left, details on the right.

use super::art::{ArtState, Slot};
use super::theme;
use super::widgets::{centered, display_width, format_duration, truncate};
use crate::app::info::Info;
use crate::app::playlists::songs_label;
use crate::subsonic::Song;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

/// Widest the popup gets, in columns.
const MAX_WIDTH: u16 = 76;
/// Rows the body gets at least, so the cover has room.
const MIN_BODY_ROWS: u16 = 10;
/// Below these the cover is left out.
const ART_MIN_WIDTH: u16 = 44;
const ART_MIN_ROWS: u16 = 6;
/// Columns between the cover and the text.
const GAP: u16 = 3;
/// Width of the label column, including the space after it.
const LABEL_WIDTH: usize = 8;

pub fn draw(frame: &mut Frame, area: Rect, info: &Info, art: &mut ArtState) {
    let details = Details::of(info);
    let text_rows = u16::try_from(details.line_count()).unwrap_or(u16::MAX);
    // Borders and padding take 4 rows and 6 columns.
    let body_rows = text_rows
        .max(MIN_BODY_ROWS)
        .min(area.height.saturating_sub(6));
    let width = area.width.saturating_sub(4).min(MAX_WIDTH);
    let rect = centered(area, width, body_rows + 4);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::accent())
        .padding(Padding::new(2, 2, 1, 1))
        .title(Span::styled(
            format!(" {} ", info.kind()),
            theme::accent_bold(),
        ))
        .title_bottom(
            Line::from(vec![
                Span::styled(" i ", theme::muted()),
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

    let mut text = inner;
    if inner.width >= ART_MIN_WIDTH && inner.height >= ART_MIN_ROWS {
        let font = art.picker.font_size();
        let cover = art_rect(
            inner,
            u32::from(font.width.max(1)),
            u32::from(font.height.max(1)),
        );
        art.render(frame, cover, Slot::Info);
        let left = cover.right() + GAP;
        text = Rect {
            x: left,
            width: inner.right().saturating_sub(left),
            ..inner
        };
    }
    frame.render_widget(Paragraph::new(details.lines(usize::from(text.width))), text);
}

/// A pixel-square area at the top left of `inner`, as tall as the body and
/// at most two fifths of its width.
fn art_rect(inner: Rect, cell_w: u32, cell_h: u32) -> Rect {
    let max_w = u32::from(inner.width) * 2 / 5;
    let mut h = u32::from(inner.height);
    let mut w = h * cell_h / cell_w;
    if w > max_w {
        w = max_w;
        h = w * cell_w / cell_h;
    }
    let w = u16::try_from(w).unwrap_or(inner.width);
    let h = u16::try_from(h).unwrap_or(inner.height);
    Rect::new(inner.x, inner.y, w, h)
}

/// The text side of the popup.
struct Details {
    title: String,
    subtitle: Option<String>,
    rows: Vec<(&'static str, String)>,
}

impl Details {
    fn of(info: &Info) -> Self {
        match info {
            Info::Song(s) => Self::song(s),
            Info::Album(a) => {
                let mut rows = Vec::new();
                push(&mut rows, "Year", a.year.map(|y| y.to_string()));
                push(&mut rows, "Genre", a.genre.clone());
                if a.song_count > 0 {
                    rows.push(("Songs", songs_label(a.song_count as usize)));
                }
                push(&mut rows, "Length", length(a.duration));
                push(&mut rows, "Plays", a.play_count.map(|n| n.to_string()));
                Self {
                    title: a.name.clone(),
                    subtitle: non_empty(&a.artist),
                    rows,
                }
            }
            Info::Artist(a) => {
                let albums = match a.album_count {
                    1 => "1 album".to_string(),
                    n => format!("{n} albums"),
                };
                Self {
                    title: a.name.clone(),
                    subtitle: None,
                    rows: vec![("Albums", albums)],
                }
            }
            Info::Playlist(p) => {
                let mut rows = Vec::new();
                push(&mut rows, "Owner", p.owner.clone());
                rows.push(("Songs", songs_label(p.song_count as usize)));
                push(&mut rows, "Length", length(p.duration));
                Self {
                    title: p.name.clone(),
                    subtitle: None,
                    rows,
                }
            }
        }
    }

    fn song(s: &Song) -> Self {
        let mut rows = Vec::new();
        push(&mut rows, "Album", non_empty(&s.album));
        push(&mut rows, "Year", s.year.map(|y| y.to_string()));
        let track = match (s.track, s.disc_number) {
            (Some(t), Some(d)) => Some(format!("{t} (disc {d})")),
            (Some(t), None) => Some(t.to_string()),
            (None, _) => None,
        };
        push(&mut rows, "Track", track);
        push(&mut rows, "Length", length(s.duration));
        push(&mut rows, "Genre", s.genre.clone());
        push(&mut rows, "Format", format_line(s));
        push(&mut rows, "Size", s.size.map(format_size));
        push(&mut rows, "Plays", s.play_count.map(|n| n.to_string()));
        let star = if s.starred { " ★" } else { "" };
        Self {
            title: format!("{}{star}", s.title),
            subtitle: non_empty(&s.artist),
            rows,
        }
    }

    fn line_count(&self) -> usize {
        // Title, subtitle, a blank line, then the rows.
        1 + usize::from(self.subtitle.is_some()) + 1 + self.rows.len()
    }

    fn lines(&self, width: usize) -> Vec<Line<'static>> {
        let mut lines = vec![Line::styled(
            truncate(&self.title, width),
            theme::accent_bold(),
        )];
        if let Some(sub) = &self.subtitle {
            lines.push(Line::raw(truncate(sub, width)));
        }
        lines.push(Line::raw(""));
        let room = width.saturating_sub(LABEL_WIDTH);
        for (label, value) in &self.rows {
            let pad = LABEL_WIDTH.saturating_sub(display_width(label));
            lines.push(Line::from(vec![
                Span::styled(format!("{label}{}", " ".repeat(pad)), theme::muted()),
                Span::raw(truncate(value, room)),
            ]));
        }
        lines
    }
}

fn push(rows: &mut Vec<(&'static str, String)>, label: &'static str, value: Option<String>) {
    if let Some(v) = value {
        rows.push((label, v));
    }
}

fn non_empty(s: &str) -> Option<String> {
    (!s.trim().is_empty()).then(|| s.to_string())
}

fn length(secs: u32) -> Option<String> {
    (secs > 0).then(|| format_duration(f64::from(secs)))
}

/// File type and quality, such as "FLAC · 24-bit · 96 kHz · 2304 kbps".
fn format_line(s: &Song) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(suffix) = s.suffix.as_deref().filter(|x| !x.is_empty()) {
        parts.push(suffix.to_uppercase());
    }
    if let Some(depth) = s.bit_depth.filter(|d| *d > 0) {
        parts.push(format!("{depth}-bit"));
    }
    if let Some(rate) = s.sampling_rate.filter(|r| *r > 0) {
        parts.push(format_rate(rate));
    }
    if let Some(kbps) = s.bit_rate.filter(|b| *b > 0) {
        parts.push(format!("{kbps} kbps"));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn format_rate(hz: u32) -> String {
    if hz.is_multiple_of(1000) {
        format!("{} kHz", hz / 1000)
    } else {
        format!("{:.1} kHz", f64::from(hz) / 1000.0)
    }
}

/// Decimal megabytes with one place, or kilobytes under one megabyte.
fn format_size(bytes: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "file sizes are far below 2^52 bytes"
    )]
    let b = bytes as f64;
    if bytes >= 1_000_000 {
        format!("{:.1} MB", b / 1_000_000.0)
    } else {
        format!("{:.0} KB", b / 1_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subsonic::{Album, Artist, Playlist};
    use crate::ui::testing::buffer_text;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn song() -> Song {
        Song {
            id: "s1".into(),
            title: "Kataomoi".into(),
            artist: "Aimer".into(),
            album: "Penny Rain".into(),
            track: Some(1),
            disc_number: Some(1),
            year: Some(2019),
            duration: 250,
            genre: Some("J-Pop".into()),
            suffix: Some("flac".into()),
            bit_depth: Some(24),
            sampling_rate: Some(96_000),
            bit_rate: Some(2304),
            size: Some(31_457_280),
            play_count: Some(12),
            starred: true,
            cover_art: Some("al-1".into()),
            ..Default::default()
        }
    }

    fn render(info: &Info, art: &mut ArtState, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| {
            art.begin_frame(true);
            draw(f, f.area(), info, art);
            art.end_frame();
        })
        .unwrap();
        buffer_text(term.backend().buffer())
    }

    #[test]
    fn a_track_shows_its_details() {
        let text = render(&Info::Song(song()), &mut ArtState::default(), 100, 30);
        for want in [
            " Track ",
            "Kataomoi ★",
            "Aimer",
            "Penny Rain",
            "2019",
            "1 (disc 1)",
            "4:10",
            "J-Pop",
            "FLAC · 24-bit · 96 kHz · 2304 kbps",
            "31.5 MB",
            "Plays",
            "i close",
        ] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    #[test]
    fn missing_values_leave_their_rows_out() {
        let bare = Song {
            title: "Untitled".into(),
            ..Default::default()
        };
        let text = render(&Info::Song(bare), &mut ArtState::default(), 100, 30);
        assert!(text.contains("Untitled"), "{text}");
        // Labels are padded, which sets them apart from the " Track " title.
        for absent in ["Album", "Year", "Track", "Format", "Size", "Plays"] {
            let label = format!("{absent:<LABEL_WIDTH$}");
            assert!(!text.contains(&label), "{label:?} in\n{text}");
        }
    }

    #[test]
    fn albums_artists_and_playlists_have_their_own_rows() {
        let album = Info::Album(Album {
            name: "Penny Rain".into(),
            artist: "Aimer".into(),
            year: Some(2019),
            song_count: 12,
            duration: 3130,
            ..Default::default()
        });
        let text = render(&album, &mut ArtState::default(), 100, 30);
        for want in [" Album ", "Penny Rain", "Aimer", "12 songs", "52:10"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }

        let artist = Info::Artist(Artist {
            name: "Aimer".into(),
            album_count: 1,
            ..Default::default()
        });
        let text = render(&artist, &mut ArtState::default(), 100, 30);
        assert!(
            text.contains(" Artist ") && text.contains("1 album"),
            "{text}"
        );

        let playlist = Info::Playlist(Playlist {
            name: "Night Drive".into(),
            song_count: 3,
            owner: Some("alice".into()),
            ..Default::default()
        });
        let text = render(&playlist, &mut ArtState::default(), 100, 30);
        for want in [" Playlist ", "Night Drive", "alice", "3 songs"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    #[test]
    fn the_cover_draws_even_though_a_popup_is_open() {
        let mut art = ArtState::default();
        art.expect_info("al-1".into());
        let img: image::DynamicImage = image::RgbImage::from_fn(40, 40, |x, y| {
            image::Rgb([
                u8::try_from(x * 6).unwrap(),
                u8::try_from(y * 6).unwrap(),
                90,
            ])
        })
        .into();
        art.set_image("al-1", img);
        let text = render(&Info::Song(song()), &mut art, 100, 30);
        assert!(text.contains(['▀', '▄']), "{text}");
    }

    #[test]
    fn narrow_terminals_drop_the_cover_and_tiny_ones_do_not_panic() {
        let text = render(&Info::Song(song()), &mut ArtState::default(), 40, 20);
        assert!(!text.contains('♪'), "{text}");
        assert!(text.contains("Kataomoi"), "{text}");
        for (w, h) in [(0, 0), (1, 1), (5, 3), (12, 6), (200, 4)] {
            render(&Info::Song(song()), &mut ArtState::default(), w, h);
        }
    }

    #[test]
    fn rates_and_sizes_read_naturally() {
        assert_eq!(format_rate(44_100), "44.1 kHz");
        assert_eq!(format_rate(48_000), "48 kHz");
        assert_eq!(format_size(512_000), "512 KB");
        assert_eq!(format_size(8_400_000), "8.4 MB");
    }
}

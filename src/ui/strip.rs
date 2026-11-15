use super::art::Slot;
use super::theme;
use super::widgets::format_duration;
use crate::app::App;
use crate::player::queue::RepeatMode;
use crate::player::{PlayerSnapshot, Status};
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Rows the strip occupies at the bottom of the screen.
pub const HEIGHT: u16 = 3;
/// Columns reserved for cover art.
pub const ART_WIDTH: u16 = 6;
/// Widest the progress line and the rest of the strip's text get, so they
/// stay together on wide terminals instead of stretching edge to edge.
pub const MAX_PROGRESS_WIDTH: u16 = 80;

pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    // On wide terminals the art, bar, and indicators sit together in the
    // middle instead of hugging the left edge.
    let content = ART_WIDTH + 1 + MAX_PROGRESS_WIDTH;
    let [area] = Layout::horizontal([Constraint::Length(content.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [art, _, info] = Layout::horizontal([
        Constraint::Length(ART_WIDTH),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);
    app.art.render(frame, art, Slot::Strip);
    // The text, bar, and indicators share one width, so the indicators sit
    // at the end of the bar instead of the far edge of a wide window.
    let info = Rect {
        width: info.width.min(MAX_PROGRESS_WIDTH),
        ..info
    };

    let [track, progress, footer] = Layout::vertical([Constraint::Length(1); 3]).areas(info);
    frame.render_widget(Paragraph::new(track_line(&app.player)), track);
    frame.render_widget(
        Paragraph::new(progress_line(&app.player, progress.width)),
        progress,
    );
    draw_footer(frame, footer, app);
}

fn track_line(snap: &PlayerSnapshot) -> Line<'static> {
    let Some(song) = snap
        .current
        .and_then(|i| snap.items.get(i))
        .map(|item| &item.song)
    else {
        return Line::styled("Nothing playing", theme::dim());
    };
    let mut spans = vec![Span::styled(song.title.clone(), theme::accent_bold())];
    for part in [&song.artist, &song.album] {
        if !part.is_empty() {
            spans.push(Span::styled(" · ", theme::dim()));
            spans.push(Span::styled(part.clone(), theme::muted()));
        }
    }
    if song.starred {
        spans.push(Span::styled(" ♥", theme::accent()));
    }
    Line::from(spans)
}

/// The play state, times, and bar, at most `MAX_PROGRESS_WIDTH` wide.
pub(super) fn progress_line(snap: &PlayerSnapshot, width: u16) -> Line<'static> {
    let width = width.min(MAX_PROGRESS_WIDTH);
    let icon = match snap.status {
        Status::Playing => "▶",
        Status::Paused => "⏸",
        Status::Stopped => "■",
    };
    let song_secs = snap
        .current
        .and_then(|i| snap.items.get(i))
        .map_or(0.0, |item| f64::from(item.song.duration));
    let duration = if snap.duration > 0.0 {
        snap.duration
    } else {
        song_secs
    };
    let elapsed = format_duration(snap.position);
    let total = format_duration(duration);

    // Icon, elapsed, and total, each followed or preceded by one space.
    let fixed = 1 + 1 + elapsed.chars().count() + 1 + 1 + total.chars().count();
    let gauge = usize::from(width).saturating_sub(fixed);
    let ratio = if duration > 0.0 && snap.position.is_finite() {
        (snap.position / duration).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let gauge_cells = f64::from(u16::try_from(gauge).unwrap_or(u16::MAX));
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "ratio is clamped to 0..=1, so the result is within 0..=gauge"
    )]
    let filled = (gauge_cells * ratio).round() as usize;

    Line::from(vec![
        Span::styled(icon, theme::accent()),
        Span::raw(" "),
        Span::raw(elapsed),
        Span::raw(" "),
        Span::styled("━".repeat(filled), theme::accent()),
        Span::styled("─".repeat(gauge - filled.min(gauge)), theme::dim()),
        Span::raw(" "),
        Span::styled(total, theme::muted()),
    ])
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let right = modes_line(&app.player);
    let [left_area, right_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(u16::try_from(right.width()).unwrap_or(u16::MAX)),
    ])
    .areas(area);

    let left = match app.status_text() {
        Some(msg) => Line::styled(msg.to_owned(), theme::accent()),
        None => Line::from(vec![
            Span::styled("z ", theme::muted()),
            Span::styled("Now Playing", theme::dim()),
        ]),
    };
    frame.render_widget(Paragraph::new(left), left_area);
    frame.render_widget(Paragraph::new(right).right_aligned(), right_area);
}

pub(super) fn modes_line(snap: &PlayerSnapshot) -> Line<'static> {
    let mut labels = Vec::new();
    if snap.shuffle {
        labels.push("shuffle");
    }
    match snap.repeat {
        RepeatMode::Off => {}
        RepeatMode::All => labels.push("repeat all"),
        RepeatMode::One => labels.push("repeat one"),
    }
    let mut spans = Vec::new();
    for label in labels {
        spans.push(Span::styled(label, theme::accent()));
        spans.push(Span::styled(" · ", theme::dim()));
    }
    spans.push(Span::styled(
        format!("vol {}%", snap.volume),
        theme::muted(),
    ));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> PlayerSnapshot {
        PlayerSnapshot {
            status: Status::Playing,
            position: 30.0,
            duration: 120.0,
            ..Default::default()
        }
    }

    #[test]
    fn strip_fills_narrow_terminals() {
        use crate::ui::testing::buffer_text;
        let mut app = App::new();
        app.player = snap();
        let mut term =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, HEIGHT)).unwrap();
        term.draw(|f| draw(f, f.area(), &mut app)).unwrap();
        let text = buffer_text(term.backend().buffer());
        let rows: Vec<&str> = text.lines().collect();
        let col = |row: &str, needle: &str| row.chars().position(|c| needle.starts_with(c));
        // Art from column 0, a one-column gap, then the text.
        let text_col = usize::from(ART_WIDTH) + 1;
        assert_eq!(col(rows[0], "Nothing"), Some(text_col), "{text}");
        assert_eq!(col(rows[1], "▶"), Some(text_col), "{text}");
        // The volume ends at the right edge, and no tabs are drawn.
        assert!(rows[2].ends_with("vol 0%"), "{text}");
        assert!(!rows[2].contains("Library"), "{text}");
    }

    #[test]
    fn strip_is_centered_on_wide_terminals() {
        use crate::ui::testing::buffer_text;
        let mut app = App::new();
        app.player = snap();
        let mut term =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, HEIGHT)).unwrap();
        term.draw(|f| draw(f, f.area(), &mut app)).unwrap();
        let text = buffer_text(term.backend().buffer());
        let rows: Vec<Vec<char>> = text.lines().map(|r| r.chars().collect()).collect();
        let content = usize::from(ART_WIDTH + 1 + MAX_PROGRESS_WIDTH);
        let start = rows[1].iter().position(|c| *c == '▶').unwrap() - usize::from(ART_WIDTH) - 1;
        let end = start + content;
        // The bar and the volume both end at the right of the content.
        assert_eq!(rows[1][end - 1], '0', "{text}");
        assert_eq!(rows[2][end - 1], '%', "{text}");
        // Equal space on both sides, give or take a column.
        assert!(start.abs_diff(200 - end) <= 1, "{start} {end}\n{text}");
    }

    #[test]
    fn modes_are_words() {
        let text = |snap: &PlayerSnapshot| {
            modes_line(snap)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        let mut s = snap();
        s.volume = 80;
        assert_eq!(text(&s), "vol 80%");
        s.shuffle = true;
        s.repeat = RepeatMode::All;
        assert_eq!(text(&s), "shuffle · repeat all · vol 80%");
        s.shuffle = false;
        s.repeat = RepeatMode::One;
        assert_eq!(text(&s), "repeat one · vol 80%");
    }

    #[test]
    fn progress_line_fills_narrow_widths_and_caps_wide_ones() {
        assert_eq!(progress_line(&snap(), 40).width(), 40);
        assert_eq!(
            progress_line(&snap(), MAX_PROGRESS_WIDTH).width(),
            usize::from(MAX_PROGRESS_WIDTH)
        );
        assert_eq!(
            progress_line(&snap(), 300).width(),
            usize::from(MAX_PROGRESS_WIDTH)
        );
    }
}

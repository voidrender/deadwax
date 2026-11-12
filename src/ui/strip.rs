use super::art::Slot;
use super::theme;
use super::widgets::format_duration;
use crate::app::{App, View};
use crate::player::queue::RepeatMode;
use crate::player::{PlayerSnapshot, Status};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Rows the strip occupies at the bottom of the screen.
pub const HEIGHT: u16 = 3;
/// Columns reserved for cover art.
pub const ART_WIDTH: u16 = 6;

pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    let [art, _, info] = Layout::horizontal([
        Constraint::Length(ART_WIDTH),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);
    app.art.render(frame, art, Slot::Strip);

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

pub(super) fn progress_line(snap: &PlayerSnapshot, width: u16) -> Line<'static> {
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
    let filled = ((gauge as f64) * ratio).round() as usize;

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
        None => tabs_line(app.view, app.now_playing),
    };
    frame.render_widget(Paragraph::new(left), left_area);
    frame.render_widget(Paragraph::new(right).right_aligned(), right_area);
}

fn tabs_line(active: View, now_playing: bool) -> Line<'static> {
    let mut spans = Vec::new();
    for (n, view) in (1..).zip(View::ALL) {
        let style = if view == active && !now_playing {
            theme::selected()
        } else {
            theme::muted()
        };
        spans.push(Span::styled(format!(" {n} {} ", view.title()), style));
    }
    Line::from(spans)
}

fn modes_line(snap: &PlayerSnapshot) -> Line<'static> {
    let mut spans = Vec::new();
    if snap.shuffle {
        spans.push(Span::raw("🔀 "));
    }
    match snap.repeat {
        RepeatMode::Off => {}
        RepeatMode::All => spans.push(Span::raw("🔁 ")),
        RepeatMode::One => spans.push(Span::raw("🔂 ")),
    }
    spans.push(Span::styled(
        format!("vol {}%", snap.volume),
        theme::muted(),
    ));
    Line::from(spans)
}

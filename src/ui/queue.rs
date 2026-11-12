use super::library::fit;
use super::theme;
use super::widgets::{format_duration, pane};
use crate::app::App;
use crate::app::playlists::songs_label;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Width of the "▶ " marker column.
const MARK: usize = 2;

pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let items = &app.player.items;
    let block = pane(&format!("Queue · {}", songs_label(items.len())), true);
    let area = {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        inner
    };
    if area.is_empty() {
        return;
    }
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled("Queue is empty", theme::dim())),
            area,
        );
        return;
    }
    let height = usize::from(area.height);
    let width = usize::from(area.width);
    let sel = app.queue.sel.min(items.len() - 1);
    let offset = (sel + 1).saturating_sub(height);
    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, item)| {
            let current = app.player.current == Some(i);
            let mark = if current { "▶ " } else { "  " };
            let left = format!("{}  {}", item.song.title, item.song.artist);
            let right = format_duration(f64::from(item.song.duration));
            let text = fit(&left, &right, width.saturating_sub(MARK));
            let (mark_style, text_style) = if i == sel {
                (theme::selected(), theme::selected())
            } else {
                (theme::accent_bold(), ratatui::style::Style::new())
            };
            Line::from(vec![
                Span::styled(mark, mark_style),
                Span::styled(text, text_style),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::PlayerSnapshot;
    use crate::player::queue::QueueItem;
    use crate::subsonic::Song;
    use crate::ui::testing::buffer_text;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn item(uid: u64, title: &str) -> QueueItem {
        QueueItem {
            uid,
            song: Song {
                id: uid.to_string(),
                title: title.into(),
                artist: "Boards of Canada".into(),
                artist_id: None,
                album: "A".into(),
                album_id: None,
                track: None,
                year: None,
                duration: 151,
                cover_art: None,
                suffix: None,
                bit_rate: None,
                starred: false,
                ..Default::default()
            },
        }
    }

    fn render(app: &App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, f.area(), app)).unwrap();
        buffer_text(term.backend().buffer())
    }

    fn app_with(n: u64) -> App {
        let mut app = App::new();
        app.player = PlayerSnapshot {
            items: (0..n).map(|i| item(i, &format!("Song {i}"))).collect(),
            current: Some(1),
            ..Default::default()
        };
        app
    }

    #[test]
    fn current_is_marked() {
        let app = app_with(3);
        let text = render(&app, 60, 6);
        assert!(text.contains("Queue · 3 songs"), "{text}");
        let line = text.lines().find(|l| l.contains("Song 1")).unwrap();
        assert!(line.starts_with("│▶"), "{line:?}");
        let other = text.lines().find(|l| l.contains("Song 0")).unwrap();
        assert!(!other.contains('▶'), "{other:?}");
        assert!(line.contains("2:31") && line.contains("Boards of Canada"));
    }

    #[test]
    fn empty_queue_shows_a_hint() {
        let app = App::new();
        assert!(render(&app, 40, 5).contains("Queue is empty"));
    }

    #[test]
    fn long_lists_scroll_to_the_selection() {
        let mut app = app_with(50);
        app.queue.sel = 40;
        let text = render(&app, 40, 5);
        assert!(text.contains("Song 40"), "{text}");
        assert!(!text.contains("Song 0 "), "{text}");
    }

    #[test]
    fn tiny_areas_and_stale_selection_do_not_panic() {
        let mut app = app_with(5);
        app.queue.sel = 99;
        for (w, h) in [(0, 0), (1, 1), (3, 20), (2, 2), (40, 1)] {
            render(&app, w, h);
        }
    }
}

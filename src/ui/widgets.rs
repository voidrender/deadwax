use ratatui::layout::{Constraint, Flex, Layout, Rect};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Terminal columns `s` occupies.
pub fn display_width(s: &str) -> usize {
    s.width()
}

/// `s` cut to at most `room` columns, ending in `…` when cut. A wide char
/// that would straddle the boundary is dropped, so the result may be one
/// column short of `room`.
pub fn truncate(s: &str, room: usize) -> String {
    if s.width() <= room {
        return s.to_string();
    }
    if room == 0 {
        return String::new();
    }
    let budget = room - 1;
    let mut used = 0;
    let mut out = String::new();
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

/// The longest suffix of `s` that fits in `room` columns.
pub fn tail(s: &str, room: usize) -> &str {
    let mut used = 0;
    let mut start = s.len();
    for (i, c) in s.char_indices().rev() {
        let w = c.width().unwrap_or(0);
        if used + w > room {
            break;
        }
        used += w;
        start = i;
    }
    &s[start..]
}

/// Formats seconds as `m:ss`, or `h:mm:ss` from one hour up.
pub fn format_duration(secs: f64) -> String {
    let total = if secs.is_finite() && secs > 0.0 {
        secs as u64
    } else {
        0
    };
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A rect of at most `width` x `height` centered in `area`.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [cell] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(row);
    cell
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_and_tail_count_columns() {
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abcd", 3), "ab…");
        assert_eq!(truncate("旋律の", 5), "旋律…");
        assert_eq!(truncate("旋律の", 4), "旋…");
        assert_eq!(truncate("旋律の", 0), "");
        assert_eq!(tail("ab旋律", 4), "旋律");
        assert_eq!(tail("ab旋律", 3), "律");
        assert_eq!(tail("ab旋律", 0), "");
    }

    #[test]
    fn format_duration_cases() {
        assert_eq!(format_duration(0.0), "0:00");
        assert_eq!(format_duration(72.4), "1:12");
        assert_eq!(format_duration(3723.0), "1:02:03");
        assert_eq!(format_duration(-5.0), "0:00");
        assert_eq!(format_duration(f64::NAN), "0:00");
    }

    #[test]
    fn centered_fits_inside_small_areas() {
        let area = Rect::new(2, 3, 10, 4);
        let r = centered(area, 50, 20);
        assert!(area.contains(r.as_position()) || r.area() == 0);
        assert!(r.width <= area.width && r.height <= area.height);
    }
}

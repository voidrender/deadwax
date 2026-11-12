use ratatui::layout::{Constraint, Flex, Layout, Rect};

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

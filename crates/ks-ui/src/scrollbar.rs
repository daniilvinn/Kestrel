//! A one-cell vertical scrollbar thumb for the list and the preview. Only the thumb is drawn
//! (no track), and only when the content is taller than the viewport, so short directories
//! and files look exactly as before.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::theme::Theme;

const THUMB_GLYPH: &str = "┃";
const THUMB_ASCII: &str = "#";

/// Rows of the thumb within a viewport of `rows` rows showing `total` rows from `offset`:
/// `(first, len)`, or `None` when everything fits.
pub fn thumb(rows: usize, total: usize, offset: usize) -> Option<(usize, usize)> {
    if rows == 0 || total <= rows {
        return None;
    }
    let len = (rows * rows / total).clamp(1, rows);
    let travel = rows - len;
    let max_offset = total - rows;
    let offset = offset.min(max_offset);
    // Rounded, so the thumb touches the bottom exactly at the last offset.
    let first = (offset * travel + max_offset / 2) / max_offset;
    Some((first.min(travel), len))
}

/// Draws the thumb into column `x` of `area` (rows `area.top()..area.bottom()`).
pub fn render(
    buf: &mut Buffer,
    x: u16,
    area: Rect,
    total: usize,
    offset: usize,
    theme: &Theme,
    ascii: bool,
) {
    let Some((first, len)) = thumb(usize::from(area.height), total, offset) else {
        return;
    };
    if !buf.area.contains(ratatui::layout::Position::new(x, area.y)) {
        return;
    }
    let glyph = if ascii { THUMB_ASCII } else { THUMB_GLYPH };
    let style = theme.muted();
    for row in first..first + len {
        let y = area.y + u16::try_from(row).unwrap_or(u16::MAX);
        if y >= area.bottom() {
            break;
        }
        buf[(x, y)].set_symbol(glyph).set_style(style);
    }
}

#[cfg(test)]
mod tests {
    use super::thumb;

    #[test]
    fn no_thumb_when_everything_fits() {
        assert_eq!(thumb(10, 10, 0), None);
        assert_eq!(thumb(10, 3, 0), None);
        assert_eq!(thumb(0, 30, 0), None);
    }

    #[test]
    fn thumb_spans_top_to_bottom() {
        assert_eq!(thumb(10, 100, 0), Some((0, 1)));
        assert_eq!(thumb(10, 100, 90), Some((9, 1)));
        assert_eq!(
            thumb(10, 100, 500),
            Some((9, 1)),
            "offset past the end is clamped"
        );
        assert_eq!(thumb(10, 20, 0), Some((0, 5)));
        assert_eq!(thumb(10, 20, 10), Some((5, 5)));
        let (first, len) = thumb(10, 20, 5).unwrap();
        assert!(first > 0 && first < 5 && len == 5);
    }
}

//! Tab strip (header), bookmark list and frecency jump overlays.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use ks_core::config::IconSet;
use ks_core::{AppState, JumpHit, Overlay};

use crate::text::{sanitize, truncate_end, width};
use crate::theme::Theme;

const CURSOR_GLYPH: &str = "▌";

/// Longest tab name in the strip.
const TAB_NAME_MAX: usize = 16;
/// Below this, names are dropped and only the numbers remain.
const TAB_NAME_MIN: usize = 4;

/// `1 src ▏2 Kestrel ▏3 docs` with the active tab in the accent colour. Empty with a single
/// tab. Names shrink so the strip takes at most half the header, and disappear on a header too
/// narrow for them.
pub fn tab_strip(state: &AppState, theme: &Theme) -> Vec<Span<'static>> {
    let count = state.tabs.count();
    if count < 2 {
        return Vec::new();
    }
    let ascii = state.config.icons == IconSet::Ascii;
    let separator = if ascii { "|" } else { "▏" };
    // Per tab: the number, a space before the name and the separator.
    let budget = usize::from(state.size.width) / 2;
    let per_tab = (budget / count).saturating_sub(4).min(TAB_NAME_MAX);
    let names = per_tab >= TAB_NAME_MIN;
    let mut spans = Vec::new();
    for index in 0..count {
        if index > 0 {
            spans.push(Span::styled(format!(" {separator}"), theme.muted()));
        }
        let style = if index == state.tabs.active_index() {
            theme.bold_accent()
        } else {
            theme.dim()
        };
        spans.push(Span::styled((index + 1).to_string(), style));
        if names {
            let name = sanitize(&state.tabs.title(state, index));
            spans.push(Span::styled(
                format!(" {}", truncate_end(&name, per_tab)),
                style,
            ));
        }
        if state.tabs.is_stale(index) {
            // The tab's directory changed while it was in the background.
            let mark = if ascii { "*" } else { "•" };
            spans.push(Span::styled(mark, theme.muted()));
        }
    }
    spans
}

/// Draws the bookmark or jump overlay when one is on top of the overlay stack.
pub fn render_overlays(frame: &mut Frame, state: &AppState, theme: &Theme) {
    for overlay in &state.overlays {
        match overlay {
            Overlay::Bookmarks => render_bookmarks(frame, state, theme),
            Overlay::Jump => render_jump(frame, state, theme),
            _ => {}
        }
    }
}

/// The centred box of an overlay: `inner_rows` content rows, rounded border with `title`.
fn overlay_box(frame: &mut Frame, title: &str, inner_rows: u16, theme: &Theme) -> Option<Rect> {
    let area = frame.area();
    if area.width < 24 || area.height < 8 {
        return None;
    }
    let box_width = (area.width * 3 / 4).clamp(24, 100).min(area.width - 2);
    let box_height = (inner_rows + 2).min(area.height.saturating_sub(5)).max(3);
    let x = area.x + (area.width - box_width) / 2;
    let y = area.y + ((area.height - box_height) / 2).max(1);
    let rect = Rect::new(x, y, box_width, box_height);
    clear_straddling_wide_chars(frame, rect);
    frame.render_widget(Clear, rect);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border))
        .style(theme.base_style())
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                title.to_owned(),
                Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    Some(inner)
}

/// A wide character that starts just left of the box would claim the first cell of the box
/// (and ratatui would then skip drawing the border there): blank such cells first.
fn clear_straddling_wide_chars(frame: &mut Frame, rect: Rect) {
    if rect.x == 0 {
        return;
    }
    let buffer = frame.buffer_mut();
    for y in rect.y..rect.bottom() {
        let cell = &mut buffer[(rect.x - 1, y)];
        if width(cell.symbol()) > 1 {
            cell.set_symbol(" ");
        }
    }
}

fn render_bookmarks(frame: &mut Frame, state: &AppState, theme: &Theme) {
    let bookmarks = &state.aids.bookmarks;
    let rows = bookmarks.len().clamp(1, 14);
    let Some(inner) = overlay_box(frame, "Bookmarks", u16::try_from(rows).unwrap_or(14), theme)
    else {
        return;
    };
    if bookmarks.is_empty() {
        let text = "No bookmarks yet. Press m and a key to bookmark this directory.";
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_end(text, usize::from(inner.width).saturating_sub(2)),
                theme.dim(),
            ))
            .block(Block::new().padding(ratatui::widgets::Padding::left(1))),
            inner,
        );
        return;
    }
    let cursor = state.aids.bookmark_cursor;
    let visible = usize::from(inner.height);
    let first = cursor.saturating_sub(visible.saturating_sub(1));
    for (row, (key, path)) in bookmarks.iter().enumerate().skip(first).take(visible) {
        let selected = row == cursor;
        let y = inner.y + u16::try_from(row - first).unwrap_or(0);
        let area = Rect::new(inner.x, y, inner.width, 1);
        let shown = ks_core::aids::display_path(path, state.home.as_deref());
        let budget = usize::from(inner.width).saturating_sub(6);
        let mut spans = vec![
            cursor_marker(selected, theme),
            Span::styled(format!(" {key} "), theme.bold_accent()),
            Span::styled(
                format!(
                    " {}",
                    crate::text::truncate_start(&sanitize(&shown), budget)
                ),
                Style::new().fg(theme.fg),
            ),
        ];
        pad_row(&mut spans, inner.width);
        render_row(frame, area, spans, selected, theme);
    }
}

fn cursor_marker(selected: bool, theme: &Theme) -> Span<'static> {
    if selected {
        Span::styled(CURSOR_GLYPH, Style::new().fg(theme.accent))
    } else {
        Span::raw(" ")
    }
}

/// Pads `spans` with spaces up to `target` cells so a selection background fills the row.
fn pad_row(spans: &mut Vec<Span<'static>>, target: u16) {
    let used: usize = spans.iter().map(|s| width(&s.content)).sum();
    let missing = usize::from(target).saturating_sub(used);
    if missing > 0 {
        spans.push(Span::raw(" ".repeat(missing)));
    }
}

fn render_row(
    frame: &mut Frame,
    area: Rect,
    spans: Vec<Span<'static>>,
    selected: bool,
    theme: &Theme,
) {
    let mut line = Line::from(spans);
    if selected {
        line = line.style(Style::new().bg(theme.selection_bg));
    }
    frame.render_widget(Paragraph::new(line), area);
}

fn render_jump(frame: &mut Frame, state: &AppState, theme: &Theme) {
    let jump = &state.aids.jump;
    let area = frame.area();
    let list_rows = (area.height / 2).clamp(6, 14);
    let Some(inner) = overlay_box(frame, "Jump to directory", list_rows + 2, theme) else {
        return;
    };
    if inner.height < 3 {
        return;
    }

    // Input line: `› query` with the counter on the right.
    let counter = if jump.input.trim().is_empty() {
        format!("{}", jump.total)
    } else {
        format!("{} / {}", jump.hits.len(), jump.total)
    };
    let counter_width = width(&counter) + 1;
    let input_budget = usize::from(inner.width).saturating_sub(counter_width + 4);
    let shown_input = crate::text::truncate_start(&sanitize(&jump.input), input_budget);
    let input = Line::from(vec![
        Span::raw(" "),
        Span::styled("›", theme.bold_accent()),
        Span::raw(" "),
        Span::styled(shown_input, Style::new().fg(theme.fg)),
        Span::styled(" ", Style::new().add_modifier(Modifier::REVERSED)),
    ]);
    frame.render_widget(
        Paragraph::new(input),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Span::styled(counter, theme.muted())).alignment(Alignment::Right),
        Rect::new(inner.x, inner.y, inner.width.saturating_sub(1), 1),
    );
    let rule = "─".repeat(usize::from(inner.width));
    frame.render_widget(
        Paragraph::new(Span::styled(rule, Style::new().fg(theme.border))),
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
    );

    let rows = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 2);
    if jump.hits.is_empty() {
        let text = if jump.input.trim().is_empty() {
            "Nothing here yet. Directories you visit show up here."
        } else {
            "No match"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_end(text, usize::from(rows.width).saturating_sub(2)),
                theme.dim(),
            ))
            .block(Block::new().padding(ratatui::widgets::Padding::left(1))),
            rows,
        );
        return;
    }
    let visible = usize::from(rows.height);
    let first = jump.cursor.saturating_sub(visible.saturating_sub(1));
    for (row, hit) in jump.hits.iter().enumerate().skip(first).take(visible) {
        let selected = row == jump.cursor;
        let y = rows.y + u16::try_from(row - first).unwrap_or(0);
        let mut spans = vec![cursor_marker(selected, theme), Span::raw(" ")];
        spans.extend(label_spans(
            hit,
            usize::from(rows.width).saturating_sub(3),
            theme,
        ));
        pad_row(&mut spans, rows.width);
        render_row(
            frame,
            Rect::new(rows.x, y, rows.width, 1),
            spans,
            selected,
            theme,
        );
    }
}

/// The path with the matched characters in accent + bold; a path that does not fit loses
/// its start (the end is what tells directories apart).
fn label_spans(hit: &JumpHit, budget: usize, theme: &Theme) -> Vec<Span<'static>> {
    let chars: Vec<char> = hit.label.chars().collect();
    let total: usize = chars.iter().map(|c| c.width().unwrap_or(0)).sum();
    let mut skip = 0;
    let mut prefix_ellipsis = false;
    if total > budget {
        // Drop leading characters until the rest plus the ellipsis fits.
        let mut remaining = total + 1;
        while skip < chars.len() && remaining > budget {
            remaining -= chars[skip].width().unwrap_or(0);
            skip += 1;
        }
        prefix_ellipsis = true;
    }
    let normal = Style::new().fg(theme.fg);
    let matched = Style::new().fg(theme.match_).add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    if prefix_ellipsis {
        spans.push(Span::styled("…", theme.dim()));
    }
    let mut run = String::new();
    let mut run_matched = false;
    for (index, c) in chars.iter().enumerate().skip(skip) {
        let is_match = hit
            .matched
            .binary_search(&u32::try_from(index).unwrap_or(u32::MAX))
            .is_ok();
        if is_match != run_matched && !run.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if run_matched { matched } else { normal },
            ));
        }
        run_matched = is_match;
        run.push(if ks_search::is_unsafe_char(*c) {
            '?'
        } else {
            *c
        });
    }
    if !run.is_empty() {
        spans.push(Span::styled(
            run,
            if run_matched { matched } else { normal },
        ));
    }
    spans
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use ks_core::{update, Effect, KeyChord, Msg};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::render;
    use crate::testutil::{dump, fixture_entries, listing_state};
    use crate::theme::{ColorDepth, Theme};

    const SIZES: [(u16, u16); 2] = [(80, 24), (160, 48)];
    const THEMES: [&str; 2] = ["ember", "paper"];

    fn theme(name: &str) -> Theme {
        Theme::builtin(name, ColorDepth::TrueColor).unwrap()
    }

    #[test]
    fn jump_labels_neutralise_bidi_overrides() {
        let hit = JumpHit {
            path: PathBuf::from("/x"),
            label: "~/invoice\u{202E}fdp.exe".to_owned(),
            matched: vec![],
            score: 0.0,
        };
        let spans = label_spans(&hit, 60, &theme("ember"));
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "~/invoice?fdp.exe");
    }

    fn draw(state: &AppState, theme: &Theme) -> String {
        let mut terminal =
            Terminal::new(TestBackend::new(state.size.width, state.size.height)).unwrap();
        terminal.draw(|f| render(f, state, theme)).unwrap();
        dump(terminal.backend().buffer())
    }

    /// Sends the key and plays the file system for any listing it starts.
    fn press(state: &mut AppState, keys: &str) {
        for part in keys.split(' ') {
            let mut queue = update(state, Msg::Key(part.parse::<KeyChord>().unwrap()));
            while let Some(effect) = queue.pop() {
                if let Effect::ListDir { gen, .. } = effect {
                    update(
                        state,
                        Msg::ListingChunk {
                            gen,
                            entries: fixture_entries(),
                        },
                    );
                    queue.extend(update(state, Msg::ListingFinished { gen, error: None }));
                }
            }
        }
    }

    fn text(state: &AppState) -> String {
        draw(state, &theme("ember"))
            .split("--- styles")
            .next()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn tab_strip_snapshots() {
        for name in THEMES {
            for (w, h) in SIZES {
                let mut s = listing_state(w, h);
                press(&mut s, "t t 2");
                assert_eq!(s.tabs.count(), 3);
                insta::assert_snapshot!(format!("tabs_{name}_{w}x{h}"), draw(&s, &theme(name)));
            }
        }
    }

    #[test]
    fn tab_strip_next_to_the_git_summary_snapshots() {
        for name in THEMES {
            for (w, h) in SIZES {
                let mut s = listing_state(w, h);
                press(&mut s, "t t 2");
                s.git = Some(crate::testutil::fixture_git());
                insta::assert_snapshot!(format!("tabs_git_{name}_{w}x{h}"), draw(&s, &theme(name)));
            }
        }
        // Both stay readable when the header is tight.
        let mut s = listing_state(40, 12);
        press(&mut s, "t t 2");
        s.git = Some(crate::testutil::fixture_git());
        let header = text(&s).lines().next().unwrap().to_owned();
        assert!(header.contains("main"), "{header}");
        assert!(header.contains('1') && header.contains('3'), "{header}");
    }

    #[test]
    fn pending_bookmark_prefix_snapshots() {
        for prefix in ["m", "'"] {
            let tag = if prefix == "m" { "set" } else { "jump" };
            for name in THEMES {
                for (w, h) in SIZES {
                    let mut s = bookmarks_state(w, h);
                    press(&mut s, prefix);
                    s.which_key_visible = true;
                    insta::assert_snapshot!(
                        format!("pending_{tag}_{name}_{w}x{h}"),
                        draw(&s, &theme(name))
                    );
                }
            }
        }
    }

    #[test]
    fn stale_tab_marker_snapshots() {
        for name in THEMES {
            for (w, h) in SIZES {
                let mut s = listing_state(w, h);
                press(&mut s, "t t 2");
                let path = s.cwd.clone();
                update(&mut s, Msg::ParkedDirChanged { path });
                assert!(s.tabs.is_stale(0));
                insta::assert_snapshot!(
                    format!("tabs_stale_{name}_{w}x{h}"),
                    draw(&s, &theme(name))
                );
            }
        }
    }

    #[test]
    fn a_parked_tab_that_changed_is_marked_in_the_strip() {
        let mut s = listing_state(80, 24);
        press(&mut s, "t t 2");
        let plain: String = tab_strip(&s, &theme("ember"))
            .iter()
            .map(|sp| sp.content.to_string())
            .collect();
        assert!(!plain.contains('•'), "{plain}");
        // Tabs 1 and 3 are parked in the same directory as the active tab 2.
        let path = s.cwd.clone();
        update(&mut s, Msg::ParkedDirChanged { path });
        assert!(s.tabs.is_stale(0) && s.tabs.is_stale(2) && !s.tabs.is_stale(1));
        let marked: String = tab_strip(&s, &theme("ember"))
            .iter()
            .map(|sp| sp.content.to_string())
            .collect();
        assert!(marked.contains('•'), "{marked}");
        let mut ascii = s;
        ascii.config.icons = IconSet::Ascii;
        let marked: String = tab_strip(&ascii, &theme("ember"))
            .iter()
            .map(|sp| sp.content.to_string())
            .collect();
        assert!(marked.contains('*') && !marked.contains('•'), "{marked}");
    }

    #[test]
    fn a_single_tab_shows_no_strip() {
        let s = listing_state(80, 24);
        assert!(tab_strip(&s, &theme("ember")).is_empty());
    }

    #[test]
    fn the_strip_marks_the_active_tab_with_the_accent() {
        let mut s = listing_state(80, 24);
        press(&mut s, "t t 2");
        let t = theme("ember");
        let spans = tab_strip(&s, &t);
        let digits: Vec<_> = spans
            .iter()
            .filter(|sp| sp.content.chars().all(|c| c.is_ascii_digit()))
            .collect();
        assert_eq!(digits.len(), 3);
        assert_eq!(digits[1].style.fg, Some(t.accent));
        assert_ne!(digits[0].style.fg, Some(t.accent));
        assert_ne!(digits[2].style.fg, Some(t.accent));
    }

    #[test]
    fn tabs_are_named_after_their_directory() {
        let mut s = listing_state(120, 30);
        press(&mut s, "t");
        let plain: String = tab_strip(&s, &theme("ember"))
            .iter()
            .map(|sp| sp.content.to_string())
            .collect();
        assert_eq!(plain, "1 renderer ▏2 renderer");
        assert_eq!(crate::render::window_title(&s), "renderer — Kestrel");
    }

    #[test]
    fn the_strip_survives_a_narrow_header_and_ascii_icons() {
        let mut s = listing_state(30, 12);
        press(&mut s, "t");
        s.config.icons = IconSet::Ascii;
        let out = text(&s);
        assert!(out.lines().next().unwrap().contains("1 |2"), "{out}");
    }

    fn bookmarks_state(w: u16, h: u16) -> AppState {
        let mut s = listing_state(w, h);
        let b = &mut s.aids.bookmarks;
        b.set('c', PathBuf::from("/home/me/code/kestrel"));
        b.set('d', PathBuf::from("/home/me/Downloads"));
        b.set(
            'w',
            PathBuf::from(
                "/mnt/work/a/very/long/path/that/will/not/fit/in/the/box/at/all/ever/projects",
            ),
        );
        b.set('é', PathBuf::from("/home/me/Dokumente"));
        s
    }

    #[test]
    fn bookmark_overlay_snapshots() {
        for name in THEMES {
            for (w, h) in SIZES {
                let mut s = bookmarks_state(w, h);
                press(&mut s, "M j");
                insta::assert_snapshot!(
                    format!("bookmarks_{name}_{w}x{h}"),
                    draw(&s, &theme(name))
                );
                let mut empty = listing_state(w, h);
                press(&mut empty, "M");
                insta::assert_snapshot!(
                    format!("bookmarks_empty_{name}_{w}x{h}"),
                    draw(&empty, &theme(name))
                );
            }
        }
    }

    fn jump_state(w: u16, h: u16) -> AppState {
        let mut s = listing_state(w, h);
        let db = &mut s.aids.frecency;
        let t = 1_741_953_600;
        for (path, visits) in [
            ("/home/me/code/kestrel", 9),
            ("/home/me/code/kestrel/crates/ks-core", 4),
            ("/home/me/Downloads", 6),
            ("/home/me/projects/omniforce/src/renderer", 3),
            (
                "/mnt/work/a/very/long/path/that/will/not/fit/in/the/box/at/all/ever/projects",
                2,
            ),
            ("D:/Games/日本語のフォルダ", 1),
        ] {
            for _ in 0..visits {
                db.visit(Path::new(path), t);
            }
        }
        s
    }

    #[test]
    fn jump_overlay_snapshots() {
        for name in THEMES {
            for (w, h) in SIZES {
                let mut s = jump_state(w, h);
                press(&mut s, "z");
                insta::assert_snapshot!(format!("jump_{name}_{w}x{h}"), draw(&s, &theme(name)));
                press(&mut s, "k e down");
                insta::assert_snapshot!(
                    format!("jump_query_{name}_{w}x{h}"),
                    draw(&s, &theme(name))
                );
                press(&mut s, "x x x");
                insta::assert_snapshot!(
                    format!("jump_nomatch_{name}_{w}x{h}"),
                    draw(&s, &theme(name))
                );
                let mut empty = listing_state(w, h);
                press(&mut empty, "z");
                insta::assert_snapshot!(
                    format!("jump_empty_{name}_{w}x{h}"),
                    draw(&empty, &theme(name))
                );
            }
        }
    }

    #[test]
    fn matched_characters_are_accented_and_bold() {
        let mut s = jump_state(100, 30);
        press(&mut s, "z d o w");
        let t = theme("ember");
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| render(f, &s, &t)).unwrap();
        let buffer = terminal.backend().buffer();
        let found = (0..buffer.area.height).any(|y| {
            (0..buffer.area.width).any(|x| {
                let cell = &buffer[(x, y)];
                cell.symbol() == "D"
                    && cell.fg == t.match_
                    && cell.modifier.contains(Modifier::BOLD)
            })
        });
        assert!(found, "{}", text(&s));
    }

    #[test]
    fn long_paths_lose_their_start_and_keep_the_matched_end() {
        let mut s = jump_state(80, 24);
        press(&mut s, "z p r o j e c t s");
        let out = text(&s);
        let long = out
            .lines()
            .find(|l| l.contains("…"))
            .expect("an elided row");
        assert!(long.contains("ever/projects"), "{long}");
    }

    #[test]
    fn overlays_degrade_on_tiny_terminals() {
        for (w, h) in [(10, 5), (24, 8), (30, 9), (0, 0)] {
            let mut s = jump_state(80, 24);
            press(&mut s, "z");
            s.size = ks_core::Size::new(w, h);
            let _ = draw(&s, &theme("ember"));
            let mut b = bookmarks_state(80, 24);
            press(&mut b, "M");
            b.size = ks_core::Size::new(w, h);
            let _ = draw(&b, &theme("paper"));
        }
    }
}

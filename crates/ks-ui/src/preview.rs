//! The body of the main screen when more than the list is shown: optional parent column,
//! the list, and the preview pane.
//!
//! Pane geometry comes from `ks-core` ([`AppState::pane_widths_at`]) so `update` and the
//! renderer agree on it. This module only draws; the engine results are plain data in
//! `state.preview`.

use ks_core::config::{IconSet, SizeBase};
use ks_core::preview::{hex_bytes_per_row, HEADER_ROWS};
use ks_core::{AppState, Attrs, Entry, EntryKind};
use ks_preview::{
    ArchivePreview, BinaryPreview, Color as PreviewColor, DirEntryInfo, DirPreview, FileMetadata,
    HexRow, Preview, PreviewBody, Role, SpanStyle, StyledLine, TextPreview,
};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::format::{absolute_time, human_size, relative_time};
use crate::icons::icon_for;
use crate::list::render_list;
use crate::text::{sanitize, truncate_end, truncate_middle, width};
use crate::theme::Theme;

const DIVIDER_GLYPH: &str = "│";
const RULE_GLYPH: &str = "─";
const CURSOR_GLYPH: &str = "▌";

/// Draws the parent column, the list and the preview into `area` (the space between the
/// header rule and the bottom rule).
pub fn render_body(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let widths = state.pane_widths_at(area.width);
    let mut x = area.x;
    if widths.has_parent() {
        let pane = Rect::new(x, area.y, widths.parent, area.height);
        render_parent(frame, pane, state, theme);
        x += widths.parent;
        render_divider(frame, Rect::new(x, area.y, 1, area.height), theme);
        x += 1;
    }
    render_list(
        frame,
        Rect::new(x, area.y, widths.list, area.height),
        state,
        theme,
    );
    x += widths.list;
    if widths.has_preview() {
        render_divider(frame, Rect::new(x, area.y, 1, area.height), theme);
        x += 1;
        render_preview(
            frame,
            Rect::new(x, area.y, widths.preview, area.height),
            state,
            theme,
        );
    }
}

fn render_divider(frame: &mut Frame, area: Rect, theme: &Theme) {
    let style = Style::new().fg(theme.border);
    let buf = frame.buffer_mut();
    for y in area.top()..area.bottom() {
        buf[(area.x, y)].set_symbol(DIVIDER_GLYPH).set_style(style);
    }
}

// ----- style mapping -----

fn role_color(role: Role, theme: &Theme) -> ratatui::style::Color {
    match role {
        Role::Fg => theme.fg,
        Role::FgDim => theme.fg_dim,
        Role::FgMuted => theme.fg_muted,
        Role::Accent => theme.accent,
        Role::Dir => theme.dir,
        Role::Exec => theme.exec,
        Role::Symlink => theme.symlink,
        Role::Hidden => theme.hidden,
        Role::Success => theme.success,
        Role::Warning => theme.warning,
        Role::Error => theme.error,
    }
}

/// Maps an engine span style to a terminal style: roles become theme tokens, RGB values are
/// converted for the terminal's colour depth, `fg == None` keeps the pane's foreground.
pub fn span_style(style: SpanStyle, theme: &Theme) -> Style {
    let mut out = Style::new();
    match style.fg {
        Some(PreviewColor::Role(role)) => out = out.fg(role_color(role, theme)),
        Some(PreviewColor::Rgb(rgb)) => out = out.fg(theme.rgb(rgb)),
        None => {}
    }
    if style.bold {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.italic {
        out = out.add_modifier(Modifier::ITALIC);
    }
    if style.underline {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    if style.dim {
        out = out.add_modifier(Modifier::DIM);
    }
    out
}

// ----- directory rows (shared by the directory preview and the parent column) -----

/// How a directory row is drawn.
#[derive(Clone, Copy)]
struct DirRowStyle {
    icons: IconSet,
    base: SizeBase,
    /// Show the size column.
    sizes: bool,
    /// The row of the directory we are in (parent column only).
    current: bool,
    /// Left margin cells before the icon.
    lead: usize,
}

fn info_entry(info: &DirEntryInfo) -> Entry {
    let mut entry = match info.kind {
        ks_preview::EntryKind::Dir => Entry::dir(&info.name),
        ks_preview::EntryKind::Symlink { to_dir } => {
            let link = Entry::new(&info.name, EntryKind::Symlink);
            if to_dir {
                link.with_dir_target()
            } else {
                link
            }
        }
        ks_preview::EntryKind::File | ks_preview::EntryKind::Other => {
            Entry::file(&info.name, info.size.unwrap_or(0))
        }
    };
    if info.hidden {
        entry = entry.with_attrs(Attrs::HIDDEN);
    }
    entry
}

fn dir_row(
    info: &DirEntryInfo,
    width_cells: usize,
    style: DirRowStyle,
    theme: &Theme,
) -> Line<'static> {
    let entry = info_entry(info);
    let is_dir = info.kind.is_dir_like();
    let bg = if style.current {
        Style::new().bg(theme.selection_bg)
    } else {
        Style::new()
    };
    // Everything here is dimmed, like the list, dimmed.
    let name_style = if style.current {
        Style::new().fg(theme.dir).add_modifier(Modifier::BOLD)
    } else if info.hidden {
        Style::new().fg(theme.hidden).add_modifier(Modifier::DIM)
    } else if is_dir {
        Style::new().fg(theme.dir).add_modifier(Modifier::DIM)
    } else {
        theme.dim()
    };
    let icon = icon_for(&entry, style.icons);
    let size_text = if style.sizes && !is_dir {
        info.size.map(|s| human_size(s, style.base))
    } else {
        None
    };
    let size_width = size_text.as_ref().map_or(0, |_| 9);
    let fixed = style.lead + 2 + size_width + usize::from(size_width > 0);
    let name_cells = width_cells.saturating_sub(fixed).max(1);
    let mut name = sanitize(&info.name);
    if is_dir {
        name.push('/');
    }
    let name = truncate_middle(&name, name_cells, !is_dir);
    let pad = name_cells.saturating_sub(width(&name));

    let span = |text: String, s: Style| Span::styled(text, s.patch(bg));
    let mut spans = Vec::with_capacity(6);
    if style.lead >= 2 {
        spans.push(span(" ".repeat(style.lead - 1), Style::new()));
        spans.push(span(
            if style.current { CURSOR_GLYPH } else { " " }.to_owned(),
            theme.accent_style(),
        ));
    } else {
        spans.push(span(" ".repeat(style.lead), Style::new()));
    }
    spans.push(span(format!("{icon} "), name_style));
    spans.push(span(name, name_style));
    spans.push(span(" ".repeat(pad), Style::new()));
    if let Some(size) = size_text {
        spans.push(span(format!(" {size:>8}"), theme.muted()));
    }
    Line::from(spans)
}

/// The parent directory with the current directory highlighted.
fn render_parent(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let Some(preview) = state.preview.parent.as_deref() else {
        return;
    };
    let PreviewBody::Directory(dir) = &preview.body else {
        return;
    };
    let current = state
        .cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    let at = current
        .as_deref()
        .and_then(|name| dir.entries.iter().position(|e| e.name == name));
    let rows = usize::from(area.height);
    let offset = at
        .map_or(0, |i| i.saturating_sub(rows / 2))
        .min(dir.entries.len().saturating_sub(rows));
    let row_style = DirRowStyle {
        icons: state.config.icons,
        base: state.config.files.size_base,
        sizes: false,
        current: false,
        lead: 2,
    };
    let lines: Vec<Line> = dir
        .entries
        .iter()
        .enumerate()
        .skip(offset)
        .take(rows)
        .map(|(index, info)| {
            dir_row(
                info,
                usize::from(area.width),
                DirRowStyle {
                    current: Some(index) == at,
                    ..row_style
                },
                theme,
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

// ----- the preview pane -----

fn render_preview(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    if area.height == 0 || area.width < 4 {
        return;
    }
    let [header, rule, content] = split_pane(area);
    render_header(frame, header, state, theme);
    if rule.height > 0 {
        let line = RULE_GLYPH.repeat(usize::from(rule.width));
        frame.render_widget(
            Paragraph::new(Span::styled(line, Style::new().fg(theme.border))),
            rule,
        );
    }
    if content.is_empty() {
        return;
    }
    // One cell of padding on both sides of the content.
    let content = Rect::new(
        content.x + 1,
        content.y,
        content.width.saturating_sub(2),
        content.height,
    );
    let Some(preview) = state.preview.current.as_deref() else {
        if state.preview.loading {
            frame.render_widget(
                Paragraph::new(Span::styled("loading…", theme.muted())),
                content,
            );
        }
        return;
    };
    // A result for a path other than the cursor's is never shown.
    if state.preview.target.as_deref() != Some(preview.path.as_path()) {
        return;
    }
    // The overlay draws the (shared) preview itself; a second draw of the same image at
    // another size would make the shared protocol re-encode on every frame.
    if matches!(preview.body, PreviewBody::Image(_)) && crate::search::preview_rect(state).is_some()
    {
        return;
    }
    render_content(frame, content, state, theme, preview, None);
}

/// The preview pane inside a search overlay (find file / content search): the same content
/// renderer as the main pane, with a header naming the highlighted file instead of the list's
/// cursor entry. `line` (1-based) is the hit of a content search; it gets the selection
/// background when it is on screen.
pub(crate) fn render_overlay_pane(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    target: &ks_core::PreviewTarget,
) {
    if area.height == 0 || area.width < 4 {
        return;
    }
    let [header, rule, content] = split_pane(area);
    let name = target
        .path
        .file_name()
        .map_or_else(String::new, |n| sanitize(&n.to_string_lossy()));
    let right = target.line.map(|l| format!("line {l}")).unwrap_or_default();
    let total = usize::from(header.width).saturating_sub(2);
    let right_width = if right.is_empty() {
        0
    } else {
        width(&right) + 2
    };
    let name = truncate_middle(&name, total.saturating_sub(right_width), true);
    let pad = total.saturating_sub(width(&name) + width(&right));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(name, Style::new().fg(theme.fg).add_modifier(Modifier::BOLD)),
            Span::raw(" ".repeat(pad)),
            Span::styled(right, theme.dim()),
            Span::raw(" "),
        ])),
        header,
    );
    if rule.height > 0 {
        let line = RULE_GLYPH.repeat(usize::from(rule.width));
        frame.render_widget(
            Paragraph::new(Span::styled(line, Style::new().fg(theme.border))),
            rule,
        );
    }
    if content.is_empty() {
        return;
    }
    let content = Rect::new(
        content.x + 1,
        content.y,
        content.width.saturating_sub(2),
        content.height,
    );
    let current =
        state.preview.current.as_deref().filter(|p| {
            p.path == target.path && state.preview.target.as_deref() == Some(&target.path)
        });
    match current {
        Some(preview) => {
            let mark = target
                .line
                .and_then(|l| usize::try_from(l.saturating_sub(1)).ok());
            render_content(frame, content, state, theme, preview, mark);
        }
        None if state.preview.loading => frame.render_widget(
            Paragraph::new(Span::styled("loading…", theme.muted())),
            content,
        ),
        None => {}
    }
}

/// Header line, rule and content, each possibly empty on very short terminals.
fn split_pane(area: Rect) -> [Rect; 3] {
    let header = Rect::new(area.x, area.y, area.width, area.height.min(1));
    let rule_h = area.height.saturating_sub(1).min(HEADER_ROWS - 1);
    let rule = Rect::new(area.x, area.y + header.height, area.width, rule_h);
    let used = header.height + rule.height;
    let content = Rect::new(
        area.x,
        area.y + used,
        area.width,
        area.height.saturating_sub(used),
    );
    [header, rule, content]
}

/// Name on the left, `size · modified` on the right (more for archives, images, ...).
fn render_header(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let Some(entry) = state.view.current() else {
        return;
    };
    let mut name = sanitize(&entry.display_name());
    if entry.is_dir {
        name.push('/');
    }
    let total = usize::from(area.width).saturating_sub(2);
    let mut segments = header_segments(state, entry);
    // Drop the least important segment until the name has room (at least 12 cells or its
    // own width, whichever is smaller).
    let wanted = width(&name).min(12);
    let right = loop {
        let right = segments
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join(" · ");
        let used = if right.is_empty() {
            0
        } else {
            width(&right) + 2
        };
        if total.saturating_sub(used) >= wanted || segments.is_empty() {
            break right;
        }
        if let Some(worst) = segments
            .iter()
            .enumerate()
            .max_by_key(|(_, (priority, _))| *priority)
            .map(|(i, _)| i)
        {
            segments.remove(worst);
        }
    };
    let right_width = if right.is_empty() {
        0
    } else {
        width(&right) + 2
    };
    let name = truncate_middle(&name, total.saturating_sub(right_width), !entry.is_dir);
    // A link shows where it points, in whatever room the name leaves.
    let link = link_suffix(
        state,
        entry,
        total.saturating_sub(right_width + width(&name)),
    );
    let link_width = link.as_ref().map_or(0, |(text, _)| width(text));
    let pad = total.saturating_sub(width(&name) + link_width + width(&right));
    let name_style = Style::new().fg(theme.fg).add_modifier(Modifier::BOLD);
    let link = link.map(|(text, broken)| {
        let color = if broken { theme.error } else { theme.symlink };
        Span::styled(text, Style::new().fg(color))
    });
    let line = Line::from(vec![
        Span::raw(" "),
        Span::styled(name, name_style),
        link.unwrap_or_else(|| Span::raw("")),
        Span::raw(" ".repeat(pad)),
        Span::styled(right, theme.dim()),
        Span::raw(" "),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// ` → target` for a symlink or junction whose target is known from the preview result, cut
/// to `room` cells (nothing when fewer than 6 remain). The flag marks a broken link.
fn link_suffix(state: &AppState, entry: &Entry, room: usize) -> Option<(String, bool)> {
    if !entry.is_link() || room < 6 {
        return None;
    }
    let meta = state
        .preview
        .current
        .as_deref()
        .filter(|p| state.preview.target.as_deref() == Some(p.path.as_path()))?
        .meta
        .as_ref()?;
    let target = sanitize(&meta.link_target.as_ref()?.to_string_lossy());
    let text = truncate_middle(&target, room - 3, false);
    Some((format!(" → {text}"), meta.link_broken))
}

/// `(priority, text)` pairs in display order; a higher priority number is dropped first.
fn header_segments(state: &AppState, entry: &Entry) -> Vec<(u8, String)> {
    let base = state.config.files.size_base;
    let mut out: Vec<(u8, String)> = Vec::new();
    let preview = state
        .preview
        .current
        .as_deref()
        .filter(|p| state.preview.target.as_deref() == Some(p.path.as_path()));
    match preview.map(|p| &p.body) {
        Some(PreviewBody::Image(image)) => {
            out.push((
                3,
                format!("{}×{} {}", image.width, image.height, image.format),
            ));
        }
        Some(PreviewBody::Archive(archive)) => {
            out.push((
                3,
                format!(
                    "{} · {} entries",
                    archive.format.label(),
                    archive.total_entries
                ),
            ));
        }
        Some(PreviewBody::Binary(binary)) => {
            if let Some(hint) = binary.type_hint {
                out.push((3, hint.to_owned()));
            }
        }
        Some(PreviewBody::Text(text)) => {
            if let Some(syntax) = &text.syntax {
                out.push((3, syntax.clone()));
            }
        }
        _ => {}
    }
    if entry.is_dir {
        let items = match preview.map(|p| &p.body) {
            Some(PreviewBody::Directory(dir)) => Some(count_label(dir)),
            _ => entry.children.map(|n| format!("{n} items")),
        };
        if let Some(items) = items {
            out.push((1, items));
        }
    } else {
        out.push((1, human_size(entry.size, base)));
    }
    if let Some(mtime) = entry.mtime {
        out.push((2, relative_time(mtime, state.now, state.utc_offset_secs)));
    }
    out
}

fn count_label(dir: &DirPreview) -> String {
    let plus = if dir.scan_capped { "+" } else { "" };
    let noun = if dir.total == 1 && !dir.scan_capped {
        "item"
    } else {
        "items"
    };
    format!("{}{plus} {noun}", dir.total)
}

fn render_content(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    preview: &Preview,
    mark: Option<usize>,
) {
    let rows = usize::from(area.height);
    // The overlay's hit line, or the visual-lines selection of the main pane.
    let marked: Option<(usize, usize)> = mark
        .map(|m| (m, m))
        .or_else(|| state.preview.select.as_ref().map(|s| s.range()));
    let cursor_line = state.preview.select.as_ref().map(|s| s.cursor);
    let cells = usize::from(area.width);
    let scroll = state.preview.scroll;
    let lines: Vec<Line> = match &preview.body {
        PreviewBody::Image(image) => {
            crate::images::render_image(frame, area, state.preview.image_id, &image.image);
            return;
        }
        PreviewBody::Metadata => metadata_lines(preview.meta.as_ref(), state, theme),
        PreviewBody::Empty => vec![Line::from(Span::styled("empty file", theme.muted()))],
        PreviewBody::Text(text) => text_lines(
            text,
            true,
            (scroll, marked, cursor_line),
            rows,
            cells,
            theme,
        ),
        PreviewBody::Markdown(text) => {
            text_lines(text, false, (scroll, None, None), rows, cells, theme)
        }
        PreviewBody::Directory(dir) => dir_lines(dir, state, scroll, rows, cells, theme),
        PreviewBody::Archive(archive) => archive_lines(archive, state, scroll, rows, cells, theme),
        PreviewBody::Binary(binary) => binary_lines(binary, area.width, scroll, rows, theme),
        PreviewBody::Unavailable { message, .. } => {
            let mut lines: Vec<Line> = wrap(&sanitize(message), cells)
                .into_iter()
                .map(|row| Line::from(Span::styled(row, Style::new().fg(theme.warning))))
                .collect();
            if preview.meta.is_some() {
                lines.push(Line::default());
                lines.extend(metadata_lines(preview.meta.as_ref(), state, theme));
            }
            lines
        }
    };
    frame.render_widget(Paragraph::new(lines), area);
    // In the right padding cell of the pane.
    crate::scrollbar::render(
        frame.buffer_mut(),
        area.right(),
        area,
        ks_core::preview::content_len(preview, area.width),
        scroll,
        theme,
        state.config.icons == ks_core::config::IconSet::Ascii,
    );
}

// ----- text -----

/// Breaks `text` at spaces into rows of at most `max` cells (long words are cut).
pub(crate) fn wrap(text: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    for word in text.split_whitespace() {
        let word = truncate_end(word, max);
        let needed = if row.is_empty() { 0 } else { 1 } + width(&word);
        if width(&row) + needed > max && !row.is_empty() {
            rows.push(std::mem::take(&mut row));
        }
        if !row.is_empty() {
            row.push(' ');
        }
        row.push_str(&word);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

fn digits(n: usize) -> usize {
    n.max(1).ilog10() as usize + 1
}

/// Cuts a styled line to `max` cells, appending its spans to `out`.
pub(crate) fn push_clipped(
    out: &mut Vec<Span<'static>>,
    line: &StyledLine,
    max: usize,
    theme: &Theme,
) {
    let mut used = 0;
    for span in &line.spans {
        if used >= max {
            break;
        }
        let style = span_style(span.style, theme);
        let w = width(&span.text);
        if used + w <= max {
            out.push(Span::styled(span.text.clone(), style));
            used += w;
            continue;
        }
        let mut cut = String::new();
        for c in span.text.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if used + cw > max {
                break;
            }
            cut.push(c);
            used += cw;
        }
        out.push(Span::styled(cut, style));
        break;
    }
}

pub(crate) fn text_lines(
    text: &TextPreview,
    numbered: bool,
    (scroll, mark, cursor): (usize, Option<(usize, usize)>, Option<usize>),
    rows: usize,
    cells: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let gutter = if numbered {
        digits(text.first_line + text.line_count) + 1
    } else {
        0
    };
    let body = cells.saturating_sub(gutter);
    let mut out: Vec<Line> = text
        .lines
        .iter()
        .enumerate()
        .skip(scroll)
        .take(rows)
        .map(|(index, line)| {
            let mut spans = Vec::new();
            if numbered {
                spans.push(Span::styled(
                    format!("{:>w$} ", text.first_line + index + 1, w = gutter - 1),
                    theme.muted(),
                ));
            }
            push_clipped(&mut spans, line, body, theme);
            let line = Line::from(spans);
            if mark.is_some_and(|(a, b)| (a..=b).contains(&(text.first_line + index))) {
                // The content-search hit or the selected range: whole rows on the selection
                // background; the moving end of a selection is bold.
                let pad = (gutter + body).saturating_sub(line.width());
                let mut line = line;
                line.spans.push(Span::raw(" ".repeat(pad)));
                let mut style = Style::new().bg(theme.selection_bg);
                if cursor == Some(index) {
                    style = style.add_modifier(Modifier::BOLD);
                }
                line.style(style)
            } else {
                line
            }
        })
        .collect();
    if text.truncated && out.len() < rows && scroll + out.len() >= text.lines.len() {
        out.push(Line::from(Span::styled(
            truncate_end(
                &if text.first_line > 0 {
                    format!(
                        "… truncated, showing lines {}-{}",
                        text.first_line + 1,
                        text.first_line + text.lines.len()
                    )
                } else {
                    format!("… truncated, showing the first {} lines", text.lines.len())
                },
                cells,
            ),
            theme.muted(),
        )));
    }
    out
}

// ----- directories, archives, binary -----

fn dir_lines(
    dir: &DirPreview,
    state: &AppState,
    scroll: usize,
    rows: usize,
    cells: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if dir.entries.is_empty() {
        return vec![Line::from(Span::styled("empty directory", theme.muted()))];
    }
    let style = DirRowStyle {
        icons: state.config.icons,
        base: state.config.files.size_base,
        sizes: cells >= 28,
        current: false,
        lead: 0,
    };
    let mut out: Vec<Line> = dir
        .entries
        .iter()
        .skip(scroll)
        .take(rows)
        .map(|info| dir_row(info, cells, style, theme))
        .collect();
    if dir.truncated && out.len() < rows && scroll + out.len() >= dir.entries.len() {
        let more = dir.total.saturating_sub(dir.entries.len());
        let text = if dir.scan_capped {
            "… more entries not listed".to_owned()
        } else {
            format!("… and {more} more")
        };
        out.push(Line::from(Span::styled(text, theme.muted())));
    }
    out
}

fn archive_lines(
    archive: &ArchivePreview,
    state: &AppState,
    scroll: usize,
    rows: usize,
    cells: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if !archive.listed {
        return vec![Line::from(Span::styled(
            format!(
                "{} archive: contents are not listed",
                archive.format.label()
            ),
            theme.muted(),
        ))];
    }
    let base = state.config.files.size_base;
    let size_cells = 9;
    let name_cells = cells.saturating_sub(size_cells + 1).max(1);
    let mut out: Vec<Line> = archive
        .entries
        .iter()
        .skip(scroll)
        .take(rows)
        .map(|entry| {
            let name = truncate_middle(&sanitize(&entry.name), name_cells, !entry.is_dir);
            let pad = name_cells.saturating_sub(width(&name));
            let name_style = if entry.is_dir {
                Style::new().fg(theme.dir).add_modifier(Modifier::DIM)
            } else {
                theme.dim()
            };
            let size = if entry.is_dir {
                String::new()
            } else {
                human_size(entry.size, base)
            };
            Line::from(vec![
                Span::styled(name, name_style),
                Span::raw(" ".repeat(pad)),
                Span::styled(format!(" {size:>8}"), theme.muted()),
            ])
        })
        .collect();
    if archive.truncated && out.len() < rows && scroll + out.len() >= archive.entries.len() {
        let more = archive.total_entries.saturating_sub(archive.entries.len());
        out.push(Line::from(Span::styled(
            format!("… and {more} more"),
            theme.muted(),
        )));
    }
    out
}

fn hex_text(chunk: &[u8], per_row: usize) -> String {
    if per_row == 16 {
        return HexRow {
            offset: 0,
            bytes: chunk.to_vec(),
        }
        .hex();
    }
    let mut hex = chunk
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    let full = per_row * 3 - 1;
    hex.push_str(&" ".repeat(full - hex.len()));
    hex
}

pub(crate) fn binary_lines(
    binary: &BinaryPreview,
    pane_width: u16,
    scroll: usize,
    rows: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let per_row = hex_bytes_per_row(pane_width);
    let bytes: Vec<u8> = binary
        .rows
        .iter()
        .flat_map(|row| row.bytes.iter().copied())
        .collect();
    let start = binary.rows.first().map_or(0, |row| row.offset);
    let mut out: Vec<Line> = bytes
        .chunks(per_row)
        .enumerate()
        .skip(scroll)
        .take(rows)
        .map(|(index, chunk)| {
            let offset = start + (index * per_row) as u64;
            let ascii = HexRow {
                offset,
                bytes: chunk.to_vec(),
            }
            .ascii();
            Line::from(vec![
                Span::styled(format!("{offset:08x}"), theme.muted()),
                Span::raw("  "),
                Span::styled(hex_text(chunk, per_row), Style::new().fg(theme.fg)),
                Span::raw("  "),
                Span::styled(ascii, theme.dim()),
            ])
        })
        .collect();
    if let Some(note) = &binary.note {
        let lines = bytes.len().div_ceil(per_row);
        if out.len() < rows && scroll + out.len() >= lines {
            out.push(Line::from(Span::styled(
                sanitize(note),
                Style::new().fg(theme.warning),
            )));
        }
    }
    out
}

// ----- metadata -----

fn metadata_lines(
    meta: Option<&FileMetadata>,
    state: &AppState,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some(meta) = meta else {
        return vec![Line::from(Span::styled(
            "no metadata available",
            theme.muted(),
        ))];
    };
    let base = state.config.files.size_base;
    let offset = state.utc_offset_secs;
    let mut rows: Vec<(&str, String, Style)> = Vec::new();
    let plain = Style::new().fg(theme.fg);
    let name = meta
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    rows.push(("Name", sanitize(&name), plain));
    rows.push((
        "Path",
        sanitize(&dunce::simplified(&meta.path).display().to_string()),
        plain,
    ));
    let kind = match meta.kind {
        ks_preview::EntryKind::Dir => "directory",
        ks_preview::EntryKind::File => "file",
        ks_preview::EntryKind::Symlink { to_dir: true } => "link to a directory",
        ks_preview::EntryKind::Symlink { to_dir: false } => "link",
        ks_preview::EntryKind::Other => "other",
    };
    rows.push(("Type", kind.to_owned(), plain));
    if !meta.is_dir() {
        rows.push((
            "Size",
            format!("{} ({} bytes)", human_size(meta.size, base), meta.size),
            plain,
        ));
    }
    for (label, time) in [
        ("Created", meta.created),
        ("Modified", meta.modified),
        ("Accessed", meta.accessed),
    ] {
        if let Some(time) = time {
            rows.push((label, absolute_time(time, offset), plain));
        }
    }
    let attrs = meta.attributes.labels();
    if !attrs.is_empty() {
        rows.push(("Attrs", attrs.join(", "), plain));
    }
    if let Some(target) = &meta.link_target {
        let mut text = sanitize(&dunce::simplified(target).display().to_string());
        let style = if meta.link_broken {
            text.push_str(" (broken)");
            Style::new().fg(theme.error)
        } else {
            Style::new().fg(theme.symlink)
        };
        rows.push(("Target", text, style));
    } else if meta.link_broken {
        rows.push((
            "Target",
            "unreadable (broken link)".to_owned(),
            Style::new().fg(theme.error),
        ));
    }
    if let Some(owner) = &meta.owner {
        rows.push(("Owner", sanitize(owner), plain));
    }
    rows.into_iter()
        .map(|(label, value, style)| {
            Line::from(vec![
                Span::styled(format!("{label:<9} "), theme.muted()),
                Span::styled(value, style),
            ])
        })
        .collect()
}

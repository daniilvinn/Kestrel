//! The file list and the drives view. Only the rows inside the viewport are built, so a
//! directory with 100k entries costs the same to draw as one with 20.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use ks_core::config::IconSet;
use ks_core::layout::SIZES_MIN_WIDTH;
use ks_core::{AppState, DriveInfo, DriveKind, Entry, GitState};

use crate::format::{human_size, relative_time};
use crate::icons::icon_for;
use crate::text::{truncate_end, truncate_middle, width};
use crate::theme::Theme;

const CURSOR_GLYPH: &str = "▌";
const MARK_GLYPH: &str = "●";
/// Shown in the mark column for an entry waiting in the cut register (next to the dimming, so
/// the state does not depend on colour alone).
const CUT_GLYPH: &str = "×";
/// Cells before the icon: left margin, cursor bar, mark.
const LEAD_WIDTH: usize = 3;
/// Icon and the space after it.
const ICON_WIDTH: usize = 2;
/// Space and the git glyph, then a right margin.
const GIT_WIDTH: usize = 2;
const MARGIN_RIGHT: usize = 1;
const SIZE_WIDTH: usize = 9;
const TIME_WIDTH: usize = 10;
/// The modified-time column needs a roomy list.
const TIME_MIN_WIDTH: u16 = 90;
/// Names are never squeezed below this; other columns give way first.
const MIN_NAME_WIDTH: usize = 12;
/// Narrowest list pane that shows the size column: fixed cells, size cell and a usable name.
const SIZE_PANE_MIN: usize =
    LEAD_WIDTH + ICON_WIDTH + GIT_WIDTH + MARGIN_RIGHT + SIZE_WIDTH + 1 + MIN_NAME_WIDTH;

/// Which optional columns fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Columns {
    size: bool,
    time: bool,
}

impl Columns {
    /// `pane` is the list pane width, `terminal` the whole terminal width: hides
    /// sizes under 60 terminal columns, and the pane must still leave names their minimum.
    fn for_width(pane: u16, terminal: u16) -> Self {
        Self {
            size: terminal >= SIZES_MIN_WIDTH && usize::from(pane) >= SIZE_PANE_MIN,
            time: pane >= TIME_MIN_WIDTH,
        }
    }

    fn fixed_width(self) -> usize {
        LEAD_WIDTH
            + ICON_WIDTH
            + GIT_WIDTH
            + MARGIN_RIGHT
            + if self.size { SIZE_WIDTH + 1 } else { 0 }
            + if self.time { TIME_WIDTH + 1 } else { 0 }
    }
}

pub fn render_list(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    if state.show_drives {
        render_drives(frame, area, state, theme);
        return;
    }
    let view = &state.view;
    if view.is_empty() {
        render_placeholder(frame, area, state, theme);
        return;
    }

    let columns = Columns::for_width(area.width, state.size.width);
    let name_width = usize::from(area.width)
        .saturating_sub(columns.fixed_width())
        .max(MIN_NAME_WIDTH.min(usize::from(area.width)));
    let cut = cut_names(state);
    let first = view.cursor.offset;
    let rows = usize::from(area.height);
    let lines: Vec<Line> = (first..first.saturating_add(rows).min(view.len()))
        .filter_map(|index| {
            let entry = view.row(index)?;
            Some(entry_line(
                entry,
                RowContext {
                    state,
                    theme,
                    columns,
                    name_width,
                    cursor: index == view.cursor.index,
                    marked: view.is_marked(entry),
                    cut: !cut.is_empty() && cut.contains(&cut_key(&entry.display_name())),
                    highlight: view.highlight(index),
                },
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
    // In the right margin, so it never covers a name or a size.
    crate::scrollbar::render(
        frame.buffer_mut(),
        area.right().saturating_sub(1),
        area,
        view.len(),
        first,
        theme,
        state.config.icons == IconSet::Ascii,
    );
}

struct RowContext<'a> {
    state: &'a AppState,
    theme: &'a Theme,
    columns: Columns,
    name_width: usize,
    cursor: bool,
    marked: bool,
    /// The entry sits in the cut register (`x`): dimmed and flagged with a glyph.
    cut: bool,
    /// Matched character positions of the in-directory filter.
    highlight: &'a [u32],
}

fn entry_line(entry: &Entry, ctx: RowContext<'_>) -> Line<'static> {
    let RowContext {
        state,
        theme,
        columns,
        name_width,
        cursor,
        marked,
        cut,
        highlight,
    } = ctx;
    let row_bg = if cursor {
        Style::new().bg(theme.selection_bg)
    } else {
        Style::new()
    };
    let span = |text: String, style: Style| Span::styled(text, style.patch(row_bg));

    let mut spans: Vec<Span> = Vec::with_capacity(9);
    spans.push(span(" ".into(), Style::new()));
    spans.push(span(
        if cursor { CURSOR_GLYPH } else { " " }.into(),
        theme.accent_style(),
    ));
    spans.push(match (marked, cut) {
        (true, _) => span(MARK_GLYPH.into(), theme.accent_style()),
        (false, true) => span(CUT_GLYPH.into(), theme.dim()),
        (false, false) => span(" ".into(), theme.accent_style()),
    });

    let mut name_style = name_style(entry, marked, theme);
    let mut icon_style = icon_style(entry, theme);
    if cut {
        name_style = dimmed(name_style, theme);
        icon_style = dimmed(icon_style, theme);
    }
    if cursor {
        name_style = readable_on_cursor(name_style, theme);
        icon_style = readable_on_cursor(icon_style, theme);
    }
    spans.push(span(
        format!("{} ", icon_for(entry, state.config.icons)),
        icon_style,
    ));

    let mut name = crate::text::sanitize(&entry.display_name());
    if entry.is_dir {
        name.push('/');
    }
    // A file Claude edited gets a glyph at the end of the name column.
    let touched = crate::claude::touched_glyph(entry, state);
    let name_width = if touched.is_some() {
        name_width.saturating_sub(crate::claude::TOUCHED_WIDTH)
    } else {
        name_width
    };
    let shown = truncate_middle(&name, name_width, !entry.is_dir);
    let padding = name_width.saturating_sub(width(&shown));
    let mapped;
    let highlight = if highlight.is_empty() || shown == name {
        highlight
    } else {
        mapped = remap_highlight(&name, &shown, highlight);
        &mapped
    };
    if highlight.is_empty() {
        spans.push(span(shown, name_style));
    } else {
        for part in crate::search::highlighted(&shown, highlight, name_style, theme.bold_accent()) {
            spans.push(Span::styled(part.content, part.style.patch(row_bg)));
        }
    }
    spans.push(span(" ".repeat(padding), Style::new()));
    if let Some(glyph) = touched {
        spans.push(span(format!(" {glyph}"), theme.accent_style()));
    }

    if columns.size {
        spans.push(span(" ".into(), Style::new()));
        spans.push(span(
            format!("{:>SIZE_WIDTH$}", size_text(entry, state)),
            theme.dim(),
        ));
    }
    if columns.time {
        let text = entry.mtime.map_or_else(String::new, |t| {
            relative_time(t, state.now, state.utc_offset_secs)
        });
        spans.push(span(" ".into(), Style::new()));
        spans.push(span(format!("{text:>TIME_WIDTH$}"), theme.muted()));
    }

    let git = state
        .git
        .as_ref()
        .and_then(|snapshot| snapshot.state_of(&entry.display_name()));
    spans.push(span(" ".into(), Style::new()));
    spans.push(match git {
        Some(git) => span(git.glyph().to_string(), git_style(git, theme)),
        None => span(" ".into(), Style::new()),
    });
    spans.push(span(" ".repeat(MARGIN_RIGHT), Style::new()));
    Line::from(spans)
}

/// The dimmed look of an entry that is waiting to be moved.
fn dimmed(style: Style, theme: &Theme) -> Style {
    style
        .fg(theme.fg_muted)
        .remove_modifier(Modifier::BOLD)
        .add_modifier(Modifier::DIM)
}

/// Faint text (hidden or cut entries) on the cursor row: the selection background already
/// marks the row, and dim text on it is barely readable, so it gets the secondary text colour.
fn readable_on_cursor(style: Style, theme: &Theme) -> Style {
    if style.add_modifier.contains(Modifier::DIM) {
        style.remove_modifier(Modifier::DIM).fg(theme.fg_dim)
    } else {
        style
    }
}

/// Lookup key of a name in this directory (file names compare case-insensitively on Windows).
fn cut_key(name: &str) -> String {
    if cfg!(windows) {
        name.to_lowercase()
    } else {
        name.to_owned()
    }
}

/// Names in the current directory that are in the cut register.
fn cut_names(state: &AppState) -> std::collections::HashSet<String> {
    let Some(register) = state.fileops.register.as_ref().filter(|r| r.cut) else {
        return Default::default();
    };
    register
        .paths
        .iter()
        .filter(|path| {
            path.parent()
                .is_some_and(|parent| ks_core::fileops::same_path(parent, &state.cwd))
        })
        .filter_map(|path| path.file_name())
        .map(|name| cut_key(&name.to_string_lossy()))
        .collect()
}

/// Maps match positions in `name` onto the shortened `shown` (`head…tail`); positions inside
/// the cut-out part are dropped.
fn remap_highlight(name: &str, shown: &str, highlight: &[u32]) -> Vec<u32> {
    let name_len = name.chars().count();
    let shown_len = shown.chars().count();
    let prefix = name
        .chars()
        .zip(shown.chars())
        .take_while(|(a, b)| a == b)
        .count();
    // `shown` is `prefix chars`, the ellipsis, then a tail copied from the end of `name`.
    let tail = shown_len.saturating_sub(prefix + 1);
    highlight
        .iter()
        .filter_map(|&i| {
            let i = i as usize;
            if i < prefix {
                Some(i as u32)
            } else if tail > 0 && i >= name_len - tail {
                u32::try_from(prefix + 1 + (i - (name_len - tail))).ok()
            } else {
                None
            }
        })
        .collect()
}

/// Directories show their number of children (once known), files their size.
fn size_text(entry: &Entry, state: &AppState) -> String {
    if entry.is_dir {
        entry.children.map_or_else(String::new, |n| n.to_string())
    } else if state.view.label.is_some() && entry.size == 0 {
        // Search results carry no metadata yet.
        String::new()
    } else {
        human_size(entry.size, state.config.files.size_base)
    }
}

fn name_style(entry: &Entry, marked: bool, theme: &Theme) -> Style {
    if marked {
        return theme.bold_accent();
    }
    if entry.is_cloud_placeholder() {
        return theme.dim().add_modifier(Modifier::ITALIC);
    }
    if entry.is_hidden() {
        return Style::new().fg(theme.hidden).add_modifier(Modifier::DIM);
    }
    if entry.is_link() {
        return Style::new().fg(theme.symlink);
    }
    if entry.is_dir {
        return Style::new().fg(theme.dir);
    }
    if entry.is_exec() {
        return Style::new().fg(theme.exec);
    }
    Style::new().fg(theme.fg)
}

fn icon_style(entry: &Entry, theme: &Theme) -> Style {
    if entry.is_dir {
        Style::new().fg(theme.dir)
    } else if entry.is_link() {
        Style::new().fg(theme.symlink)
    } else if entry.is_exec() {
        Style::new().fg(theme.exec)
    } else {
        theme.dim()
    }
}

pub fn git_style(state: GitState, theme: &Theme) -> Style {
    Style::new().fg(match state {
        GitState::Ignored => theme.git_ignored,
        GitState::Untracked => theme.git_untracked,
        GitState::Added | GitState::Renamed => theme.git_added,
        GitState::Deleted | GitState::Modified => theme.git_modified,
        GitState::Conflict => theme.git_conflict,
    })
}

fn render_placeholder(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let view = &state.view;
    let text = if view.loading || state.is_navigating() {
        "loading…".to_owned()
    } else if let Some(query) = view.filter_query() {
        format!("no entries match /{query} (esc clears the filter)")
    } else if view.hidden_count() > 0 {
        let n = view.hidden_count();
        let noun = if n == 1 { "entry" } else { "entries" };
        format!("{n} hidden {noun} (press . to show)")
    } else {
        "empty directory".to_owned()
    };
    let line = Line::from(vec![
        Span::raw(" ".repeat(LEAD_WIDTH)),
        Span::styled(
            truncate_end(&text, usize::from(area.width).saturating_sub(LEAD_WIDTH)),
            theme.muted(),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

// ----- drives -----

const DRIVE_ROOT_WIDTH: usize = 4;
const DRIVE_FS_WIDTH: usize = 6;
const DRIVE_BAR_WIDTH: usize = 10;
/// `1024.0 GB free of 1024.0 GB`
const DRIVE_SPACE_WIDTH: usize = 26;

fn render_drives(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let view = &state.drives;
    let ascii = state.config.icons == IconSet::Ascii;
    let first = view.cursor.offset;
    let rows = usize::from(area.height);
    let total_width = usize::from(area.width);
    // Drop details from the right as the terminal narrows.
    let show_space = total_width >= 70;
    let show_bar = total_width >= 54;
    let show_fs = total_width >= 40;
    let fixed = LEAD_WIDTH
        + DRIVE_ROOT_WIDTH
        + 1
        + if show_fs { DRIVE_FS_WIDTH + 1 } else { 0 }
        + if show_bar { DRIVE_BAR_WIDTH + 1 } else { 0 }
        + if show_space { DRIVE_SPACE_WIDTH + 1 } else { 0 }
        + MARGIN_RIGHT;
    let label_width = total_width.saturating_sub(fixed).max(1);

    let lines: Vec<Line> = (first..first.saturating_add(rows).min(view.drives.len()))
        .map(|index| {
            let drive = &view.drives[index];
            let cursor = index == view.cursor.index;
            let row_bg = if cursor {
                Style::new().bg(theme.selection_bg)
            } else {
                Style::new()
            };
            let span = |text: String, style: Style| Span::styled(text, style.patch(row_bg));
            let mut spans = vec![
                span(" ".into(), Style::new()),
                span(
                    if cursor { CURSOR_GLYPH } else { " " }.into(),
                    theme.accent_style(),
                ),
                span(" ".into(), Style::new()),
                span(
                    format!("{:<DRIVE_ROOT_WIDTH$}", root_label(drive)),
                    Style::new().fg(theme.dir).add_modifier(Modifier::BOLD),
                ),
                span(" ".into(), Style::new()),
                span(
                    format!(
                        "{:<label_width$}",
                        truncate_end(&drive_title(drive), label_width)
                    ),
                    Style::new().fg(theme.fg),
                ),
            ];
            if show_fs {
                spans.push(span(
                    format!(
                        " {:<DRIVE_FS_WIDTH$}",
                        truncate_end(&drive.fs, DRIVE_FS_WIDTH)
                    ),
                    theme.muted(),
                ));
            }
            if show_bar {
                spans.push(span(" ".into(), Style::new()));
                spans.push(span(
                    usage_bar(drive, DRIVE_BAR_WIDTH, ascii),
                    bar_style(drive, theme),
                ));
            }
            if show_space {
                spans.push(span(" ".into(), Style::new()));
                spans.push(span(
                    format!(
                        "{:>DRIVE_SPACE_WIDTH$}",
                        space_text(drive, state.config.files.size_base)
                    ),
                    theme.dim(),
                ));
            }
            spans.push(span(" ".repeat(MARGIN_RIGHT), Style::new()));
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// `C:` for `C:\`; other roots as they are.
fn root_label(drive: &DriveInfo) -> String {
    let text = dunce::simplified(&drive.root).display().to_string();
    match text.strip_suffix(['\\', '/']) {
        Some(stripped) if !stripped.is_empty() => stripped.to_owned(),
        _ => text,
    }
}

/// The volume label, or a description of the drive when it has none.
fn drive_title(drive: &DriveInfo) -> String {
    if !drive.label.is_empty() {
        return drive.label.clone();
    }
    match drive.kind {
        DriveKind::Fixed => "Local Disk",
        DriveKind::Removable => "Removable Disk",
        DriveKind::Network => "Network Drive",
        DriveKind::Optical => "CD/DVD Drive",
        DriveKind::RamDisk => "RAM Disk",
        DriveKind::Unknown => "Drive",
    }
    .to_owned()
}

/// A bar of `cells` cells showing how full the drive is. Empty (all shaded) when unknown.
pub fn usage_bar(drive: &DriveInfo, cells: usize, ascii: bool) -> String {
    let (full, empty) = if ascii { ('#', '-') } else { ('█', '░') };
    let filled = drive
        .used_fraction()
        .map_or(0, |f| (f * cells as f64).round() as usize)
        .min(cells);
    let mut bar = String::new();
    bar.extend(std::iter::repeat_n(full, filled));
    bar.extend(std::iter::repeat_n(empty, cells - filled));
    bar
}

fn bar_style(drive: &DriveInfo, theme: &Theme) -> Style {
    match drive.used_fraction() {
        Some(f) if f >= 0.95 => Style::new().fg(theme.error),
        Some(f) if f >= 0.85 => Style::new().fg(theme.warning),
        Some(_) => theme.accent_style(),
        None => theme.muted(),
    }
}

fn space_text(drive: &DriveInfo, base: ks_core::config::SizeBase) -> String {
    if drive.unavailable {
        return "not responding".to_owned();
    }
    match (drive.free, drive.total) {
        (Some(free), Some(total)) => format!(
            "{} free of {}",
            human_size(free, base),
            human_size(total, base)
        ),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faint_names_are_readable_on_the_cursor_row() {
        let theme = Theme::builtin("ember", crate::theme::ColorDepth::TrueColor).unwrap();
        let hidden = Style::new().fg(theme.hidden).add_modifier(Modifier::DIM);
        let lifted = readable_on_cursor(hidden, &theme);
        assert!(!lifted.add_modifier.contains(Modifier::DIM));
        assert_eq!(lifted.fg, Some(theme.fg_dim));
        // Styles that are not faint are left alone.
        let dir = Style::new().fg(theme.dir);
        assert_eq!(readable_on_cursor(dir, &theme), dir);
    }

    #[test]
    fn highlight_survives_truncation() {
        let name = "quarterly_report_final_v2.xlsx";
        let shown = truncate_middle(name, 20, true);
        assert_ne!(shown, name);
        // `rep` in the name is cut out or kept; whatever is kept must still point at the
        // same characters in `shown`.
        let hl: Vec<u32> = vec![0, 1, 10, 11, 12, 27, 28, 29];
        let mapped = remap_highlight(name, &shown, &hl);
        let chars: Vec<char> = shown.chars().collect();
        let name_chars: Vec<char> = name.chars().collect();
        assert!(mapped.contains(&0) && mapped.contains(&1));
        let last = *mapped.last().unwrap() as usize;
        assert_eq!(chars[last], name_chars[29]);
    }

    fn drive(total: Option<u64>, free: Option<u64>) -> DriveInfo {
        DriveInfo {
            total,
            free,
            ..DriveInfo::new("C:/")
        }
    }

    #[test]
    fn usage_bar_fills_proportionally() {
        assert_eq!(
            usage_bar(&drive(Some(100), Some(100)), 10, false),
            "░░░░░░░░░░"
        );
        assert_eq!(
            usage_bar(&drive(Some(100), Some(0)), 10, false),
            "██████████"
        );
        assert_eq!(
            usage_bar(&drive(Some(100), Some(50)), 10, false),
            "█████░░░░░"
        );
        assert_eq!(usage_bar(&drive(Some(100), Some(25)), 8, true), "######--");
        assert_eq!(usage_bar(&drive(None, None), 4, true), "----");
    }

    #[test]
    fn usage_bar_is_always_the_requested_width() {
        for used in 0..=100 {
            let d = drive(Some(100), Some(100 - used));
            assert_eq!(usage_bar(&d, 10, false).chars().count(), 10);
        }
    }

    #[test]
    fn an_unresponsive_drive_says_so() {
        let mut d = DriveInfo::new("Z:/");
        d.kind = DriveKind::Network;
        d.unavailable = true;
        assert_eq!(
            space_text(&d, ks_core::config::SizeBase::Binary),
            "not responding"
        );
        assert_eq!(drive_title(&d), "Network Drive");
    }

    #[test]
    fn drive_titles_and_roots() {
        assert_eq!(drive_title(&DriveInfo::new("C:/")), "Drive");
        let mut labelled = DriveInfo::new("D:/");
        labelled.label = "Data".into();
        assert_eq!(drive_title(&labelled), "Data");
        labelled.label.clear();
        labelled.kind = DriveKind::Removable;
        assert_eq!(drive_title(&labelled), "Removable Disk");
        assert_eq!(root_label(&DriveInfo::new("C:/")), "C:");
        assert_eq!(root_label(&DriveInfo::new("/")), "/");
    }

    #[test]
    fn sizes_show_for_a_preview_sized_pane_on_a_wide_enough_terminal() {
        // 80x24 and 120x32 with the preview open: 40% panes of 31 and 47 cells.
        assert!(Columns::for_width(31, 80).size);
        assert!(Columns::for_width(47, 120).size);
        assert!(!Columns::for_width(29, 80).size);
        // Under 60 terminal columns sizes go, whatever the pane.
        assert!(!Columns::for_width(40, 59).size);
    }

    #[test]
    fn optional_columns_follow_the_terminal_width() {
        assert_eq!(
            Columns::for_width(59, 59),
            Columns {
                size: false,
                time: false
            }
        );
        assert_eq!(
            Columns::for_width(80, 80),
            Columns {
                size: true,
                time: false
            }
        );
        assert_eq!(
            Columns::for_width(120, 120),
            Columns {
                size: true,
                time: true
            }
        );
    }
}

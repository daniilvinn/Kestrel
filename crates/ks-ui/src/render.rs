//! The main screen: header, separator, body, separator, prompt, hint line.
//!
//! `render` is a pure function of the state and the theme.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use ks_core::config::IconSet;
use ks_core::keymap::{Continuation, ContinuationTarget, KeyPattern};
use ks_core::layout::shows_hint_line;
use ks_core::{Action, AppState, GitSnapshot, KeyContext, Level, Mode};

use crate::hints::{fit_by_priority, NORMAL_HINTS};
use crate::preview::render_body;
use crate::text::{breadcrumb, truncate_end, width};
use crate::theme::Theme;

pub use ks_core::layout::HINT_MIN_HEIGHT;

const PROMPT_GLYPH: &str = "›";
const SEPARATOR_GLYPH: &str = "─";
const UNICODE_SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const ASCII_SPINNER: &[&str] = &["|", "/", "-", "\\"];

/// Draws the whole screen.
///
/// The body is split into an optional parent column, the list and the preview pane by
/// [`render_body`].
pub fn render(frame: &mut Frame, state: &AppState, theme: &Theme) {
    let area = frame.area();
    frame.render_widget(Block::new().style(theme.base_style()), area);

    let show_hint = shows_hint_line(area.height);
    let [header, top_rule, body, bottom_rule, prompt, hint] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(u16::from(show_hint)),
    ])
    .areas(area);

    render_header(frame, header, state, theme);
    render_rule(frame, top_rule, theme);
    crate::images::begin_frame();
    // The Claude pane takes its room first; the list/preview split lives in what is left.
    let (list_area, pane) = crate::claude::split_body(body, state);
    render_body(frame, list_area, state, theme);
    if let Some((pane_rect, split)) = pane {
        crate::claude::render_pane(frame, pane_rect, split, state, theme);
    }
    render_rule(frame, bottom_rule, theme);
    render_prompt(frame, prompt, state, theme);
    // The which-key popup floats over the list; only when there is no room for it do the
    // choices replace the hint line (or the lower rule on a short terminal).
    let popup_shown = crate::which_key::render(frame, body, state, theme);
    let in_hints = state.which_key_visible && !popup_shown;
    if show_hint {
        render_hints(frame, hint, state, theme, in_hints);
    } else if in_hints {
        render_hints(frame, bottom_rule, state, theme, true);
    }
    crate::search::render_overlays(frame, state, theme);
    crate::palette::render(frame, state, theme);
    crate::help::render(frame, state, theme);
    crate::navaids::render_overlays(frame, state, theme);
    // Overlays are drawn last, over everything.
    crate::fileops::render_overlays(frame, state, theme);
    crate::claude::render_overlays(frame, state, theme);
}

/// The terminal window (tab) title: the place the active tab shows, then the app name.
pub fn window_title(state: &AppState) -> String {
    let place =
        ks_core::tabs::place_title(&state.cwd, state.show_drives, state.view.label.as_deref());
    // Sanitized: the title travels inside an escape sequence.
    format!("{} — Kestrel", crate::text::sanitize(&place))
}

fn render_header(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    // The git summary sits on the right and is never elided; the path gives way.
    let mut right = if state.show_drives {
        Vec::new()
    } else {
        state
            .git
            .as_ref()
            .map(|git| git_summary(git, state.config.icons, theme))
            .unwrap_or_default()
    };
    let tabs = crate::navaids::tab_strip(state, theme);
    if !tabs.is_empty() {
        if !right.is_empty() {
            right.push(Span::raw("  "));
        }
        right.extend(tabs);
    }
    let right_width: usize = right.iter().map(|s| width(&s.content)).sum();
    let reserved = if right_width > 0 { right_width + 3 } else { 0 };
    let budget = usize::from(area.width).saturating_sub(2 + reserved);

    let line = if state.show_drives {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                truncate_end("This PC", budget),
                Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
        ])
    } else if let Some(label) = &state.view.label {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                truncate_end(&crate::text::sanitize(label), budget),
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
        ])
    } else {
        path_line(state, budget, theme)
    };
    frame.render_widget(Paragraph::new(line), area);

    if right_width > 0 && usize::from(area.width) > right_width + 2 {
        let w = u16::try_from(right_width).unwrap_or(u16::MAX);
        let x = area.right().saturating_sub(w + 1);
        frame.render_widget(
            Paragraph::new(Line::from(right)),
            Rect::new(x, area.y, w, 1),
        );
    }
}

fn path_line(state: &AppState, budget: usize, theme: &Theme) -> Line<'static> {
    let text = breadcrumb(&state.cwd, state.home.as_deref(), budget);
    match text.rsplit_once('/') {
        // Parent segments are dimmed, the current directory is emphasised.
        Some((parents, last)) if !last.is_empty() => Line::from(vec![
            Span::raw(" "),
            Span::styled(format!("{parents}/"), theme.dim()),
            Span::styled(
                last.to_owned(),
                Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
        ]),
        _ => Line::from(vec![Span::raw(" "), Span::styled(text, theme.dim())]),
    }
}

/// `main ↑1 ↓2 +2 ~3 ?4`: branch, distance from upstream, then staged, modified, untracked
/// and conflicted counts, or a check mark when the work tree is clean.
fn git_summary(git: &GitSnapshot, icons: IconSet, theme: &Theme) -> Vec<Span<'static>> {
    let ascii = icons == IconSet::Ascii;
    let mut spans = vec![Span::styled(
        git.branch.label(),
        Style::new().fg(theme.fg_dim).add_modifier(Modifier::BOLD),
    )];
    let mut push = |text: String, style: Style| {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(text, style));
    };
    if git.branch.ahead > 0 {
        let arrow = if ascii { "^" } else { "↑" };
        push(format!("{arrow}{}", git.branch.ahead), theme.dim());
    }
    if git.branch.behind > 0 {
        let arrow = if ascii { "v" } else { "↓" };
        push(format!("{arrow}{}", git.branch.behind), theme.dim());
    }
    let c = git.counts;
    if c.is_clean() {
        let ok = if ascii { "ok" } else { "✓" };
        push(ok.to_owned(), Style::new().fg(theme.success));
    }
    for (count, glyph, color) in [
        (c.conflicted, 'U', theme.git_conflict),
        (c.staged, '+', theme.git_added),
        (c.modified, '~', theme.git_modified),
        (c.untracked, '?', theme.git_untracked),
    ] {
        if count > 0 {
            push(format!("{glyph}{count}"), Style::new().fg(color));
        }
    }
    spans
}

fn render_rule(frame: &mut Frame, area: Rect, theme: &Theme) {
    let rule = SEPARATOR_GLYPH.repeat(usize::from(area.width));
    frame.render_widget(
        Paragraph::new(Span::styled(rule, Style::new().fg(theme.border))),
        area,
    );
}

pub(crate) fn spinner_glyph(state: &AppState) -> &'static str {
    let frames = if state.config.icons == IconSet::Ascii {
        ASCII_SPINNER
    } else {
        UNICODE_SPINNER
    };
    frames[state.spinner_frame % frames.len()]
}

/// `4/8 · 1 selected · loading…`: position in the list, marks, and listing progress.
fn list_status(state: &AppState) -> Vec<String> {
    let mut parts = Vec::new();
    let (position, total) = if state.show_drives {
        (state.drives.cursor.index, state.drives.drives.len())
    } else {
        (state.view.cursor.index, state.view.len())
    };
    parts.push(if total == 0 {
        "0/0".to_owned()
    } else {
        format!("{}/{total}", position + 1)
    });
    if state.mode == Mode::Visual {
        parts.push("visual".to_owned());
    }
    if !state.show_drives && state.view.marks_count() > 0 {
        parts.push(format!("{} selected", state.view.marks_count()));
    }
    if let Some(register) = crate::fileops::register_segment(state) {
        parts.push(register);
    }
    if state.view.loading || state.is_navigating() {
        parts.push("loading…".to_owned());
    }
    parts
}

/// Appends `span` to `segments`, separated from what is there by two spaces.
fn add<'a>(segments: &mut Vec<Span<'a>>, span: Span<'a>) {
    if !segments.is_empty() {
        segments.push(Span::raw("  "));
    }
    segments.push(span);
}

fn render_prompt(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    // Rename / create / shell take over the whole line while they are open.
    if crate::fileops::render_input_prompt(frame, area, state, theme) {
        return;
    }
    let mut left_spans = vec![
        Span::raw(" "),
        Span::styled(PROMPT_GLYPH, theme.bold_accent()),
        Span::raw(" "),
    ];
    let mut left_width = width(PROMPT_GLYPH) + 2;
    // The in-directory filter: `/query` with a cursor while typing, a chip once kept.
    let filter_budget = usize::from(area.width) / 2;
    if state.mode == Mode::Filter {
        left_spans.push(Span::styled("/", theme.bold_accent()));
        let spans = crate::search::input_spans(
            &state.search.filter_input,
            filter_budget,
            true,
            "filter",
            theme,
        );
        left_width += 1 + spans.iter().map(|s| width(&s.content)).sum::<usize>();
        left_spans.extend(spans);
    } else if let Some(query) = state.view.filter_query() {
        let chip = format!("/{}", truncate_end(query, filter_budget));
        left_width += width(&chip);
        left_spans.push(Span::styled(chip, theme.bold_accent()));
    }
    frame.render_widget(Paragraph::new(Line::from(left_spans)), area);

    // Right side, in order: pending keys, status message, list status, jobs.
    let mut segments: Vec<Span> = Vec::new();
    if !state.pending.is_empty() {
        let pending = state
            .pending
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        add(&mut segments, Span::styled(pending, theme.bold_accent()));
    }

    let list = list_status(state).join(" · ");
    let jobs = crate::fileops::job_segment(state).or_else(|| {
        state.has_running_jobs().then(|| {
            let count = state.jobs.len();
            let label = if count == 1 { "job" } else { "jobs" };
            format!("{} {count} {label}", spinner_glyph(state))
        })
    });

    if let Some(status) = &state.status {
        let color = match status.level {
            Level::Info => theme.fg_dim,
            Level::Warning => theme.warning,
            Level::Error => theme.error,
        };
        // Whatever the other segments need is reserved first; the message takes the rest.
        let used: usize = segments.iter().map(|s| width(&s.content)).sum();
        let others = width(&list) + jobs.as_ref().map_or(0, |j| width(j) + 2) + 2;
        let budget = usize::from(area.width).saturating_sub(left_width + used + others + 4);
        add(
            &mut segments,
            Span::styled(
                truncate_end(&crate::text::sanitize(&status.text), budget),
                Style::new().fg(color),
            ),
        );
    }
    add(&mut segments, Span::styled(list, theme.dim()));
    if let Some(jobs) = jobs {
        segments.push(Span::styled(" · ", theme.muted()));
        segments.push(Span::styled(jobs, theme.accent_style()));
    }

    let line = Line::from(segments);
    let line_width = u16::try_from(line.width()).unwrap_or(u16::MAX);
    let x = area
        .right()
        .saturating_sub(line_width.saturating_add(1))
        .max(area.x + u16::try_from(left_width).unwrap_or(0));
    let right = Rect::new(x, area.y, area.right().saturating_sub(x), 1);
    frame.render_widget(Paragraph::new(line), right);
}

/// Hint entries `(action, label)` for the current state.
fn hints_for(state: &AppState) -> &'static [(Action, &'static str)] {
    match (state.key_context(), state.show_drives) {
        (KeyContext::Normal, true) => &[
            (Action::DirEnter, "open"),
            (Action::CursorDown, "down"),
            (Action::CursorUp, "up"),
            (Action::HistoryBack, "back"),
            (Action::Quit, "quit"),
        ],
        (KeyContext::Normal, false) => NORMAL_HINTS,
        (KeyContext::Claude, _) => &[
            (Action::ClaudeFocus, "explorer"),
            (Action::ClaudeGrow, "grow"),
            (Action::ClaudeShrink, "shrink"),
            (Action::ClaudeScrollUp, "scroll"),
        ],
        (KeyContext::ClaudeChanges, _) => &[
            (Action::Accept, "reveal"),
            (Action::CursorDown, "down"),
            (Action::CursorUp, "up"),
            (Action::ClaudeChangesClear, "clear"),
            (Action::Cancel, "close"),
        ],
        (KeyContext::PreviewLines, _) => &[
            (Action::CursorDown, "down"),
            (Action::CursorUp, "up"),
            (Action::PreviewLinesSend, "send to Claude"),
            (Action::Cancel, "cancel"),
        ],
        (KeyContext::Visual, _) => &[
            (Action::CursorDown, "down"),
            (Action::CursorUp, "up"),
            (Action::VisualToggle, "done"),
            (Action::Cancel, "cancel"),
        ],
        (KeyContext::Palette, _) => &[
            (Action::Accept, "run"),
            (Action::CursorDown, "down"),
            (Action::CursorUp, "up"),
            (Action::Cancel, "close"),
        ],
        (KeyContext::Help, _) => &[
            (Action::FilterOpen, "filter"),
            (Action::CursorDown, "scroll"),
            (Action::PageHalfDown, "page"),
            (Action::Cancel, "close"),
        ],
        (KeyContext::Bookmarks, _) => &[
            (Action::Accept, "jump"),
            (Action::BookmarkDelete, "delete"),
            (Action::Cancel, "close"),
        ],
        (KeyContext::Jump, _) => &[
            (Action::Accept, "jump"),
            (Action::CursorDown, "next"),
            (Action::CursorUp, "previous"),
            (Action::Cancel, "close"),
        ],
        (KeyContext::Filter, _) => &[
            (Action::Accept, "keep"),
            (Action::Cancel, "clear"),
            (Action::CursorDown, "down"),
            (Action::CursorUp, "up"),
        ],
        (KeyContext::Finder, _) => &[
            (Action::Accept, "go"),
            (Action::FinderOpen, "open"),
            (Action::FinderMark, "mark"),
            (Action::FinderScope, "scope"),
            (Action::Cancel, "close"),
        ],
        (KeyContext::Grep, _) => &[
            (Action::Accept, "edit"),
            (Action::GrepNextField, "field"),
            (Action::GrepRegex, "regex"),
            (Action::GrepWholeWord, "word"),
            (Action::GrepToList, "list"),
            (Action::GrepMark, "mark"),
            (Action::GrepSendClaude, "claude"),
            (Action::Cancel, "close"),
        ],
        _ => &[(Action::Cancel, "cancel")],
    }
}

fn render_hints(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme, which_key: bool) {
    let entries: Vec<(String, String)> = if which_key {
        state
            .keymap
            .continuations(state.key_context(), &state.pending)
            .iter()
            .map(which_key_entry)
            .collect()
    } else {
        let ctx = state.key_context();
        let resolved = hints_for(state).iter().filter_map(|(action, label)| {
            let keys = state.keymap.keys_for(ctx, *action);
            keys.first()
                .map(|k| (*action, k.clone(), (*label).to_owned()))
        });
        if std::ptr::eq(hints_for(state), NORMAL_HINTS) {
            // Width-aware: `: ? q` stay, optional hints give way by priority.
            fit_by_priority(
                resolved.collect(),
                usize::from(area.width).saturating_sub(3),
            )
        } else {
            resolved.map(|(_, key, label)| (key, label)).collect()
        }
    };

    let budget = usize::from(area.width).saturating_sub(3);
    let mut spans = vec![Span::raw("   ")];
    let mut used = 0;
    for (index, (key, label)) in entries.iter().enumerate() {
        let gap = if index == 0 { 0 } else { 2 };
        let cost = gap + width(key) + width(label) + 3;
        if used + cost > budget {
            spans.push(Span::styled("…", theme.muted()));
            break;
        }
        used += cost;
        if gap > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled("‹", theme.muted()));
        spans.push(Span::styled(key.clone(), theme.dim()));
        spans.push(Span::styled(format!("› {label}"), theme.muted()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn which_key_entry(c: &Continuation) -> (String, String) {
    let key = match c.key {
        KeyPattern::Chord(chord) => chord.to_string(),
        KeyPattern::AnyChar => "any".to_owned(),
    };
    let label = match c.target {
        ContinuationTarget::Action(action) => action.title().to_owned(),
        ContinuationTarget::Group => "…".to_owned(),
    };
    (key, label)
}

#[cfg(test)]
mod tests {
    use ks_core::layout::body_height;
    use ks_core::{update, Entry, KeyChord, Msg, TimerKind};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::testutil::{
        drives_state, dump, empty_state, fixture_entries, listing_state, move_to,
    };
    use crate::theme::{ColorDepth, Theme};

    fn buffer_of(state: &AppState, theme: &Theme) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(state.size.width, state.size.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, state, theme)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn draw(state: &AppState, theme: &Theme) -> String {
        dump(&buffer_of(state, theme))
    }

    /// Only the text rows, without the style legend.
    fn text_of(state: &AppState) -> String {
        draw(state, &theme("ember"))
            .split("--- styles")
            .next()
            .unwrap()
            .to_owned()
    }

    fn theme(name: &str) -> Theme {
        Theme::builtin(name, ColorDepth::TrueColor).unwrap()
    }

    const SIZES: [(u16, u16); 2] = [(80, 24), (160, 48)];
    const THEMES: [&str; 2] = ["ember", "paper"];

    #[test]
    fn main_screen_snapshots() {
        for name in THEMES {
            for (w, h) in SIZES {
                let rendered = draw(&listing_state(w, h), &theme(name));
                insta::assert_snapshot!(format!("main_{name}_{w}x{h}"), rendered);
            }
        }
    }

    #[test]
    fn narrow_screen_snapshots() {
        for name in THEMES {
            let rendered = draw(&listing_state(60, 20), &theme(name));
            insta::assert_snapshot!(format!("narrow_{name}_60x20"), rendered);
            // Under 60 columns the sizes go.
            let rendered = draw(&listing_state(50, 20), &theme(name));
            insta::assert_snapshot!(format!("narrow_{name}_50x20"), rendered);
        }
    }

    #[test]
    fn drives_view_snapshots() {
        for name in THEMES {
            for (w, h) in SIZES {
                let rendered = draw(&drives_state(w, h), &theme(name));
                insta::assert_snapshot!(format!("drives_{name}_{w}x{h}"), rendered);
            }
            let rendered = draw(&drives_state(50, 12), &theme(name));
            insta::assert_snapshot!(format!("drives_{name}_50x12"), rendered);
        }
    }

    #[test]
    fn drives_view_with_an_unresponsive_share_snapshots() {
        use ks_core::{DriveInfo, DriveKind};
        for name in THEMES {
            for (w, h) in SIZES {
                let mut state = drives_state(w, h);
                let mut ok = DriveInfo::new("C:/");
                ok.label = "Windows".into();
                ok.fs = "NTFS".into();
                ok.kind = DriveKind::Fixed;
                ok.total = Some(496_960_008_192);
                ok.free = Some(33_758_236_672);
                let mut dead = DriveInfo::new("Z:/");
                dead.kind = DriveKind::Network;
                dead.unavailable = true;
                let rows = state.viewport_height();
                state
                    .drives
                    .set(vec![ok, dead], std::path::Path::new("C:/x"), rows);
                let rendered = draw(&state, &theme(name));
                assert!(rendered.contains("not responding"));
                insta::assert_snapshot!(format!("drives_dead_{name}_{w}x{h}"), rendered);
            }
        }
    }

    #[test]
    fn visual_mode_snapshot() {
        for name in THEMES {
            for (w, h) in SIZES {
                let mut s = listing_state(w, h);
                let rows = s.viewport_height();
                s.view.enter_visual();
                s.mode = ks_core::Mode::Visual;
                s.view.move_cursor(2, rows);
                insta::assert_snapshot!(format!("visual_{name}_{w}x{h}"), draw(&s, &theme(name)));
            }
        }
    }

    #[test]
    fn loading_and_empty_snapshots() {
        for name in THEMES {
            for (w, h) in SIZES {
                let mut s = empty_state(w, h);
                s.view.loading = true;
                insta::assert_snapshot!(format!("loading_{name}_{w}x{h}"), draw(&s, &theme(name)));
                s.view.finish_loading();
                insta::assert_snapshot!(format!("empty_{name}_{w}x{h}"), draw(&s, &theme(name)));
            }
        }
    }

    #[test]
    fn error_status_snapshots() {
        for name in THEMES {
            for (w, h) in SIZES {
                let mut s = listing_state(w, h);
                update(
                    &mut s,
                    Msg::Notify {
                        level: Level::Error,
                        text: "Cannot open C:\\Secret: Access is denied.".into(),
                    },
                );
                insta::assert_snapshot!(format!("error_{name}_{w}x{h}"), draw(&s, &theme(name)));
            }
        }
    }

    #[test]
    fn which_key_is_shown_even_without_a_hint_line() {
        let mut s = listing_state(80, 15);
        assert!(!shows_hint_line(15));
        update(&mut s, Msg::Key(KeyChord::char('S')));
        s.which_key_visible = true;
        let text = text_of(&s);
        assert!(text.contains("size"), "{text}");
    }

    #[test]
    fn icon_set_snapshots() {
        for (set, label) in [
            (ks_core::config::IconSet::Unicode, "unicode"),
            (ks_core::config::IconSet::Ascii, "ascii"),
        ] {
            let mut s = listing_state(80, 24);
            s.config.icons = set;
            insta::assert_snapshot!(format!("icons_{label}_80x24"), draw(&s, &theme("ember")));
        }
    }

    #[test]
    fn empty_and_loading_placeholders() {
        let mut s = empty_state(80, 24);
        s.view.finish_loading();
        assert!(text_of(&s).contains("empty directory"));
        s.view.loading = true;
        let text = text_of(&s);
        assert!(text.contains("loading…"), "{text}");

        // Only hidden entries: say so.
        let mut s = empty_state(80, 24);
        let rows = s.viewport_height();
        s.view
            .push_chunk(vec![Entry::file(".a", 1), Entry::file(".b", 1)], rows);
        s.view.finish_loading();
        assert!(text_of(&s).contains("2 hidden entries (press . to show)"));
    }

    fn pending_with_status(w: u16, h: u16) -> AppState {
        let mut s = listing_state(w, h);
        update(&mut s, Msg::Key(KeyChord::char('g')));
        update(&mut s, Msg::JobStarted(ks_core::JobId(1)));
        update(
            &mut s,
            Msg::Notify {
                level: Level::Error,
                text: "config.toml: expected a string".into(),
            },
        );
        s
    }

    #[test]
    fn prompt_status_snapshots() {
        for name in THEMES {
            for (w, h) in SIZES {
                let rendered = draw(&pending_with_status(w, h), &theme(name));
                insta::assert_snapshot!(format!("prompt_status_{name}_{w}x{h}"), rendered);
            }
        }
    }

    #[test]
    fn which_key_snapshots() {
        for name in THEMES {
            for (w, h) in SIZES {
                let mut s = listing_state(w, h);
                let effects = update(&mut s, Msg::Key(KeyChord::char('g')));
                let ks_core::Effect::StartTimer { gen, .. } = effects[0] else {
                    panic!("expected which-key timer");
                };
                update(
                    &mut s,
                    Msg::Timer {
                        kind: TimerKind::WhichKey,
                        gen,
                    },
                );
                insta::assert_snapshot!(
                    format!("which_key_{name}_{w}x{h}"),
                    draw(&s, &theme(name))
                );
            }
        }
    }

    #[test]
    fn body_height_matches_the_real_layout() {
        for (w, h) in [(80, 24), (80, 19), (80, 20), (30, 5), (120, 3), (10, 1)] {
            let s = listing_state(w, h);
            let buffer = buffer_of(&s, &theme("ember"));
            // The body is the stretch between the two rules.
            let rule_rows: Vec<u16> = (0..h)
                .filter(|&y| w > 0 && buffer[(0, y)].symbol() == "─")
                .collect();
            if let [top, bottom] = rule_rows.as_slice() {
                assert_eq!(bottom - top - 1, body_height(s.size), "{w}x{h}");
            }
        }
    }

    #[test]
    fn marks_show_a_dot_and_the_cursor_a_bar() {
        let s = listing_state(80, 24);
        let text = text_of(&s);
        let cursor_row = text
            .lines()
            .find(|l| l.contains("vulkan_device.rs"))
            .unwrap();
        assert!(cursor_row.trim_start().starts_with('▌'), "{cursor_row}");
        let marked_row = text.lines().find(|l| l.contains("swapchain.rs")).unwrap();
        assert!(marked_row.contains('●'), "{marked_row}");
        assert!(!marked_row.contains('▌'));
        assert!(text.contains("2/") || text.contains("1 selected"), "{text}");
    }

    #[test]
    fn long_names_are_cut_in_the_middle_and_keep_their_extension() {
        let text = text_of(&listing_state(80, 24));
        let row = text
            .lines()
            .find(|l| l.contains("a_rather_long"))
            .unwrap_or_else(|| panic!("{text}"));
        assert!(row.contains('…'), "{row}");
        assert!(row.contains(".rs"), "{row}");
        assert!(!row.contains("the_list_row.rs"), "the middle is cut: {row}");
    }

    #[test]
    fn directories_show_counts_and_files_sizes() {
        let text = text_of(&listing_state(80, 24));
        let row = |needle: &str| {
            text.lines()
                .find(|l| l.contains(needle))
                .unwrap()
                .to_owned()
        };
        assert!(row("shaders/").contains("12"), "{}", row("shaders/"));
        assert!(row("vulkan_device.rs").contains("14.2 KB"));
        assert!(row("mod.rs").contains("412 B"));
        assert!(
            row("current/").contains('/'),
            "links to directories get a slash"
        );
    }

    #[test]
    fn git_glyphs_aggregate_to_directories() {
        let text = text_of(&listing_state(80, 24));
        let row = |needle: &str| {
            text.lines()
                .find(|l| l.contains(needle))
                .unwrap()
                .to_owned()
        };
        assert!(row("vulkan_device.rs").trim_end().ends_with('M'));
        assert!(row("descriptors.rs").trim_end().ends_with('+'));
        assert!(row("new_pass.rs").trim_end().ends_with('?'));
        assert!(row("passes/").trim_end().ends_with('M'));
        assert!(row("shaders/").trim_end().ends_with("12 !") || row("shaders/").contains('!'));
        assert!(row("mod.rs").trim_end().ends_with('B'));
    }

    #[test]
    fn header_shows_the_branch_and_counts_on_the_right() {
        let text = text_of(&listing_state(80, 24));
        let header = text.lines().next().unwrap();
        assert!(header.trim_end().ends_with("main ↑1 +1 ~2 ?1"), "{header}");
        assert!(
            header.contains("~/projects/omniforce/src/renderer"),
            "{header}"
        );

        // A clean repository gets a check mark; an ASCII terminal gets ASCII.
        let mut s = listing_state(80, 24);
        s.git.as_mut().unwrap().counts = ks_core::GitCounts::default();
        s.config.icons = ks_core::config::IconSet::Ascii;
        let header = text_of(&s).lines().next().unwrap().to_owned();
        assert!(header.trim_end().ends_with("main ^1 ok"), "{header}");

        // Too narrow for both: the path gives way, the branch stays.
        let narrow = text_of(&listing_state(40, 24));
        let header = narrow.lines().next().unwrap();
        assert!(header.trim_end().ends_with("main ↑1 +1 ~2 ?1"), "{header}");
        assert!(header.contains('…'), "{header}");
    }

    #[test]
    fn prompt_line_shows_position_selection_and_jobs() {
        let mut s = listing_state(100, 24);
        let rows = s.viewport_height();
        s.view.move_cursor(1, rows);
        update(&mut s, Msg::JobStarted(ks_core::JobId(1)));
        let text = text_of(&s);
        let prompt = text.lines().rev().nth(1).unwrap();
        let total = s.view.len();
        let position = s.view.cursor.index + 1;
        assert!(
            prompt.contains(&format!("{position}/{total} · 1 selected · ")),
            "{prompt}"
        );
        assert!(prompt.trim_end().ends_with("1 job"), "{prompt}");
    }

    #[test]
    fn drives_view_header_and_counter() {
        let s = drives_state(100, 24);
        let text = text_of(&s);
        assert!(text.lines().next().unwrap().contains("This PC"));
        assert!(text.contains("Windows"));
        assert!(text.contains("free of"));
        assert!(text.contains("2/4"), "{text}");
    }

    #[test]
    fn only_visible_rows_are_drawn_and_the_cursor_row_is_in_view() {
        let mut s = empty_state(80, 24);
        let rows = s.viewport_height();
        let many: Vec<Entry> = (0..100_000)
            .map(|i| Entry::file(format!("file_{i:06}.txt"), i))
            .collect();
        s.view.push_chunk(many, rows);
        s.view.finish_loading();
        s.view.move_cursor(50_000, rows);
        let text = text_of(&s);
        let cursor_row = text.lines().find(|l| l.contains('▌')).unwrap();
        assert!(cursor_row.contains("file_050000.txt"), "{cursor_row}");
        assert!(text.contains("50001/100000"), "{text}");
        let shown = text.lines().filter(|l| l.contains("file_")).count();
        assert_eq!(shown, usize::from(body_height(s.size)));
    }

    #[test]
    fn hint_line_is_hidden_on_short_terminals() {
        let short = text_of(&listing_state(100, HINT_MIN_HEIGHT - 1));
        assert!(!short.contains("‹q› quit"), "{short}");
        let tall = text_of(&listing_state(100, HINT_MIN_HEIGHT));
        assert!(tall.contains("‹q› quit"), "{tall}");
    }

    fn hint_row(width: u16) -> String {
        let text = text_of(&listing_state(width, 24));
        text.lines()
            .rev()
            .find(|l| l.contains("‹"))
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn essential_hints_survive_every_width() {
        for width in 40..=200u16 {
            let row = hint_row(width);
            for hint in ["‹:› commands", "‹?› help", "‹q› quit"] {
                // below 41 columns not even the essentials fit
                if width >= 41 {
                    assert!(row.contains(hint), "{width}: {hint} missing in {row:?}");
                }
            }
            assert!(!row.contains('…'), "{width}: {row:?}");
        }
    }

    #[test]
    fn optional_hints_appear_as_width_allows() {
        let narrow = hint_row(80);
        assert!(narrow.contains("‹/› filter"), "{narrow}");
        assert!(!narrow.contains("‹ctrl-f› search"), "{narrow}");
        let wide = hint_row(120);
        for hint in ["‹/› filter", "‹ctrl-p› find", "‹ctrl-f› search"] {
            assert!(wide.contains(hint), "{wide}");
        }
    }

    #[test]
    fn hints_follow_the_mode() {
        let mut s = listing_state(160, 24);
        let normal = text_of(&s);
        assert!(normal.contains("‹space› mark"), "{normal}");
        assert!(normal.contains("‹g /› drives"), "{normal}");
        s.view.enter_visual();
        s.mode = ks_core::Mode::Visual;
        let visual = text_of(&s);
        assert!(visual.contains("‹v› done"), "{visual}");
        assert!(visual.contains("‹esc› cancel"), "{visual}");
        assert!(!visual.contains("‹space› mark"));
    }

    #[test]
    fn which_key_popup_floats_over_the_list_and_the_hint_line_stays() {
        let mut s = listing_state(200, 24);
        let effects = update(&mut s, Msg::Key(KeyChord::char('g')));
        let ks_core::Effect::StartTimer { gen, .. } = effects[0] else {
            panic!("expected which-key timer");
        };
        update(
            &mut s,
            Msg::Timer {
                kind: TimerKind::WhichKey,
                gen,
            },
        );
        let out = text_of(&s);
        assert!(out.contains("g  Jump to top"), "{out}");
        assert!(out.contains("j  Jobs overlay"), "{out}");
        assert!(
            out.contains("╭─ g "),
            "popup title is the pending prefix: {out}"
        );
        assert!(out.contains("‹q› quit"), "normal hints stay: {out}");
    }

    #[test]
    fn the_sort_menu_is_the_which_key_popup() {
        let mut s = listing_state(200, 24);
        let effects = update(&mut s, Msg::Key(KeyChord::char('S')));
        let ks_core::Effect::StartTimer { gen, .. } = effects[0] else {
            panic!("expected which-key timer");
        };
        update(
            &mut s,
            Msg::Timer {
                kind: TimerKind::WhichKey,
                gen,
            },
        );
        let out = text_of(&s);
        for entry in [
            "n  Sort by name",
            "s  Sort by size",
            "m  Sort by modified time",
            "e  Sort by extension",
        ] {
            assert!(out.contains(entry), "{entry} missing in {out}");
        }
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (10, 3), (20, 5), (200, 2), (3, 40)] {
            let _ = draw(&listing_state(w, h), &theme("paper"));
            let _ = draw(&drives_state(w, h), &theme("paper"));
            let _ = draw(&empty_state(w, h), &theme("paper"));
        }
    }

    #[test]
    fn status_text_neutralises_bidi_overrides_and_escapes() {
        let mut s = listing_state(80, 24);
        update(
            &mut s,
            Msg::Notify {
                level: Level::Info,
                text: "Deleted invoice\u{202E}fdp.exe \u{1b}[31m".to_owned(),
            },
        );
        let out = text_of(&s);
        assert!(out.contains("invoice?fdp.exe"), "{out}");
        assert!(!out.contains('\u{202E}') && !out.contains('\u{1b}'));
    }

    #[test]
    fn long_status_is_truncated_not_wrapped() {
        let mut s = listing_state(40, 24);
        update(
            &mut s,
            Msg::Notify {
                level: Level::Warning,
                text: "x".repeat(200),
            },
        );
        let out = text_of(&s);
        assert!(out.contains('…'));
        let prompt = out.lines().rev().nth(1).unwrap();
        assert!(width(prompt) <= 40, "{prompt}");
    }

    #[test]
    fn scrolling_keeps_the_cursor_visible_at_every_position() {
        let mut s = empty_state(80, 12);
        let rows = s.viewport_height();
        s.view.push_chunk(
            (0..60)
                .map(|i| Entry::file(format!("f{i:02}"), 1))
                .collect(),
            rows,
        );
        s.view.finish_loading();
        for step in 0..60 {
            let text = text_of(&s);
            assert!(text.contains('▌'), "step {step}\n{text}");
            let name = format!("f{:02}", s.view.cursor.index);
            let row = text.lines().find(|l| l.contains('▌')).unwrap();
            assert!(row.contains(&name), "step {step}: {row}");
            s.view.move_cursor(1, rows);
        }
        move_to(&mut s, "f00");
        assert!(text_of(&s).contains("f00"));
        let _ = fixture_entries();
    }
}

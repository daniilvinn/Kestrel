//! `update` logic of the file-operation feature: actions, overlay and prompt keys, and the
//! handling of engine messages.
//!
//! Hooks into `update.rs` (all additive): [`intercept_key`] runs before the keymap,
//! [`handle_action`] runs before the generic action dispatch, and `Msg::Job`, `Msg::Shell`,
//! `Msg::Foreground`, `Msg::ForegroundDone`, `Msg::OpenHandlers` and `Msg::BulkPlanned` are
//! forwarded to the `on_*` functions.

use std::path::{Path, PathBuf};

use super::input::PromptInput;
use super::types::{
    BulkPlanResult, ConfirmAction, ConfirmDialog, ConflictAnswer, ConflictDialog, FgPending,
    ForegroundPurpose, ForegroundRequest, JobDone, JobMsg, JobRequest, JobRow, JobStatus, Menu,
    MenuItem, MenuKind, OpKind, Pager, PagerLine, PagerStatus, PathFormatKind, Register, ShellMsg,
    TextPrompt, PAGER_MAX_LINES,
};
use crate::action::Action;
use crate::effect::Effect;
use crate::key::{Key, KeyChord, Modifiers};
use crate::keymap::Invocation;
use crate::mode::{Mode, Overlay, PromptKind};
use crate::msg::TimerKind;
use crate::nav;
use crate::state::{AppState, JobId, Level, QuitKind, Size};
use crate::update::set_status;

/// Finished jobs kept for the Jobs overlay.
const JOB_HISTORY: usize = 40;
/// Lines of a confirmation dialog that are listed before "and N more".
const CONFIRM_LINES: usize = 12;

// ----- geometry shared with the renderer -----

/// Rows of output the pager shows at a terminal size. The box leaves the top row (header)
/// and the two bottom rows (prompt and hints) visible; inside it the borders and the footer
/// take three more.
pub fn pager_rows(size: Size) -> usize {
    usize::from(size.height).saturating_sub(6).max(1)
}

/// Width of a pager line (box minus borders and one cell of padding on each side).
pub fn pager_width(size: Size) -> usize {
    usize::from(size.width).saturating_sub(8).max(1)
}

// ----- small helpers -----

/// Path equality as the file system sees it: case-insensitive on Windows.
pub fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

fn push_overlay(state: &mut AppState, overlay: Overlay) {
    if state.overlays.last() != Some(&overlay) {
        state.overlays.push(overlay);
    }
}

fn remove_overlay(state: &mut AppState, overlay: Overlay) {
    if let Some(pos) = state.overlays.iter().rposition(|o| *o == overlay) {
        state.overlays.remove(pos);
    }
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn items_label(count: usize) -> String {
    if count == 1 {
        "1 item".to_owned()
    } else {
        format!("{count} items")
    }
}

/// Where the cursor starts in the rename prompt: before the extension of a file, at the end
/// for directories and names without an extension (a leading dot is not an extension).
pub fn rename_cursor(name: &str, is_dir: bool) -> usize {
    let chars = name.chars().count();
    if is_dir {
        return chars;
    }
    match name.rfind('.') {
        Some(i) if i > 0 => name[..i].chars().count(),
        _ => chars,
    }
}

/// The paths an operation acts on: the marked entries (in list order), else the entry under
/// the cursor. Only rows that are shown count, so a hidden marked file is never touched.
pub fn targets(state: &AppState) -> Vec<PathBuf> {
    if state.show_drives {
        return Vec::new();
    }
    let view = &state.view;
    if view.marks_count() > 0 {
        (0..view.len())
            .filter_map(|i| view.row(i))
            .filter(|e| view.is_marked(e))
            .map(|e| state.cwd.join(&e.name))
            .collect()
    } else {
        view.current()
            .map(|e| vec![state.cwd.join(&e.name)])
            .unwrap_or_default()
    }
}

/// Most items `o` and `e` launch in one go; more would flood the desktop (or queue that many
/// foreground editor runs).
pub const MAX_LAUNCH: usize = 8;

/// Like [`targets`], but optionally without directories.
fn launch_targets(state: &AppState, files_only: bool) -> Vec<PathBuf> {
    if !files_only {
        return targets(state);
    }
    let view = &state.view;
    if view.marks_count() > 0 {
        (0..view.len())
            .filter_map(|i| view.row(i))
            .filter(|e| view.is_marked(e) && !e.is_dir)
            .map(|e| state.cwd.join(&e.name))
            .collect()
    } else {
        view.current()
            .filter(|e| !e.is_dir)
            .map(|e| vec![state.cwd.join(&e.name)])
            .unwrap_or_default()
    }
}

fn clear_selection(state: &mut AppState) {
    state.view.clear_marks();
    if state.mode == Mode::Visual {
        state.mode = Mode::Normal;
    }
}

fn warn(state: &mut AppState, effects: &mut Vec<Effect>, text: &str) {
    set_status(state, Level::Warning, text.to_owned(), effects);
}

// ----- actions -----

/// Handles an action. Returns false when it is none of this feature's business.
pub(crate) fn handle_action(
    state: &mut AppState,
    invocation: &Invocation,
    effects: &mut Vec<Effect>,
) -> bool {
    let action = invocation.action;
    if let Some(&top) = state.overlays.last() {
        match top {
            Overlay::Jobs => return jobs_action(state, action, effects),
            Overlay::Menu => return menu_action(state, action, effects),
            Overlay::Pager => return pager_action(state, action, effects),
            Overlay::Confirm => return confirm_action(state, action, effects),
            Overlay::Conflict => return conflict_action(state, action, effects),
            _ => return false,
        }
    }
    if let Mode::Prompt(kind) = state.mode {
        if state.fileops.prompt.is_some() && kind != PromptKind::Command {
            return prompt_action(state, action, effects);
        }
    }
    file_action(state, action, effects)
}

fn file_action(state: &mut AppState, action: Action, effects: &mut Vec<Effect>) -> bool {
    match action {
        Action::Copy => yank(state, effects, false),
        Action::Cut => yank(state, effects, true),
        Action::Paste => paste(state, effects),
        Action::Trash => trash(state, effects),
        Action::DeletePermanent => ask_delete(state, effects),
        Action::Extract => extract(state, effects),
        Action::Undo => effects.push(Effect::Undo),
        Action::Rename => start_rename(state, effects),
        Action::BulkRename => start_bulk_rename(state, effects),
        Action::Create => start_create(state),
        Action::CopyPath => open_copy_path_menu(state, effects),
        Action::Open => open_default(state, effects),
        Action::OpenWith => open_with(state, effects),
        Action::OpenEditor => open_editor(state, effects),
        Action::ShellCommand => start_shell(state),
        Action::JobsOpen => {
            state.fileops.jobs_selected = 0;
            push_overlay(state, Overlay::Jobs);
        }
        // From the command palette, without the Jobs overlay: stop the newest running job.
        Action::JobCancel => match state.fileops.jobs.iter().find(|j| j.status.is_active()) {
            Some(row) => {
                effects.push(Effect::CancelJob(row.id));
                let text = format!("Cancelling: {}", row.description);
                set_status(state, Level::Info, text, effects);
            }
            None => warn(state, effects, "No job is running"),
        },
        _ => return false,
    }
    state.dirty = true;
    true
}

/// Quits, or asks first when jobs are still running (quitting cancels them, which can leave a
/// move or delete half done) or a Claude session is open (quitting kills its process). A
/// process-level interrupt (`Msg::Interrupt`) still quits at once.
pub(crate) fn request_quit(state: &mut AppState, kind: QuitKind, effects: &mut Vec<Effect>) {
    discard_reedit_if_any(state, effects);
    let claude = state.claude.is_running();
    if state.jobs.is_empty() && !claude {
        state.quit = Some(kind);
        return;
    }
    let mut lines: Vec<String> = state
        .fileops
        .jobs
        .iter()
        .filter(|j| j.status.is_active())
        .take(CONFIRM_LINES)
        .map(|j| j.description.clone())
        .collect();
    let count = state.jobs.len();
    let more = count.saturating_sub(lines.len());
    let title = match (count > 0, claude) {
        (true, false) => format!("Quit and cancel {}?", plural(count, "running job")),
        (true, true) => format!(
            "Quit, cancel {} and close Claude?",
            plural(count, "running job")
        ),
        _ => "Quit and close Claude?".to_owned(),
    };
    if claude {
        lines.push("The Claude session in the pane ends with Kestrel.".to_owned());
    }
    state.fileops.confirm = Some(ConfirmDialog {
        title,
        more,
        lines,
        danger: true,
        token: None,
        typed: PromptInput::new(),
        mismatch: false,
        action: Some(ConfirmAction::Quit { kind }),
        accept_label: "quit".to_owned(),
    });
    push_overlay(state, Overlay::Confirm);
    state.dirty = true;
}

fn yank(state: &mut AppState, effects: &mut Vec<Effect>, cut: bool) {
    let paths = targets(state);
    if paths.is_empty() {
        warn(state, effects, "Nothing to copy here");
        return;
    }
    let text = format!("{} {}", paths.len(), if cut { "cut" } else { "yanked" });
    state.fileops.register = Some(Register { paths, cut });
    clear_selection(state);
    set_status(state, Level::Info, text, effects);
}

fn paste(state: &mut AppState, effects: &mut Vec<Effect>) {
    if state.show_drives {
        warn(state, effects, "Open a directory to paste into");
        return;
    }
    let Some(register) = state.fileops.register.clone() else {
        warn(state, effects, "Nothing to paste (yank with y, cut with x)");
        return;
    };
    let cwd = state.cwd.clone();
    let items: Vec<(PathBuf, PathBuf)> = register
        .paths
        .iter()
        .filter_map(|src| src.file_name().map(|n| (src.clone(), cwd.join(n))))
        .collect();
    if register.cut {
        // Moving an item onto itself is a no-op.
        let moves: Vec<_> = items
            .into_iter()
            .filter(|(src, dst)| !same_path(src, dst))
            .collect();
        if moves.is_empty() {
            warn(state, effects, "Already in this directory");
            return;
        }
        effects.push(Effect::SubmitJob {
            request: JobRequest::Move { items: moves },
            keep_both: false,
        });
        // The cut is consumed when the move succeeds (see finish_job); a copy can be
        // pasted again, and a failed or cancelled move leaves the cut in place.
    } else {
        // Pasting a copy next to its source keeps both (`name (1).ext`) instead of asking.
        let (same, other): (Vec<_>, Vec<_>) = items
            .into_iter()
            .partition(|(src, dst)| same_path(src, dst));
        if !other.is_empty() {
            effects.push(Effect::SubmitJob {
                request: JobRequest::Copy { items: other },
                keep_both: false,
            });
        }
        if !same.is_empty() {
            effects.push(Effect::SubmitJob {
                request: JobRequest::Copy { items: same },
                keep_both: true,
            });
        }
        if effects.is_empty() {
            warn(state, effects, "Nothing to paste");
        }
    }
}

fn trash(state: &mut AppState, effects: &mut Vec<Effect>) {
    let paths = targets(state);
    if paths.is_empty() {
        warn(state, effects, "Nothing to delete here");
        return;
    }
    clear_selection(state);
    effects.push(Effect::SubmitJob {
        request: JobRequest::Trash { paths },
        keep_both: false,
    });
}

/// File extensions `X` treats as archives (the engine checks the actual format).
pub const ARCHIVE_EXTENSIONS: &[&str] = &["zip", "rar", "cbz", "cbr"];

fn is_archive(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ARCHIVE_EXTENSIONS.iter().any(|a| e.eq_ignore_ascii_case(a)))
}

/// Extracts the archives among the targets next to themselves, one job per directory.
fn extract(state: &mut AppState, effects: &mut Vec<Effect>) {
    let archives: Vec<PathBuf> = targets(state)
        .into_iter()
        .filter(|p| is_archive(p))
        .collect();
    if archives.is_empty() {
        warn(state, effects, "Not a ZIP or RAR archive");
        return;
    }
    clear_selection(state);
    let mut groups: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
    for archive in archives {
        let dir = archive.parent().map(Path::to_path_buf).unwrap_or_default();
        match groups.iter_mut().find(|(d, _)| same_path(d, &dir)) {
            Some((_, list)) => list.push(archive),
            None => groups.push((dir, vec![archive])),
        }
    }
    for (dest_dir, archives) in groups {
        effects.push(Effect::SubmitJob {
            request: JobRequest::Extract { archives, dest_dir },
            keep_both: false,
        });
    }
}

/// The token to type for a permanent delete of `count` items: `yes`, or the count.
pub fn confirmation_token(count: usize) -> String {
    if count <= 1 {
        "yes".to_owned()
    } else {
        count.to_string()
    }
}

fn ask_delete(state: &mut AppState, effects: &mut Vec<Effect>) {
    let paths = targets(state);
    if paths.is_empty() {
        warn(state, effects, "Nothing to delete here");
        return;
    }
    discard_reedit_if_any(state, effects);
    let count = paths.len();
    let token = confirmation_token(count);
    let lines: Vec<String> = paths
        .iter()
        .take(CONFIRM_LINES)
        .map(|p| name_of(p))
        .collect();
    let more = count.saturating_sub(lines.len());
    state.fileops.confirm = Some(ConfirmDialog {
        title: format!("Delete {} permanently?", items_label(count)),
        lines,
        more,
        danger: true,
        token: Some(token),
        typed: PromptInput::new(),
        mismatch: false,
        action: Some(ConfirmAction::DeletePermanent { paths }),
        accept_label: "delete".to_owned(),
    });
    push_overlay(state, Overlay::Confirm);
}

fn start_rename(state: &mut AppState, effects: &mut Vec<Effect>) {
    if state.show_drives {
        return;
    }
    // A prompt replaces visual mode; its marks stay.
    nav::leave_visual_mode(state);
    let Some(entry) = state.view.current() else {
        warn(state, effects, "Nothing to rename here");
        return;
    };
    let name = entry.display_name().into_owned();
    let cursor = rename_cursor(&name, entry.is_dir);
    let target = state.cwd.join(&entry.name);
    state.fileops.prompt = Some(TextPrompt {
        kind: PromptKind::Rename,
        label: "rename".to_owned(),
        input: PromptInput::with_text(name, cursor),
        target: Some(target),
    });
    state.mode = Mode::Prompt(PromptKind::Rename);
}

fn start_create(state: &mut AppState) {
    if state.show_drives {
        return;
    }
    // A prompt replaces visual mode; its marks stay.
    nav::leave_visual_mode(state);
    state.fileops.prompt = Some(TextPrompt {
        kind: PromptKind::Create,
        label: "new (end with / for a folder)".to_owned(),
        input: PromptInput::new(),
        target: None,
    });
    state.mode = Mode::Prompt(PromptKind::Create);
}

fn start_shell(state: &mut AppState) {
    if state.show_drives {
        return;
    }
    // A prompt replaces visual mode; its marks stay.
    nav::leave_visual_mode(state);
    state.fileops.prompt = Some(TextPrompt {
        kind: PromptKind::Shell,
        label: "!".to_owned(),
        input: PromptInput::new(),
        target: None,
    });
    state.mode = Mode::Prompt(PromptKind::Shell);
}

fn start_bulk_rename(state: &mut AppState, effects: &mut Vec<Effect>) {
    if state.show_drives {
        return;
    }
    // Marked entries; with no marks, every entry that is shown (like vidir).
    let paths = if state.view.marks_count() > 0 {
        targets(state)
    } else {
        (0..state.view.len())
            .filter_map(|i| state.view.row(i))
            .map(|e| state.cwd.join(&e.name))
            .collect()
    };
    let total = paths.len();
    let names: Vec<String> = paths
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_owned))
        .collect();
    if names.is_empty() {
        warn(state, effects, "Nothing to rename here");
        return;
    }
    if names.len() < total {
        let skipped = total - names.len();
        warn(
            state,
            effects,
            &format!("{skipped} names are not valid Unicode and were left out"),
        );
    }
    clear_selection(state);
    effects.push(Effect::BulkRenameEdit {
        dir: state.cwd.clone(),
        names,
        reuse: None,
    });
}

fn open_copy_path_menu(state: &mut AppState, effects: &mut Vec<Effect>) {
    let paths = targets(state);
    if paths.is_empty() {
        warn(state, effects, "Nothing to copy here");
        return;
    }
    let item = |label: &str, hotkey: char| MenuItem {
        label: label.to_owned(),
        hotkey: Some(hotkey),
    };
    state.fileops.menu = Some(Menu {
        kind: MenuKind::CopyPath,
        title: format!("Copy {} as", items_label(paths.len())),
        items: vec![
            item("Absolute path", 'a'),
            item("Relative path", 'r'),
            item("File name", 'n'),
        ],
        selected: 0,
        paths,
    });
    push_overlay(state, Overlay::Menu);
}

fn open_default(state: &mut AppState, effects: &mut Vec<Effect>) {
    if state.show_drives {
        if let Some(drive) = state.drives.current() {
            effects.push(Effect::OpenDefault {
                path: drive.root.clone(),
            });
        }
        return;
    }
    let paths = launch_targets(state, false);
    if paths.is_empty() {
        warn(state, effects, "Nothing to open here");
        return;
    }
    if paths.len() > MAX_LAUNCH {
        let text = format!(
            "Too many items to open at once ({} > {MAX_LAUNCH})",
            paths.len()
        );
        warn(state, effects, &text);
        return;
    }
    clear_selection(state);
    effects.extend(paths.into_iter().map(|path| Effect::OpenDefault { path }));
}

fn open_with(state: &mut AppState, effects: &mut Vec<Effect>) {
    if state.show_drives {
        warn(state, effects, "Nothing to open here");
        return;
    }
    // Marked entries (or the cursor entry), like `o`.
    let paths = launch_targets(state, false);
    if paths.is_empty() {
        warn(state, effects, "Nothing to open here");
    } else if paths.len() > MAX_LAUNCH {
        let text = format!(
            "Too many items to open at once ({} > {MAX_LAUNCH})",
            paths.len()
        );
        warn(state, effects, &text);
    } else {
        effects.push(Effect::ListOpenHandlers { paths });
    }
}

fn open_editor(state: &mut AppState, effects: &mut Vec<Effect>) {
    let paths = launch_targets(state, true);
    if paths.is_empty() {
        warn(
            state,
            effects,
            "Nothing to edit here (directories are skipped)",
        );
        return;
    }
    if paths.len() > MAX_LAUNCH {
        let text = format!(
            "Too many files to edit at once ({} > {MAX_LAUNCH})",
            paths.len()
        );
        warn(state, effects, &text);
        return;
    }
    clear_selection(state);
    effects.extend(
        paths
            .into_iter()
            .map(|path| Effect::OpenInEditor { path, line: None }),
    );
}

// ----- overlays: actions -----

fn jobs_action(state: &mut AppState, action: Action, effects: &mut Vec<Effect>) -> bool {
    let count = state.fileops.jobs.len();
    match action {
        Action::Cancel => {
            state.overlays.pop();
        }
        Action::CursorDown => {
            state.fileops.jobs_selected =
                (state.fileops.jobs_selected + 1).min(count.saturating_sub(1));
        }
        Action::CursorUp => {
            state.fileops.jobs_selected = state.fileops.jobs_selected.saturating_sub(1);
        }
        Action::CursorTop => state.fileops.jobs_selected = 0,
        Action::CursorBottom => state.fileops.jobs_selected = count.saturating_sub(1),
        Action::JobsClear => {
            state.fileops.jobs.retain(|j| j.status.is_active());
            state.fileops.jobs_selected = 0;
        }
        Action::JobCancel => match state.fileops.jobs.get(state.fileops.jobs_selected) {
            Some(row) if row.status.is_active() => {
                effects.push(Effect::CancelJob(row.id));
                let text = format!("Cancelling: {}", row.description);
                set_status(state, Level::Info, text, effects);
            }
            Some(_) => warn(state, effects, "That job has already finished"),
            None => {}
        },
        _ => {}
    }
    state.dirty = true;
    true
}

fn menu_action(state: &mut AppState, action: Action, effects: &mut Vec<Effect>) -> bool {
    let Some(menu) = state.fileops.menu.as_mut() else {
        state.overlays.pop();
        return true;
    };
    match action {
        Action::Cancel => close_menu(state),
        Action::CursorDown => {
            menu.selected = (menu.selected + 1).min(menu.items.len().saturating_sub(1))
        }
        Action::CursorUp => menu.selected = menu.selected.saturating_sub(1),
        Action::CursorTop => menu.selected = 0,
        Action::CursorBottom => menu.selected = menu.items.len().saturating_sub(1),
        Action::Accept => choose_menu_item(state, effects),
        _ => {}
    }
    state.dirty = true;
    true
}

fn close_menu(state: &mut AppState) {
    state.fileops.menu = None;
    remove_overlay(state, Overlay::Menu);
}

fn choose_menu_item(state: &mut AppState, effects: &mut Vec<Effect>) {
    let Some(menu) = state.fileops.menu.take() else {
        return;
    };
    remove_overlay(state, Overlay::Menu);
    match menu.kind {
        MenuKind::CopyPath => {
            let format = match menu.selected {
                0 => PathFormatKind::Absolute,
                1 => PathFormatKind::Relative,
                _ => PathFormatKind::Name,
            };
            effects.push(Effect::CopyPaths {
                paths: menu.paths,
                format,
                cwd: state.cwd.clone(),
            });
        }
        MenuKind::OpenWith => {
            if let Some(item) = menu.items.get(menu.selected) {
                clear_selection(state);
                effects.push(Effect::OpenWith {
                    paths: menu.paths,
                    handler: item.label.clone(),
                });
            }
        }
    }
}

fn pager_action(state: &mut AppState, action: Action, effects: &mut Vec<Effect>) -> bool {
    let rows = pager_rows(state.size);
    let Some(pager) = state.fileops.pager.as_mut() else {
        state.overlays.pop();
        return true;
    };
    let max = pager.lines.len().saturating_sub(rows);
    let half = (rows / 2).max(1);
    match action {
        Action::Cancel => {
            // Closing the pager ends the command it was showing.
            if pager.is_running() {
                effects.push(Effect::CancelShell(pager.id));
            }
            state.fileops.pager = None;
            remove_overlay(state, Overlay::Pager);
        }
        Action::JobCancel => {
            if pager.is_running() {
                effects.push(Effect::CancelShell(pager.id));
            }
        }
        Action::CursorDown => scroll_to(pager, pager.scroll.saturating_add(1), max),
        Action::CursorUp => scroll_to(pager, pager.scroll.saturating_sub(1), max),
        Action::PageHalfDown => scroll_to(pager, pager.scroll.saturating_add(half), max),
        Action::PageHalfUp => scroll_to(pager, pager.scroll.saturating_sub(half), max),
        Action::CursorTop => scroll_to(pager, 0, max),
        Action::CursorBottom => scroll_to(pager, max, max),
        _ => {}
    }
    state.dirty = true;
    true
}

/// Scrolls to `target`; reaching the bottom turns follow mode back on.
fn scroll_to(pager: &mut Pager, target: usize, max: usize) {
    pager.scroll = target.min(max);
    pager.follow = pager.scroll >= max;
}

fn confirm_action(state: &mut AppState, action: Action, effects: &mut Vec<Effect>) -> bool {
    match action {
        Action::Cancel => cancel_confirm(state, effects),
        Action::Accept => accept_confirm(state, effects),
        _ => {}
    }
    state.dirty = true;
    true
}

/// Discards the kept edit file of an open `BulkReedit` confirm (no-op for other dialogs).
/// Not part of `close_confirm`: accepting closes first and then reopens the same file.
pub(crate) fn discard_reedit_if_any(state: &mut AppState, effects: &mut Vec<Effect>) {
    discard_reedit_except(state, None, effects);
}

/// Like [`discard_reedit_if_any`], but keeps the file when the dialog replacing the open one
/// still points at it (`keep`), so a retry reusing the same path is not deleted under it.
fn discard_reedit_except(
    state: &mut AppState,
    keep: Option<&std::path::Path>,
    effects: &mut Vec<Effect>,
) {
    if let Some(ConfirmAction::BulkReedit(retry)) = state
        .fileops
        .confirm
        .as_ref()
        .and_then(|c| c.action.clone())
    {
        if keep == Some(retry.file.as_path()) {
            return;
        }
        effects.push(Effect::BulkRenameDiscard { file: retry.file });
        set_status(state, Level::Info, "Bulk rename discarded".into(), effects);
    }
}

/// Declines the open confirm (esc / `n`): discards any kept bulk-rename file, then closes.
fn cancel_confirm(state: &mut AppState, effects: &mut Vec<Effect>) {
    discard_reedit_if_any(state, effects);
    close_confirm(state);
}

fn close_confirm(state: &mut AppState) {
    crate::claude::on_confirm_closed(state);
    state.fileops.confirm = None;
    remove_overlay(state, Overlay::Confirm);
}

fn accept_confirm(state: &mut AppState, effects: &mut Vec<Effect>) {
    let Some(confirm) = state.fileops.confirm.as_mut() else {
        remove_overlay(state, Overlay::Confirm);
        return;
    };
    if let Some(token) = &confirm.token {
        if !confirm.typed.text().trim().eq_ignore_ascii_case(token) {
            confirm.mismatch = true;
            return;
        }
    }
    let action = confirm.action.clone();
    close_confirm(state);
    match action {
        Some(ConfirmAction::DeletePermanent { paths }) => {
            clear_selection(state);
            effects.push(Effect::SubmitJob {
                request: JobRequest::DeletePermanent { paths },
                keep_both: false,
            });
        }
        Some(ConfirmAction::Quit { kind }) => state.quit = Some(kind),
        Some(ConfirmAction::ClaudeProceed) => crate::claude::confirm_proceed(state, effects),
        Some(ConfirmAction::BulkRename { steps }) => effects.push(Effect::SubmitJob {
            request: JobRequest::BulkRename { steps },
            keep_both: false,
        }),
        Some(ConfirmAction::BulkReedit(retry)) => effects.push(Effect::BulkRenameEdit {
            dir: retry.dir,
            names: retry.names,
            reuse: Some(retry.file),
        }),
        None => {}
    }
}

fn conflict_action(state: &mut AppState, action: Action, effects: &mut Vec<Effect>) -> bool {
    let Some(dialog) = state.fileops.conflicts.front_mut() else {
        remove_overlay(state, Overlay::Conflict);
        return true;
    };
    if !dialog.armed {
        arm_later(state, effects);
        state.dirty = true;
        return true;
    }
    match action {
        Action::Cancel => {
            if dialog.rename.take().is_none() {
                answer_conflict(state, effects, ConflictAnswer::Cancel);
            }
        }
        Action::Accept => match dialog.rename.take() {
            Some(input) => {
                let name = input.text().trim().to_owned();
                if name.is_empty() {
                    dialog.rename = Some(input);
                } else {
                    answer_conflict(state, effects, ConflictAnswer::Rename(name));
                }
            }
            None => answer_conflict(state, effects, ConflictAnswer::KeepBoth),
        },
        _ => {}
    }
    state.dirty = true;
    true
}

/// Sends the answer for the front conflict dialog and moves on to the next one.
fn answer_conflict(state: &mut AppState, effects: &mut Vec<Effect>, answer: ConflictAnswer) {
    let Some(dialog) = state.fileops.conflicts.pop_front() else {
        return;
    };
    let job = dialog.info.job;
    if let Some(row) = state.fileops.jobs.iter_mut().find(|r| r.id == job) {
        if row.status == JobStatus::AwaitingConflict {
            row.status = JobStatus::Running;
        }
    }
    effects.push(Effect::ResolveConflict {
        job,
        answer,
        apply_to_all: dialog.apply_to_all,
    });
    if state.fileops.conflicts.is_empty() {
        remove_overlay(state, Overlay::Conflict);
    } else {
        ensure_arming(state, effects);
    }
}

/// Grace period before a conflict dialog (or the next one in the queue) accepts keys.
const CONFLICT_ARM_DELAY: std::time::Duration = std::time::Duration::from_millis(600);

fn arm_later(state: &mut AppState, effects: &mut Vec<Effect>) {
    effects.push(Effect::StartTimer {
        kind: TimerKind::ConflictArm,
        gen: state.fileops.arm_gen.bump(),
        delay: CONFLICT_ARM_DELAY,
    });
}

/// Starts the grace timer when the front dialog is not armed yet.
fn ensure_arming(state: &mut AppState, effects: &mut Vec<Effect>) {
    if state.fileops.conflicts.front().is_some_and(|d| !d.armed) {
        arm_later(state, effects);
    }
}

/// The grace timer fired: the front dialog now reacts to keys.
pub(crate) fn on_arm_timer(state: &mut AppState, gen: crate::generation::Gen) {
    if !state.fileops.arm_gen.is_current(gen) {
        return;
    }
    if let Some(dialog) = state.fileops.conflicts.front_mut() {
        dialog.armed = true;
        state.dirty = true;
    }
}

fn prompt_action(state: &mut AppState, action: Action, effects: &mut Vec<Effect>) -> bool {
    match action {
        Action::Cancel => close_prompt(state),
        Action::Accept => submit_prompt(state, effects),
        _ => {}
    }
    state.dirty = true;
    true
}

fn close_prompt(state: &mut AppState) {
    state.fileops.prompt = None;
    if matches!(state.mode, Mode::Prompt(_)) {
        state.mode = Mode::Normal;
    }
}

fn submit_prompt(state: &mut AppState, effects: &mut Vec<Effect>) {
    let Some(prompt) = state.fileops.prompt.take() else {
        close_prompt(state);
        return;
    };
    close_prompt(state);
    let text = prompt.input.text().to_owned();
    match prompt.kind {
        PromptKind::Rename => {
            let Some(from) = prompt.target else { return };
            if text.is_empty() {
                warn(state, effects, "A name cannot be empty");
            } else if from
                .file_name()
                .is_some_and(|n| n.to_string_lossy() == text)
            {
                set_status(state, Level::Info, "Name unchanged".into(), effects);
            } else {
                effects.push(Effect::Rename {
                    from,
                    new_name: text,
                });
            }
        }
        PromptKind::Create => {
            if text.trim().is_empty() {
                return;
            }
            effects.push(Effect::Create {
                base: state.cwd.clone(),
                input: text,
            });
        }
        PromptKind::Shell => {
            if text.trim().is_empty() {
                return;
            }
            let id = state.fileops.next_shell;
            state.fileops.next_shell += 1;
            state.fileops.pager = Some(Pager {
                id,
                command: text.clone(),
                cwd: state.cwd.clone(),
                lines: std::collections::VecDeque::new(),
                dropped: 0,
                scroll: 0,
                follow: true,
                status: PagerStatus::Running,
            });
            push_overlay(state, Overlay::Pager);
            effects.push(Effect::RunShell {
                id,
                command: text,
                cwd: state.cwd.clone(),
            });
        }
        PromptKind::Command => {}
    }
}

// ----- overlays: raw keys -----

fn is_plain(key: KeyChord) -> bool {
    key.mods == Modifiers::NONE
}

/// Keys that are text or dialog hot keys rather than keymap bindings. Returns true when the
/// key was consumed; `enter`, `esc` and `ctrl-c` always fall through to the keymap.
pub(crate) fn intercept_key(
    state: &mut AppState,
    key: KeyChord,
    effects: &mut Vec<Effect>,
) -> bool {
    let consumed = match state.overlays.last().copied() {
        Some(Overlay::Confirm) => confirm_key(state, key, effects),
        Some(Overlay::Conflict) => conflict_key(state, key, effects),
        Some(Overlay::Menu) => menu_key(state, key, effects),
        Some(_) => false,
        None => prompt_key(state, key),
    };
    if consumed {
        state.dirty = true;
    }
    consumed
}

fn prompt_key(state: &mut AppState, key: KeyChord) -> bool {
    let Mode::Prompt(kind) = state.mode else {
        return false;
    };
    if kind == PromptKind::Command {
        return false;
    }
    state
        .fileops
        .prompt
        .as_mut()
        .is_some_and(|p| p.input.handle_key(key))
}

fn confirm_key(state: &mut AppState, key: KeyChord, effects: &mut Vec<Effect>) -> bool {
    let Some(confirm) = state.fileops.confirm.as_mut() else {
        return false;
    };
    if confirm.token.is_some() {
        confirm.mismatch = false;
        return confirm.typed.handle_key(key);
    }
    // Plain yes / no questions answer to a single key as well as enter / esc.
    match key.key {
        Key::Char('y') if is_plain(key) && confirm.action.is_some() => {
            accept_confirm(state, effects);
            true
        }
        Key::Char('n') if is_plain(key) => {
            cancel_confirm(state, effects);
            true
        }
        _ => false,
    }
}

fn conflict_key(state: &mut AppState, key: KeyChord, effects: &mut Vec<Effect>) -> bool {
    let Some(dialog) = state.fileops.conflicts.front_mut() else {
        return false;
    };
    if !dialog.armed {
        // Typed before the dialog was usable (or still typing): swallow, and wait for a
        // quiet moment before arming.
        arm_later(state, effects);
        return true;
    }
    if let Some(input) = dialog.rename.as_mut() {
        return input.handle_key(key);
    }
    let Key::Char(c) = key.key else {
        return false;
    };
    if !is_plain(key) {
        return false;
    }
    match c {
        // Shift-O on purpose: the destructive choice must not be a letter that is also
        // part of ordinary typing. Plain `o` is swallowed.
        'O' => answer_conflict(state, effects, ConflictAnswer::Overwrite),
        'o' => {}
        's' => answer_conflict(state, effects, ConflictAnswer::Skip),
        'k' => answer_conflict(state, effects, ConflictAnswer::KeepBoth),
        'c' => answer_conflict(state, effects, ConflictAnswer::Cancel),
        'a' => dialog.apply_to_all = !dialog.apply_to_all,
        'r' => {
            let name = dialog
                .info
                .dst
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let cursor = rename_cursor(&name, dialog.info.dst_meta.is_dir);
            dialog.rename = Some(PromptInput::with_text(name, cursor));
        }
        _ => return false,
    }
    true
}

fn menu_key(state: &mut AppState, key: KeyChord, effects: &mut Vec<Effect>) -> bool {
    let Some(menu) = state.fileops.menu.as_mut() else {
        return false;
    };
    let Key::Char(c) = key.key else {
        return false;
    };
    if !is_plain(key) {
        return false;
    }
    let Some(index) = menu.items.iter().position(|i| i.hotkey == Some(c)) else {
        return false;
    };
    menu.selected = index;
    choose_menu_item(state, effects);
    true
}

// ----- engine messages -----

fn row_mut(state: &mut AppState, id: JobId) -> Option<&mut JobRow> {
    state.fileops.jobs.iter_mut().find(|r| r.id == id)
}

fn upsert_row(
    state: &mut AppState,
    id: JobId,
    kind: OpKind,
    description: String,
    status: JobStatus,
) {
    if let Some(row) = row_mut(state, id) {
        row.description = description;
        row.status = status;
        return;
    }
    // The highlight stays on the same job when a new row is inserted above it.
    if !state.fileops.jobs.is_empty() {
        state.fileops.jobs_selected += 1;
    }
    state.fileops.jobs.insert(
        0,
        JobRow {
            id,
            kind,
            description,
            status,
            scanning: false,
            progress: Default::default(),
            summary: None,
        },
    );
    // Forget the oldest finished jobs.
    while state.fileops.jobs.len() > JOB_HISTORY {
        match state
            .fileops
            .jobs
            .iter()
            .rposition(|r| !r.status.is_active())
        {
            Some(pos) => {
                state.fileops.jobs.remove(pos);
                if pos < state.fileops.jobs_selected {
                    state.fileops.jobs_selected -= 1;
                }
            }
            None => break,
        }
    }
    let last = state.fileops.jobs.len().saturating_sub(1);
    state.fileops.jobs_selected = state.fileops.jobs_selected.min(last);
}

pub(crate) fn on_job(state: &mut AppState, msg: JobMsg, effects: &mut Vec<Effect>) {
    state.dirty = true;
    match msg {
        JobMsg::Queued {
            id,
            kind,
            description,
        } => {
            state.jobs.insert(id);
            upsert_row(state, id, kind, description, JobStatus::Queued);
        }
        JobMsg::Started {
            id,
            kind,
            description,
        } => {
            state.jobs.insert(id);
            upsert_row(state, id, kind, description, JobStatus::Running);
        }
        JobMsg::Scanning { id, files, bytes } => {
            if let Some(row) = row_mut(state, id) {
                row.scanning = true;
                row.progress.files_total = files;
                row.progress.bytes_total = bytes;
            }
        }
        JobMsg::Progress { id, progress } => {
            if let Some(row) = row_mut(state, id) {
                row.scanning = false;
                row.progress = progress;
                if row.status == JobStatus::Queued {
                    row.status = JobStatus::Running;
                }
            }
        }
        JobMsg::Conflict(info) => {
            if let Some(row) = row_mut(state, info.job) {
                row.status = JobStatus::AwaitingConflict;
            }
            state.fileops.conflicts.push_back(ConflictDialog {
                info,
                apply_to_all: false,
                rename: None,
                armed: false,
            });
            push_overlay(state, Overlay::Conflict);
            if state.fileops.conflicts.len() == 1 {
                arm_later(state, effects);
            }
        }
        JobMsg::Finished(done) => finish_job(state, effects, &done),
    }
}

/// The toast for a finished job.
pub fn job_summary(done: &JobDone) -> (Level, String) {
    let n = done.items_done;
    let first = done
        .created
        .last()
        .or(done.removed.first())
        .map(|p| name_of(p));
    let subject = match (n, &first) {
        (1, Some(name)) => name.clone(),
        _ => items_label(n),
    };
    let skipped = if done.items_skipped > 0 {
        format!(", {} skipped", done.items_skipped)
    } else {
        String::new()
    };
    let bytes = if matches!(done.kind, OpKind::Copy | OpKind::Move | OpKind::Extract)
        && done.bytes_done > 0
    {
        format!(" ({})", human(done.bytes_done))
    } else {
        String::new()
    };
    let hint = if done.is_undo || matches!(done.kind, OpKind::Delete | OpKind::Restore) {
        ""
    } else {
        " · u to undo"
    };
    let (level, text) = match done.status {
        JobStatus::Cancelled => (
            Level::Warning,
            format!("{} cancelled ({n} done{skipped})", done.kind.verb()),
        ),
        JobStatus::Failed => {
            let first_error = done.errors.first().cloned().unwrap_or_default();
            let failed = done.errors.len();
            let ok = if n > 0 {
                format!("{} {subject}, ", done.kind.past().to_lowercase())
            } else {
                String::new()
            };
            (Level::Error, format!("{ok}{failed} failed: {first_error}"))
        }
        _ => {
            let text = if n == 0 && done.items_skipped > 0 {
                format!("Nothing {}{skipped}", done.kind.past().to_lowercase())
            } else {
                format!("{} {subject}{bytes}{skipped}{hint}", done.kind.past())
            };
            (Level::Info, text)
        }
    };
    if done.is_undo {
        (level, format!("Undo: {text}"))
    } else {
        (level, text)
    }
}

fn finish_job(state: &mut AppState, effects: &mut Vec<Effect>, done: &JobDone) {
    state.jobs.remove(&done.id);
    if done.kind == OpKind::Move && !done.is_undo && done.status == JobStatus::Done {
        // Consume only the cut items this job moved; a newer cut register stays.
        if let Some(register) = state.fileops.register.as_mut().filter(|r| r.cut) {
            register.paths.retain(|p| {
                !done
                    .removed
                    .iter()
                    .chain(&done.consumed_sources)
                    .any(|m| same_path(p, m))
            });
            if register.paths.is_empty() {
                state.fileops.register = None;
            }
        }
    }
    let (level, text) = job_summary(done);
    if let Some(row) = row_mut(state, done.id) {
        row.status = done.status;
        row.scanning = false;
        row.summary = Some(text.clone());
    } else {
        // A job we never saw start (events are lossy only on shutdown).
        upsert_row(state, done.id, done.kind, text.clone(), done.status);
    }
    // Conflict dialogs of a finished job are moot.
    state.fileops.conflicts.retain(|d| d.info.job != done.id);
    if state.fileops.conflicts.is_empty() {
        remove_overlay(state, Overlay::Conflict);
    } else {
        ensure_arming(state, effects);
    }
    set_status(state, level, text, effects);
    crate::preview::invalidate_paths(state, &done.created, &done.removed, effects);
    crate::search::cache_after_job(effects, &done.created, &done.removed);
    refresh_after(state, effects, done);
}

/// Re-lists the current directory right away when the job touched it, and puts the cursor on
/// the new item. (The watcher would catch up later; this is the "instant" path.)
fn refresh_after(state: &mut AppState, effects: &mut Vec<Effect>, done: &JobDone) {
    if state.show_drives {
        return;
    }
    let cwd = state.cwd.clone();
    // A results list is never re-listed, but rows the job deleted or moved away are pruned.
    if state.view.label.is_some() {
        let names: Vec<std::ffi::OsString> = done
            .removed
            .iter()
            .filter_map(|p| p.strip_prefix(&cwd).ok())
            .map(|rel| rel.as_os_str().to_owned())
            .collect();
        let height = state.viewport_height();
        // Moves and renames pair their removed source with the created target: the row
        // keeps its place in the list under the new name when the target is below the cwd.
        let mut renames: Vec<(std::ffi::OsString, std::ffi::OsString)> = Vec::new();
        if matches!(
            done.kind,
            OpKind::Move | OpKind::Rename | OpKind::BulkRename
        ) && done.removed.len() == done.created.len()
        {
            for (from, to) in done.removed.iter().zip(&done.created) {
                if let (Ok(old), Ok(new)) = (from.strip_prefix(&cwd), to.strip_prefix(&cwd)) {
                    renames.push((old.as_os_str().to_owned(), new.as_os_str().to_owned()));
                }
            }
        }
        let names: Vec<_> = names
            .into_iter()
            .filter(|n| !renames.iter().any(|(old, _)| old == n))
            .collect();
        state.view.rename_names(&renames, height);
        state.view.remove_names(&names, height);
        state.dirty = true;
        return;
    }
    let in_cwd = |p: &&PathBuf| p.parent().is_some_and(|parent| same_path(parent, &cwd));
    let touched =
        done.created.iter().any(|p| in_cwd(&p)) || done.removed.iter().any(|p| in_cwd(&p));
    if !touched {
        return;
    }
    // The reveal is only set when the listing that applies it is actually requested.
    if !nav::can_refresh(state) {
        return;
    }
    if let Some(target) = done.created.iter().rev().find(|p| in_cwd(p)) {
        if let Some(name) = target.file_name() {
            state.view.set_reveal(Some(name.to_owned()));
        }
    }
    nav::refresh_if_idle(state, effects);
}

pub(crate) fn on_shell(state: &mut AppState, msg: ShellMsg) {
    let rows = pager_rows(state.size);
    let Some(pager) = state.fileops.pager.as_mut() else {
        return;
    };
    let id = match &msg {
        ShellMsg::Line { id, .. }
        | ShellMsg::Exited { id, .. }
        | ShellMsg::Cancelled { id }
        | ShellMsg::SpawnFailed { id, .. } => *id,
    };
    if pager.id != id {
        return;
    }
    match msg {
        ShellMsg::Line { text, stderr, .. } => {
            pager.lines.push_back(PagerLine { text, stderr });
            if pager.lines.len() > PAGER_MAX_LINES {
                pager.lines.pop_front();
                pager.dropped += 1;
                pager.scroll = pager.scroll.saturating_sub(1);
            }
        }
        ShellMsg::Exited { code, .. } => pager.status = PagerStatus::Exited(code),
        ShellMsg::Cancelled { .. } => pager.status = PagerStatus::Cancelled,
        ShellMsg::SpawnFailed { error, .. } => pager.status = PagerStatus::Failed(error),
    }
    if pager.follow {
        pager.scroll = pager.lines.len().saturating_sub(rows);
    }
    state.dirty = true;
}

pub(crate) fn on_foreground_request(
    state: &mut AppState,
    request: ForegroundRequest,
    effects: &mut Vec<Effect>,
) {
    let id = state.fileops.next_fg;
    state.fileops.next_fg += 1;
    state.fileops.fg = Some(FgPending {
        id,
        purpose: request.purpose,
    });
    effects.push(Effect::RunForeground {
        id,
        spec: request.spec,
    });
}

pub(crate) fn on_foreground_done(
    state: &mut AppState,
    id: u64,
    error: Option<String>,
    effects: &mut Vec<Effect>,
) {
    state.dirty = true;
    let Some(pending) = state.fileops.fg.take_if(|p| p.id == id) else {
        return;
    };
    match pending.purpose {
        ForegroundPurpose::Editor => {
            if let Some(error) = error {
                set_status(
                    state,
                    Level::Error,
                    format!("Editor failed: {error}"),
                    effects,
                );
            }
            if !state.show_drives {
                nav::refresh_if_idle(state, effects);
            }
        }
        ForegroundPurpose::BulkEdit { dir, names, file } => {
            effects.push(Effect::BulkRenamePlan {
                dir,
                names,
                file,
                error,
            });
        }
    }
}

pub(crate) fn on_open_handlers(
    state: &mut AppState,
    paths: Vec<PathBuf>,
    names: Vec<String>,
    effects: &mut Vec<Effect>,
) {
    let Some(first) = paths.first() else {
        return;
    };
    if names.is_empty() {
        warn(state, effects, "No programs to open this with");
        return;
    }
    let items = names
        .into_iter()
        .enumerate()
        .map(|(i, label)| MenuItem {
            label,
            hotkey: char::from_digit(u32::try_from(i + 1).unwrap_or(10), 10).filter(|c| *c != '0'),
        })
        .collect();
    state.fileops.menu = Some(Menu {
        kind: MenuKind::OpenWith,
        title: match paths.len() {
            1 => format!("Open {} with", name_of(first)),
            n => format!("Open {n} items with"),
        },
        items,
        selected: 0,
        paths,
    });
    push_overlay(state, Overlay::Menu);
    state.dirty = true;
}

pub(crate) fn on_bulk_planned(
    state: &mut AppState,
    result: BulkPlanResult,
    effects: &mut Vec<Effect>,
) {
    match result {
        BulkPlanResult::Unchanged => {
            set_status(state, Level::Info, "No names changed".into(), effects);
        }
        BulkPlanResult::Failed(error) => {
            set_status(
                state,
                Level::Error,
                format!("Bulk rename: {error}"),
                effects,
            );
        }
        BulkPlanResult::Planned { preview, steps } => {
            discard_reedit_if_any(state, effects);
            let count = preview.len();
            let more = count.saturating_sub(CONFIRM_LINES);
            state.fileops.confirm = Some(ConfirmDialog {
                title: format!("Rename {}?", items_label(count)),
                lines: preview.into_iter().take(CONFIRM_LINES).collect(),
                more,
                danger: false,
                token: None,
                typed: PromptInput::new(),
                mismatch: false,
                action: Some(ConfirmAction::BulkRename { steps }),
                accept_label: "rename".to_owned(),
            });
            push_overlay(state, Overlay::Confirm);
        }
        BulkPlanResult::Invalid { problems, retry } => {
            discard_reedit_except(state, retry.as_ref().map(|r| r.file.as_path()), effects);
            let count = problems.len();
            let more = count.saturating_sub(CONFIRM_LINES);
            let mut lines: Vec<String> = problems.into_iter().take(CONFIRM_LINES).collect();
            if retry.is_some() {
                lines.push("Your edits are kept: enter reopens the file, esc discards it.".into());
            }
            state.fileops.confirm = Some(ConfirmDialog {
                title: format!("Bulk rename: {} found", plural(count, "problem")),
                lines,
                more,
                danger: true,
                token: None,
                typed: PromptInput::new(),
                mismatch: false,
                action: retry.map(ConfirmAction::BulkReedit),
                accept_label: "edit again".to_owned(),
            });
            push_overlay(state, Overlay::Confirm);
        }
    }
    state.dirty = true;
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

#[cfg(test)]
mod tests;

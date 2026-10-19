//! Every user-invocable action, as data.
//!
//! The keymap, the command palette, the help overlay and `docs/keymap.md` are all generated
//! from this list, so adding a feature means adding a variant here first.

use std::fmt;

macro_rules! actions {
    ($($variant:ident => $id:literal, $title:literal;)+) => {
        /// A named thing the user can do. Plain data; behaviour lives in `update`.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Action {
            $($variant,)+
        }

        impl Action {
            /// Every action, in palette order.
            pub const ALL: &'static [Action] = &[$(Action::$variant,)+];

            /// Stable machine id used in `config.toml` (`"cursor.down"`).
            pub const fn id(self) -> &'static str {
                match self {
                    $(Action::$variant => $id,)+
                }
            }

            /// Human readable title shown in the palette and help.
            pub const fn title(self) -> &'static str {
                match self {
                    $(Action::$variant => $title,)+
                }
            }

            /// Looks an action up by its [`id`](Self::id).
            pub fn from_id(id: &str) -> Option<Action> {
                match id {
                    $($id => Some(Action::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

actions! {
    // Movement
    CursorDown => "cursor.down", "Move cursor down";
    CursorUp => "cursor.up", "Move cursor up";
    DirParent => "dir.parent", "Go to parent directory";
    DirEnter => "dir.enter", "Enter directory / open file";
    CursorTop => "cursor.top", "Jump to top";
    CursorBottom => "cursor.bottom", "Jump to bottom";
    PageHalfDown => "page.half_down", "Scroll half a page down";
    PageHalfUp => "page.half_up", "Scroll half a page up";
    HistoryBack => "history.back", "History: back";
    HistoryForward => "history.forward", "History: forward";
    GoHome => "go.home", "Go to home directory";
    GoDownloads => "go.downloads", "Go to Downloads";
    GoDesktop => "go.desktop", "Go to Desktop";
    GoConfig => "go.config", "Go to config directory";
    GoDrives => "go.drives", "Go to This PC (drives)";
    // Search and overlays
    FilterOpen => "filter.open", "Filter current directory";
    FindFile => "find.file", "Find file (recursive fuzzy)";
    SearchContents => "search.contents", "Search file contents";
    PaletteOpen => "palette.open", "Command palette";
    HelpOpen => "help.open", "Help";
    JobsOpen => "jobs.open", "Jobs overlay";
    JobCancel => "job.cancel", "Cancel selected job";
    JobsClear => "jobs.clear", "Clear finished jobs";
    // Find file / content search overlays
    FinderScope => "finder.scope", "Find file: cycle scope (cwd, git root, home, drive)";
    FinderHidden => "finder.hidden", "Find file: toggle hidden files";
    FinderGitignore => "finder.gitignore", "Find file: toggle gitignored files";
    FinderOpen => "finder.open", "Find file: open with default app";
    FinderMark => "finder.mark", "Find file: mark result";
    GrepRegex => "grep.regex", "Search contents: toggle regex";
    GrepWholeWord => "grep.word", "Search contents: toggle whole word";
    GrepNextField => "grep.field", "Search contents: switch query/glob field";
    GrepToList => "grep.to_list", "Search contents: open results as a list";
    GrepMark => "grep.mark", "Search contents: mark the hit";
    GrepSendClaude => "grep.send_claude", "Search contents: send the hit to Claude";
    // Selection
    MarkToggle => "mark.toggle", "Toggle mark";
    VisualToggle => "mode.visual", "Visual (range select) mode";
    MarkAll => "mark.all", "Mark all";
    // File operations
    Copy => "clipboard.copy", "Copy";
    Cut => "clipboard.cut", "Cut";
    Paste => "clipboard.paste", "Paste";
    CopyPath => "path.copy", "Copy path to clipboard";
    Rename => "file.rename", "Rename";
    BulkRename => "file.bulk_rename", "Bulk rename in editor";
    Create => "file.create", "Create file or directory";
    Trash => "file.trash", "Move to Recycle Bin";
    DeletePermanent => "file.delete", "Delete permanently";
    Extract => "file.extract", "Extract archive";
    Undo => "file.undo", "Undo last file operation";
    Open => "open.default", "Open with default app";
    OpenWith => "open.with", "Open with...";
    OpenEditor => "open.editor", "Open in editor";
    ShellCommand => "shell.run", "Run shell command";
    // View
    HiddenToggle => "view.hidden", "Toggle hidden files";
    SortName => "sort.name", "Sort by name";
    SortSize => "sort.size", "Sort by size";
    SortMtime => "sort.mtime", "Sort by modified time";
    SortExt => "sort.ext", "Sort by extension";
    PreviewToggle => "preview.toggle", "Toggle preview";
    ParentColumnToggle => "preview.parent_column", "Toggle parent column";
    MetadataToggle => "preview.metadata", "Toggle metadata view";
    PreviewScrollDown => "preview.scroll_down", "Scroll preview down";
    PreviewScrollUp => "preview.scroll_up", "Scroll preview up";
    PreviewSelectLines => "preview.select_lines", "Select preview lines (visual lines)";
    PreviewLinesSend => "preview.lines_send", "Send the selected preview lines to Claude";
    // Navigation aids
    BookmarkSet => "bookmark.set", "Set bookmark";
    BookmarkJump => "bookmark.jump", "Jump to bookmark";
    BookmarkList => "bookmark.list", "List bookmarks";
    BookmarkDelete => "bookmark.delete", "Delete bookmark";
    FrecencyJump => "frecency.jump", "Frecency jump";
    // Tabs
    TabNew => "tab.new", "New tab";
    TabGoto => "tab.goto", "Switch to tab";
    TabClose => "tab.close", "Close tab";
    // Claude Code (phase 10)
    ClaudeOpen => "claude.open", "Open Claude";
    ClaudeSendMarked => "claude.send_marked", "Send marked files to Claude";
    ClaudeResume => "claude.resume", "Resume last Claude session";
    ClaudeSplit => "claude.split", "Open Claude in Windows Terminal split";
    ClaudeFocus => "claude.focus", "Toggle focus between explorer and Claude";
    ClaudeClose => "claude.close", "Close Claude pane";
    ClaudeGrow => "claude.grow", "Grow Claude pane";
    ClaudeShrink => "claude.shrink", "Shrink Claude pane";
    ClaudePosition => "claude.position", "Move Claude pane (bottom / right)";
    ClaudeScrollUp => "claude.scroll_up", "Scroll Claude pane up";
    ClaudeScrollDown => "claude.scroll_down", "Scroll Claude pane down";
    ClaudeChanges => "claude.changes", "Claude changes";
    ClaudeChangesClear => "claude.changes_clear", "Clear Claude changes";
    ClaudeProceed => "claude.proceed", "Proceed with API billing anyway";
    ClaudeChangesDiff => "claude.changes_diff", "Toggle the git diff in the Claude changes view";
    // Generic
    Cancel => "cancel", "Cancel / close";
    Accept => "accept", "Accept / confirm";
    ConfigReload => "config.reload", "Reload config";
    ThemeSwitch => "theme.switch", "Switch theme";
    IconsSwitch => "icons.switch", "Switch icon set";
    Quit => "quit", "Quit";
    QuitCd => "quit.cd", "Quit and cd shell to current directory";
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn ids_are_unique_and_round_trip() {
        let mut seen = HashSet::new();
        for &action in Action::ALL {
            assert!(seen.insert(action.id()), "duplicate id {}", action.id());
            assert_eq!(Action::from_id(action.id()), Some(action));
            assert!(!action.title().is_empty());
        }
        assert_eq!(Action::from_id("nope"), None);
    }
}

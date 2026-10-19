//! The command registry: category and description for every [`Action`].
//!
//! The id and title live with the action itself ([`Action::id`], [`Action::title`]); this
//! module adds the rest of the data the palette, the help overlay and `docs/keymap.md` need.
//! It is a plain table so that a new action only needs one more row (a test fails until it
//! has one).

use crate::action::Action;

/// Heading under which the help overlay and the docs group commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    Navigation,
    Search,
    Selection,
    Files,
    View,
    TabsAndBookmarks,
    Claude,
    App,
}

impl Category {
    /// In display order.
    pub const ALL: &'static [Category] = &[
        Category::Navigation,
        Category::Search,
        Category::Selection,
        Category::Files,
        Category::View,
        Category::TabsAndBookmarks,
        Category::Claude,
        Category::App,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Category::Navigation => "Navigation",
            Category::Search => "Search",
            Category::Selection => "Selection",
            Category::Files => "File operations",
            Category::View => "View",
            Category::TabsAndBookmarks => "Tabs and bookmarks",
            Category::Claude => "Claude Code",
            Category::App => "Application",
        }
    }
}

use Category as C;

/// `(action, category, description)` for every action.
const COMMANDS: &[(Action, Category, &str)] = &[
    (
        Action::CursorDown,
        C::Navigation,
        "Move the cursor one row down.",
    ),
    (
        Action::CursorUp,
        C::Navigation,
        "Move the cursor one row up.",
    ),
    (
        Action::DirParent,
        C::Navigation,
        "Go to the parent directory, keeping the cursor on the directory you left.",
    ),
    (
        Action::DirEnter,
        C::Navigation,
        "Enter the directory under the cursor, or open the file.",
    ),
    (Action::CursorTop, C::Navigation, "Jump to the first entry."),
    (
        Action::CursorBottom,
        C::Navigation,
        "Jump to the last entry.",
    ),
    (
        Action::PageHalfDown,
        C::Navigation,
        "Scroll half a page down.",
    ),
    (Action::PageHalfUp, C::Navigation, "Scroll half a page up."),
    (
        Action::HistoryBack,
        C::Navigation,
        "Go back to the previous directory.",
    ),
    (
        Action::HistoryForward,
        C::Navigation,
        "Go forward again after going back.",
    ),
    (
        Action::GoHome,
        C::Navigation,
        "Jump to your home directory.",
    ),
    (
        Action::GoDownloads,
        C::Navigation,
        "Jump to the Downloads folder.",
    ),
    (
        Action::GoDesktop,
        C::Navigation,
        "Jump to the Desktop folder.",
    ),
    (
        Action::GoConfig,
        C::Navigation,
        "Jump to Kestrel's own config directory.",
    ),
    (
        Action::GoDrives,
        C::Navigation,
        "Show all drives with free space.",
    ),
    (
        Action::FilterOpen,
        C::Search,
        "Fuzzy-filter the current directory as you type.",
    ),
    (
        Action::FindFile,
        C::Search,
        "Find a file by fuzzy path match, recursively.",
    ),
    (
        Action::SearchContents,
        C::Search,
        "Search inside files (live grep).",
    ),
    (
        Action::FinderScope,
        C::Search,
        "Find file: cycle the search scope (current directory, git root, home, drive).",
    ),
    (
        Action::FinderHidden,
        C::Search,
        "Find file: include or skip hidden files.",
    ),
    (
        Action::FinderGitignore,
        C::Search,
        "Find file: include or skip files ignored by git.",
    ),
    (
        Action::FinderOpen,
        C::Search,
        "Find file: open the marked results, or the highlighted one, with the default app.",
    ),
    (
        Action::FinderMark,
        C::Search,
        "Find file: mark the highlighted result and move down.",
    ),
    (
        Action::GrepRegex,
        C::Search,
        "Search contents: toggle regular-expression matching.",
    ),
    (
        Action::GrepWholeWord,
        C::Search,
        "Search contents: toggle whole-word matching.",
    ),
    (
        Action::GrepNextField,
        C::Search,
        "Search contents: switch between the query and glob fields.",
    ),
    (
        Action::GrepToList,
        C::Search,
        "Search contents: show the files with matches as a results list in a new tab.",
    ),
    (
        Action::GrepMark,
        C::Search,
        "Search contents: mark the highlighted hit so alt-c sends all marked hits to Claude.",
    ),
    (
        Action::GrepSendClaude,
        C::Claude,
        "Search contents: send the highlighted hit to the Claude pane as an @path#Lnn mention.",
    ),
    (Action::PaletteOpen, C::App, "Open the command palette."),
    (
        Action::HelpOpen,
        C::App,
        "Show all key bindings and commands.",
    ),
    (Action::JobsOpen, C::App, "Show running background jobs."),
    (
        Action::JobCancel,
        C::App,
        "Cancel the selected job in the jobs overlay.",
    ),
    (
        Action::JobsClear,
        C::App,
        "Drop finished jobs from the jobs overlay.",
    ),
    (
        Action::MarkToggle,
        C::Selection,
        "Mark or unmark the entry under the cursor.",
    ),
    (
        Action::VisualToggle,
        C::Selection,
        "Mark a range of entries while moving the cursor.",
    ),
    (Action::MarkAll, C::Selection, "Mark every visible entry."),
    (
        Action::Copy,
        C::Files,
        "Copy the marked entries (or the one under the cursor).",
    ),
    (
        Action::Cut,
        C::Files,
        "Cut the marked entries to move them.",
    ),
    (
        Action::Paste,
        C::Files,
        "Paste copied or cut entries into this directory.",
    ),
    (
        Action::CopyPath,
        C::Files,
        "Copy absolute path(s) to the clipboard.",
    ),
    (
        Action::Rename,
        C::Files,
        "Rename the entry under the cursor.",
    ),
    (
        Action::BulkRename,
        C::Files,
        "Rename many entries by editing their names in your editor.",
    ),
    (
        Action::Create,
        C::Files,
        "Create a file; a trailing slash creates a directory.",
    ),
    (
        Action::Trash,
        C::Files,
        "Move the entries to the Recycle Bin.",
    ),
    (
        Action::DeletePermanent,
        C::Files,
        "Delete the entries for good after typed confirmation.",
    ),
    (
        Action::Extract,
        C::Files,
        "Extract ZIP and RAR archives here; several top-level items get their own folder.",
    ),
    (
        Action::Undo,
        C::Files,
        "Undo the last rename, move, create or trash.",
    ),
    (Action::Open, C::Files, "Open with the default application."),
    (
        Action::OpenWith,
        C::Files,
        "Choose which application opens the file.",
    ),
    (Action::OpenEditor, C::Files, "Open in your editor."),
    (
        Action::ShellCommand,
        C::Files,
        "Run a shell command in this directory.",
    ),
    (Action::HiddenToggle, C::View, "Show or hide hidden files."),
    (Action::SortName, C::View, "Sort by name; again to reverse."),
    (Action::SortSize, C::View, "Sort by size; again to reverse."),
    (
        Action::SortMtime,
        C::View,
        "Sort by modified time; again to reverse.",
    ),
    (
        Action::SortExt,
        C::View,
        "Sort by extension; again to reverse.",
    ),
    (
        Action::PreviewToggle,
        C::View,
        "Show or hide the preview pane.",
    ),
    (
        Action::ParentColumnToggle,
        C::View,
        "Show or hide the parent directory column.",
    ),
    (
        Action::MetadataToggle,
        C::View,
        "Switch the preview between content and metadata.",
    ),
    (
        Action::PreviewSelectLines,
        C::Claude,
        "Select lines of the text preview (j/k extend, esc cancels) to send as @path#La-b to Claude.",
    ),
    (
        Action::PreviewLinesSend,
        C::Claude,
        "Send the selected preview lines to the Claude pane as an @path#La-b mention.",
    ),
    (
        Action::PreviewScrollDown,
        C::View,
        "Scroll the preview down.",
    ),
    (Action::PreviewScrollUp, C::View, "Scroll the preview up."),
    (
        Action::BookmarkSet,
        C::TabsAndBookmarks,
        "Remember this directory under a key.",
    ),
    (
        Action::BookmarkJump,
        C::TabsAndBookmarks,
        "Jump to the directory saved under a key.",
    ),
    (
        Action::BookmarkList,
        C::TabsAndBookmarks,
        "List the saved bookmarks and jump to one.",
    ),
    (
        Action::BookmarkDelete,
        C::TabsAndBookmarks,
        "Delete the selected bookmark in the bookmark list.",
    ),
    (
        Action::FrecencyJump,
        C::TabsAndBookmarks,
        "Jump to a frequently or recently used directory.",
    ),
    (
        Action::TabNew,
        C::TabsAndBookmarks,
        "Open a new tab in this directory.",
    ),
    (
        Action::TabGoto,
        C::TabsAndBookmarks,
        "Switch to a numbered tab.",
    ),
    (
        Action::TabClose,
        C::TabsAndBookmarks,
        "Close the current tab.",
    ),
    (
        Action::ClaudeOpen,
        C::Claude,
        "Open Claude Code beside the explorer.",
    ),
    (
        Action::ClaudeSendMarked,
        C::Claude,
        "Send the marked files to Claude as @ mentions.",
    ),
    (
        Action::ClaudeResume,
        C::Claude,
        "Resume the last Claude session of this project.",
    ),
    (
        Action::ClaudeSplit,
        C::Claude,
        "Open Claude in a Windows Terminal split pane.",
    ),
    (
        Action::ClaudeFocus,
        C::Claude,
        "Move the keyboard between the explorer and the Claude pane.",
    ),
    (
        Action::ClaudeClose,
        C::Claude,
        "Close the Claude pane and end its session process.",
    ),
    (
        Action::ClaudeGrow,
        C::Claude,
        "Make the Claude pane larger.",
    ),
    (
        Action::ClaudeShrink,
        C::Claude,
        "Make the Claude pane smaller.",
    ),
    (
        Action::ClaudePosition,
        C::Claude,
        "Move the Claude pane between the bottom and the right.",
    ),
    (
        Action::ClaudeScrollUp,
        C::Claude,
        "Scroll the Claude pane towards older output.",
    ),
    (
        Action::ClaudeScrollDown,
        C::Claude,
        "Scroll the Claude pane towards newer output.",
    ),
    (
        Action::ClaudeChanges,
        C::Claude,
        "List the files Claude changed, with their git diff.",
    ),
    (
        Action::ClaudeChangesClear,
        C::Claude,
        "Forget the list of files Claude changed.",
    ),
    (
        Action::ClaudeChangesDiff,
        C::Claude,
        "Show or hide the git diff of the selected changed file.",
    ),
    (
        Action::ClaudeProceed,
        C::Claude,
        "Launch Claude although it would bill the API key (billing question only).",
    ),
    (
        Action::Cancel,
        C::App,
        "Close the top overlay, leave the mode, clear marks.",
    ),
    (Action::Accept, C::App, "Confirm in prompts and overlays."),
    (
        Action::ConfigReload,
        C::App,
        "Re-read config.toml, the keymap and the theme.",
    ),
    (
        Action::ThemeSwitch,
        C::App,
        "Choose a colour theme for this session.",
    ),
    (
        Action::IconsSwitch,
        C::App,
        "Choose nerd, unicode or ascii icons for this session.",
    ),
    (Action::Quit, C::App, "Quit Kestrel."),
    (
        Action::QuitCd,
        C::App,
        "Quit and make the shell wrapper cd to this directory.",
    ),
];

/// Everything the UI shows about a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandInfo {
    pub action: Action,
    pub id: &'static str,
    pub title: &'static str,
    pub category: Category,
    pub description: &'static str,
}

/// Registry data for `action`. An action missing from the table (caught by a test) is
/// listed under [`Category::App`] without a description rather than breaking the UI.
pub fn info(action: Action) -> CommandInfo {
    let (category, description) = COMMANDS
        .iter()
        .find(|(a, _, _)| *a == action)
        .map_or((Category::App, ""), |&(_, c, d)| (c, d));
    CommandInfo {
        action,
        id: action.id(),
        title: action.title(),
        category,
        description,
    }
}

/// Every command, in palette order.
pub fn all() -> impl Iterator<Item = CommandInfo> {
    Action::ALL.iter().map(|&a| info(a))
}

/// Actions bound as `<prefix> <char>` need a character after the keys; run from the palette
/// they arm the key prefix instead of running directly.
pub const fn takes_char(action: Action) -> bool {
    matches!(action, Action::BookmarkSet | Action::BookmarkJump)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_action_has_exactly_one_documented_row() {
        let mut seen = HashSet::new();
        for (action, _, description) in COMMANDS {
            assert!(seen.insert(*action), "{action} listed twice");
            assert!(!description.is_empty(), "{action} has no description");
        }
        for &action in Action::ALL {
            assert!(seen.contains(&action), "{action} missing from COMMANDS");
        }
    }

    #[test]
    fn info_combines_action_and_table_data() {
        let i = info(Action::Trash);
        assert_eq!(i.id, "file.trash");
        assert_eq!(i.category, Category::Files);
        assert!(i.description.contains("Recycle"));
    }
}

//! Data types of the file-operation feature: the messages the effect runner sends, the
//! requests `update` hands back to it, and the state the overlays are drawn from.
//!
//! `ks-core` cannot depend on `ks-fs` (the dependency points the other way), so the engine's
//! events are mirrored here as plain `Eq` data; `ks-app` maps them 1:1.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::SystemTime;

use super::input::PromptInput;
use crate::mode::PromptKind;
use crate::state::JobId;

// ----- engine mirror -----

/// What a job does (mirror of the engine's `JobKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    Copy,
    Move,
    Trash,
    Delete,
    Create,
    Rename,
    BulkRename,
    Restore,
    Extract,
}

impl OpKind {
    /// "Copying"
    pub const fn verb(self) -> &'static str {
        match self {
            OpKind::Copy => "Copying",
            OpKind::Move => "Moving",
            OpKind::Trash => "Trashing",
            OpKind::Delete => "Deleting",
            OpKind::Create => "Creating",
            OpKind::Rename => "Renaming",
            OpKind::BulkRename => "Bulk renaming",
            OpKind::Restore => "Restoring",
            OpKind::Extract => "Extracting",
        }
    }

    /// "Copied"
    pub const fn past(self) -> &'static str {
        match self {
            OpKind::Copy => "Copied",
            OpKind::Move => "Moved",
            OpKind::Trash => "Moved to Recycle Bin:",
            OpKind::Delete => "Deleted",
            OpKind::Create => "Created",
            OpKind::Rename => "Renamed",
            OpKind::BulkRename => "Renamed",
            OpKind::Restore => "Restored",
            OpKind::Extract => "Extracted",
        }
    }
}

/// Lifecycle of a job as the Jobs overlay shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStatus {
    Queued,
    Running,
    AwaitingConflict,
    Done,
    /// Finished, but at least one item failed.
    Failed,
    Cancelled,
}

impl JobStatus {
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            JobStatus::Queued | JobStatus::Running | JobStatus::AwaitingConflict
        )
    }
}

/// Progress snapshot of a running job.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobProgress {
    pub bytes_done: u64,
    /// 0 while unknown.
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
    pub current: Option<PathBuf>,
    pub throughput_bps: u64,
    pub eta_secs: Option<u64>,
}

impl JobProgress {
    /// Completed fraction in percent (0..=100), when a total is known.
    pub fn percent(&self) -> Option<u8> {
        let (done, total) = if self.bytes_total > 0 {
            (self.bytes_done, self.bytes_total)
        } else if self.files_total > 0 {
            (self.files_done, self.files_total)
        } else {
            return None;
        };
        let pct = (u128::from(done.min(total)) * 100) / u128::from(total);
        Some(u8::try_from(pct).unwrap_or(100))
    }
}

/// Metadata shown in the conflict dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemMeta {
    pub is_dir: bool,
    pub is_link: bool,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

/// A job paused on an existing destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictInfo {
    pub job: JobId,
    pub src: PathBuf,
    pub dst: PathBuf,
    pub src_meta: ItemMeta,
    pub dst_meta: ItemMeta,
}

/// The final report of a job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobDone {
    pub id: JobId,
    pub kind: OpKind,
    /// `Done`, `Failed` or `Cancelled`.
    pub status: JobStatus,
    pub items_done: usize,
    pub items_skipped: usize,
    pub bytes_done: u64,
    /// One line per item error.
    pub errors: Vec<String>,
    /// Paths that now exist because of the job (copies, move/rename targets, created and
    /// restored items), in execution order.
    pub created: Vec<PathBuf>,
    /// Paths that are gone because of the job (trashed, deleted, move/rename sources).
    pub removed: Vec<PathBuf>,
    /// Move sources that are gone, including ones whose destination was replaced or merged
    /// (those appear in neither `created` nor `removed`). Consumes the cut register.
    pub consumed_sources: Vec<PathBuf>,
    /// The job was started by `u` (it is not itself undoable).
    pub is_undo: bool,
    pub elapsed_ms: u64,
}

/// A job event of the engine, as a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobMsg {
    Queued {
        id: JobId,
        kind: OpKind,
        description: String,
    },
    Started {
        id: JobId,
        kind: OpKind,
        description: String,
    },
    Scanning {
        id: JobId,
        files: u64,
        bytes: u64,
    },
    Progress {
        id: JobId,
        progress: JobProgress,
    },
    Conflict(ConflictInfo),
    Finished(JobDone),
}

/// Events of a `!` shell run, keyed by the id the pager was opened with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellMsg {
    Line { id: u64, text: String, stderr: bool },
    Exited { id: u64, code: Option<i32> },
    Cancelled { id: u64 },
    SpawnFailed { id: u64, error: String },
}

// ----- requests to the effect runner -----

/// A job `update` wants started. Name validation (rename, create) happens in the runner
/// because it needs the engine; see `Effect::Rename` and `Effect::Create`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobRequest {
    /// `(source, exact destination path)` pairs.
    Copy {
        items: Vec<(PathBuf, PathBuf)>,
    },
    Move {
        items: Vec<(PathBuf, PathBuf)>,
    },
    Trash {
        paths: Vec<PathBuf>,
    },
    DeletePermanent {
        paths: Vec<PathBuf>,
    },
    /// Ordered steps of a validated bulk-rename plan.
    BulkRename {
        steps: Vec<(PathBuf, PathBuf)>,
    },
    /// Archives to extract into `dest_dir`.
    Extract {
        archives: Vec<PathBuf>,
        dest_dir: PathBuf,
    },
}

/// The user's answer to a conflict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConflictAnswer {
    Overwrite,
    Skip,
    Rename(String),
    KeepBoth,
    /// Abort the whole job.
    Cancel,
}

/// How copied paths are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathFormatKind {
    Absolute,
    Relative,
    Name,
}

/// A program to run attached to the terminal; the TUI is suspended meanwhile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForegroundSpec {
    pub program: String,
    pub args: Vec<String>,
}

/// Why a foreground program runs; decides what happens when it ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForegroundPurpose {
    /// `e`: a terminal editor. Afterwards the listing is refreshed.
    Editor,
    /// `R`: the editor session of a bulk rename. Afterwards the edited file is planned.
    BulkEdit {
        dir: PathBuf,
        names: Vec<String>,
        file: PathBuf,
    },
}

/// Runner to `update`: please suspend the terminal and run this (`Msg::Foreground`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForegroundRequest {
    pub spec: ForegroundSpec,
    pub purpose: ForegroundPurpose,
}

/// What it takes to reopen the edit file of a bulk rename whose plan was invalid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BulkRetry {
    pub dir: PathBuf,
    pub names: Vec<String>,
    pub file: PathBuf,
}

/// Result of validating the edited bulk-rename file (`Msg::BulkPlanned`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BulkPlanResult {
    /// Nothing changed.
    Unchanged,
    Planned {
        /// `old -> new` lines.
        preview: Vec<String>,
        steps: Vec<(PathBuf, PathBuf)>,
    },
    Invalid {
        problems: Vec<String>,
        /// The kept edit file, so it can be opened again with the mistakes still in it.
        /// `None` when there is nothing to reopen.
        retry: Option<BulkRetry>,
    },
    /// The edit file could not be read or the editor failed.
    Failed(String),
}

// ----- state -----

/// The yank register.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Register {
    pub paths: Vec<PathBuf>,
    pub cut: bool,
}

/// A job as listed by the Jobs overlay and summarised in the prompt line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobRow {
    pub id: JobId,
    pub kind: OpKind,
    pub description: String,
    pub status: JobStatus,
    /// The pre-scan is still counting files.
    pub scanning: bool,
    pub progress: JobProgress,
    /// One-line result of a finished job.
    pub summary: Option<String>,
}

/// An open conflict dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictDialog {
    pub info: ConflictInfo,
    /// Reuse the answer for the job's remaining conflicts.
    pub apply_to_all: bool,
    /// The inline name prompt of the "rename" choice, while it is open.
    pub rename: Option<PromptInput>,
    /// Whether the dialog reacts to its hot keys. A dialog opens disarmed and is armed by a
    /// short timer (`TimerKind::ConflictArm`), so keys typed before it was visible (a rename
    /// prompt, a search query) are swallowed instead of answering the question.
    pub armed: bool,
}

/// What a confirmed dialog does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfirmAction {
    DeletePermanent {
        paths: Vec<PathBuf>,
    },
    BulkRename {
        steps: Vec<(PathBuf, PathBuf)>,
    },
    /// The bulk-rename plan was invalid: enter reopens the kept edit file, esc discards it.
    BulkReedit(BulkRetry),
    /// Quit although jobs are still running (they are cancelled).
    Quit {
        kind: crate::state::QuitKind,
    },
    /// Launch Claude although it would bill the API key. Accepted with `y` only; enter
    /// is ignored by `claude::handle_action`, which runs before this overlay's own handling.
    ClaudeProceed,
}

/// The generic confirmation overlay: a title, some lines, and optionally a text the user must
/// type before it can be accepted. With no `action` it is informational (enter or esc closes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmDialog {
    pub title: String,
    pub lines: Vec<String>,
    /// Lines that did not fit and are not listed.
    pub more: usize,
    /// Marks a destructive question (drawn with the error colour).
    pub danger: bool,
    /// The text that must be typed to accept.
    pub token: Option<String>,
    pub typed: PromptInput,
    /// The last accept attempt did not match the token.
    pub mismatch: bool,
    pub action: Option<ConfirmAction>,
    pub accept_label: String,
}

/// The one-line prompt under the list (rename, create, shell).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextPrompt {
    pub kind: PromptKind,
    /// Short label drawn before the input (`rename`, `new`, `$`).
    pub label: String,
    pub input: PromptInput,
    /// What the prompt acts on (the entry being renamed).
    pub target: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKind {
    CopyPath,
    OpenWith,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItem {
    pub label: String,
    pub hotkey: Option<char>,
}

/// A small list overlay (`Y`, `O`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Menu {
    pub kind: MenuKind,
    pub title: String,
    pub items: Vec<MenuItem>,
    pub selected: usize,
    /// The paths the chosen item acts on.
    pub paths: Vec<PathBuf>,
}

/// One line of command output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PagerLine {
    pub text: String,
    pub stderr: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PagerStatus {
    Running,
    Exited(Option<i32>),
    Cancelled,
    Failed(String),
}

/// Most lines the pager keeps; older ones are dropped and counted.
pub const PAGER_MAX_LINES: usize = 20_000;

/// The `!` output pager.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pager {
    pub id: u64,
    pub command: String,
    pub cwd: PathBuf,
    pub lines: VecDeque<PagerLine>,
    /// Lines dropped from the top because of [`PAGER_MAX_LINES`].
    pub dropped: u64,
    /// Index of the first visible line.
    pub scroll: usize,
    /// Keep the view on the newest line.
    pub follow: bool,
    pub status: PagerStatus,
}

impl Pager {
    pub fn is_running(&self) -> bool {
        self.status == PagerStatus::Running
    }
}

/// A foreground run waiting for its result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FgPending {
    pub id: u64,
    pub purpose: ForegroundPurpose,
}

/// Everything the file-operation feature keeps in [`AppState`](crate::state::AppState).
#[derive(Debug, Default)]
pub struct FileOps {
    pub register: Option<Register>,
    /// Newest last. Finished jobs stay (bounded) so the Jobs overlay can show results.
    pub jobs: Vec<JobRow>,
    pub jobs_selected: usize,
    pub conflicts: VecDeque<ConflictDialog>,
    pub confirm: Option<ConfirmDialog>,
    pub prompt: Option<TextPrompt>,
    pub menu: Option<Menu>,
    pub pager: Option<Pager>,
    pub(crate) next_shell: u64,
    pub(crate) next_fg: u64,
    pub(crate) fg: Option<FgPending>,
    pub(crate) arm_gen: crate::generation::GenCounter,
}

//! Engine data types: job specs, events, conflict resolution and reports.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::names::{validate_name, CreateTarget, NameError};

/// Identifier of a job, unique within one [`super::JobManager`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u64);

/// Caller-supplied generation id, echoed in every event of the job. Lets the app drop events
/// that belong to a stale context (e.g. a navigation that was superseded).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Generation(pub u64);

/// What a job does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// Copy files and directory trees.
    Copy,
    /// Move (rename on the same volume, copy + delete across volumes).
    Move,
    /// Send to the Recycle Bin.
    Trash,
    /// Delete for good.
    DeletePermanent,
    /// Create files / directories.
    Create,
    /// Rename one or more items (explicit source and destination paths).
    Rename,
    /// Execute a bulk-rename plan.
    BulkRename,
    /// Restore items from the Recycle Bin (the inverse of [`JobKind::Trash`], used by undo).
    Restore,
    /// Extract ZIP / RAR archives.
    Extract,
}

impl JobKind {
    /// Short human label for status lines ("Copying", ...).
    pub fn verb(self) -> &'static str {
        match self {
            JobKind::Copy => "Copying",
            JobKind::Move => "Moving",
            JobKind::Trash => "Trashing",
            JobKind::DeletePermanent => "Deleting",
            JobKind::Create => "Creating",
            JobKind::Rename => "Renaming",
            JobKind::BulkRename => "Bulk renaming",
            JobKind::Restore => "Restoring",
            JobKind::Extract => "Extracting",
        }
    }
}

/// A source path and the exact destination path it should end up at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferItem {
    /// Existing source.
    pub src: PathBuf,
    /// Destination path (not the destination directory).
    pub dst: PathBuf,
}

impl TransferItem {
    /// Construct from any path-likes.
    pub fn new(src: impl Into<PathBuf>, dst: impl Into<PathBuf>) -> Self {
        Self {
            src: src.into(),
            dst: dst.into(),
        }
    }
}

/// An item that was sent to the Recycle Bin and can be restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashedItem {
    /// Where it lived before.
    pub original: PathBuf,
    /// The trash crate's identifier (Windows: shell parsing name).
    pub id: OsString,
    /// Deletion time, seconds since the Unix epoch.
    pub time_deleted: i64,
}

/// A fully specified job. Build with the helper constructors where possible.
#[derive(Debug, Clone)]
pub enum JobSpec {
    /// Copy each item.
    Copy {
        /// Items to copy.
        items: Vec<TransferItem>,
    },
    /// Move each item.
    Move {
        /// Items to move.
        items: Vec<TransferItem>,
    },
    /// Send paths to the Recycle Bin.
    Trash {
        /// Absolute paths.
        paths: Vec<PathBuf>,
    },
    /// Permanently delete paths (recursively, links are not followed).
    DeletePermanent {
        /// Absolute paths.
        paths: Vec<PathBuf>,
    },
    /// Create files and directories (parents are created as needed).
    Create {
        /// Targets, see [`super::parse_create_input`].
        entries: Vec<CreateTarget>,
    },
    /// Rename (move within a volume) with conflict handling.
    Rename {
        /// `src` to `dst`.
        pairs: Vec<TransferItem>,
    },
    /// Sequence of renames that must run in order (from a [`super::RenamePlan`] or an undo).
    /// Never overwrites; stops at the first failure.
    BulkRename {
        /// Ordered steps.
        steps: Vec<TransferItem>,
    },
    /// Restore from the Recycle Bin.
    Restore {
        /// Items to restore.
        items: Vec<TrashedItem>,
    },
    /// Extract archives into a directory (see [`super::extract`] for the layout rules).
    Extract {
        /// ZIP or RAR archives.
        archives: Vec<PathBuf>,
        /// Directory to extract into.
        dest_dir: PathBuf,
    },
}

impl JobSpec {
    /// The kind of this job.
    pub fn kind(&self) -> JobKind {
        match self {
            JobSpec::Copy { .. } => JobKind::Copy,
            JobSpec::Move { .. } => JobKind::Move,
            JobSpec::Trash { .. } => JobKind::Trash,
            JobSpec::DeletePermanent { .. } => JobKind::DeletePermanent,
            JobSpec::Create { .. } => JobKind::Create,
            JobSpec::Rename { .. } => JobKind::Rename,
            JobSpec::BulkRename { .. } => JobKind::BulkRename,
            JobSpec::Restore { .. } => JobKind::Restore,
            JobSpec::Extract { .. } => JobKind::Extract,
        }
    }

    /// Copy `sources` into the directory `dest_dir`, keeping their names. Sources without a
    /// file name (drive roots) are dropped.
    pub fn copy_into(sources: &[PathBuf], dest_dir: &Path) -> Self {
        JobSpec::Copy {
            items: into_dir(sources, dest_dir),
        }
    }

    /// Move `sources` into the directory `dest_dir`, keeping their names.
    pub fn move_into(sources: &[PathBuf], dest_dir: &Path) -> Self {
        JobSpec::Move {
            items: into_dir(sources, dest_dir),
        }
    }

    /// Rename `from` to `new_name` within its directory. Validates the name.
    pub fn rename(from: &Path, new_name: &str) -> Result<Self, NameError> {
        validate_name(new_name)?;
        let to = from.with_file_name(new_name);
        Ok(JobSpec::Rename {
            pairs: vec![TransferItem::new(from, to)],
        })
    }

    /// Short description for the Jobs overlay, e.g. "Copying 3 items".
    pub fn describe(&self) -> String {
        let n = match self {
            JobSpec::Copy { items } | JobSpec::Move { items } => items.len(),
            JobSpec::Rename { pairs } => pairs.len(),
            JobSpec::BulkRename { steps } => steps.len(),
            JobSpec::Trash { paths } | JobSpec::DeletePermanent { paths } => paths.len(),
            JobSpec::Create { entries } => entries.len(),
            JobSpec::Restore { items } => items.len(),
            JobSpec::Extract { archives, .. } => archives.len(),
        };
        let first = match self {
            JobSpec::Copy { items } | JobSpec::Move { items } => items.first().map(|i| &i.src),
            JobSpec::Rename { pairs } => pairs.first().map(|i| &i.src),
            JobSpec::BulkRename { steps } => steps.first().map(|i| &i.src),
            JobSpec::Trash { paths } | JobSpec::DeletePermanent { paths } => paths.first(),
            JobSpec::Create { entries } => entries.first().map(|e| &e.path),
            JobSpec::Restore { items } => items.first().map(|i| &i.original),
            JobSpec::Extract { archives, .. } => archives.first(),
        };
        let verb = self.kind().verb();
        match (n, first.and_then(|p| p.file_name())) {
            (1, Some(name)) => format!("{verb} {}", name.to_string_lossy()),
            _ => format!("{verb} {n} items"),
        }
    }
}

fn into_dir(sources: &[PathBuf], dest_dir: &Path) -> Vec<TransferItem> {
    sources
        .iter()
        .filter_map(|s| {
            s.file_name()
                .map(|n| TransferItem::new(s.clone(), dest_dir.join(n)))
        })
        .collect()
}

/// How conflicts (target exists) are handled without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictPolicy {
    /// Pause the job and emit [`JobEvent::ConflictPrompt`] (default).
    #[default]
    Ask,
    /// Always overwrite.
    Overwrite,
    /// Always skip.
    Skip,
    /// Always keep both (`name (1).ext`).
    KeepBoth,
    /// Skip and record an [`ItemError`] ("already exists").
    Fail,
}

/// Per-job options.
#[derive(Debug, Clone, Copy)]
pub struct JobOptions {
    /// Conflict handling.
    pub conflict: ConflictPolicy,
    /// Record the result in the undo log (default true). Undo jobs themselves use false.
    pub record_undo: bool,
    /// Resolve Recycle Bin ids for trashed items (two bin listings per Trash job, cost grows with
    /// the bin size). Needed only for undo, so it is ignored when `record_undo` is false.
    /// Default true.
    pub track_trash_ids: bool,
}

impl Default for JobOptions {
    fn default() -> Self {
        Self {
            conflict: ConflictPolicy::Ask,
            record_undo: true,
            track_trash_ids: true,
        }
    }
}

/// The caller's answer to a [`ConflictPrompt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictChoice {
    /// Replace the destination.
    Overwrite,
    /// Leave both untouched; skip this item.
    Skip,
    /// Use this new name (in the destination's directory). Invalid names are an item error.
    /// Not sticky: `apply_to_all` is ignored for this choice.
    Rename(String),
    /// Automatic `name (1).ext`.
    KeepBoth,
    /// Abort the whole job (it ends as Cancelled).
    Cancel,
}

/// Choice plus the "apply to all remaining conflicts" toggle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// What to do.
    pub choice: ConflictChoice,
    /// Reuse the choice for all later conflicts of this job.
    pub apply_to_all: bool,
}

impl Resolution {
    /// A one-off resolution.
    pub fn once(choice: ConflictChoice) -> Self {
        Self {
            choice,
            apply_to_all: false,
        }
    }

    /// A resolution applied to all remaining conflicts.
    pub fn all(choice: ConflictChoice) -> Self {
        Self {
            choice,
            apply_to_all: true,
        }
    }
}

/// Metadata shown in the conflict dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    /// Directory?
    pub is_dir: bool,
    /// Symlink or junction?
    pub is_link: bool,
    /// Size (0 for directories).
    pub len: u64,
    /// Last modification.
    pub modified: Option<SystemTime>,
}

impl FileMeta {
    pub(crate) fn from_metadata(m: &std::fs::Metadata) -> Self {
        Self {
            is_dir: m.is_dir(),
            is_link: m.file_type().is_symlink(),
            len: if m.is_dir() { 0 } else { m.len() },
            modified: m.modified().ok(),
        }
    }
}

/// The job is paused until [`super::JobManager::resolve`] is called for `job`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictPrompt {
    /// Job that is waiting.
    pub job: JobId,
    /// Generation of the job.
    pub generation: Generation,
    /// Source being transferred.
    pub src: PathBuf,
    /// Existing destination.
    pub dst: PathBuf,
    /// Source metadata.
    pub src_meta: FileMeta,
    /// Destination metadata.
    pub dst_meta: FileMeta,
}

/// Progress snapshot. Totals can grow while a job runs (moves that fall back to copying).
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// Job.
    pub job: JobId,
    /// Generation.
    pub generation: Generation,
    /// Bytes processed.
    pub bytes_done: u64,
    /// Bytes expected (0 while unknown, e.g. renames).
    pub bytes_total: u64,
    /// Files (and links) processed.
    pub files_done: u64,
    /// Files expected.
    pub files_total: u64,
    /// File currently being processed.
    pub current: Option<PathBuf>,
    /// Smoothed throughput in bytes per second (3 s window).
    pub throughput_bps: u64,
    /// Estimated time remaining, when computable.
    pub eta: Option<Duration>,
}

/// An error attached to one item; the job continues with the next item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemError {
    /// Path the error is about.
    pub path: PathBuf,
    /// Human readable message.
    pub message: String,
}

/// What a job actually changed. Drives the undo log and lets the UI refresh/select results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A new copy was created (not recorded for overwritten destinations).
    Copied {
        /// Source.
        src: PathBuf,
        /// New item.
        dst: PathBuf,
    },
    /// An item was moved.
    Moved {
        /// Old location.
        src: PathBuf,
        /// New location.
        dst: PathBuf,
    },
    /// An item was renamed.
    Renamed {
        /// Old path.
        from: PathBuf,
        /// New path.
        to: PathBuf,
    },
    /// Something was created. `path` is the topmost newly created entry.
    Created {
        /// Topmost new entry.
        path: PathBuf,
        /// Whether that entry is a directory.
        is_dir: bool,
    },
    /// Sent to the Recycle Bin. `item` is `None` when the trash id could not be determined
    /// (such an item cannot be undone).
    Trashed {
        /// Original path.
        path: PathBuf,
        /// Restore handle.
        item: Option<TrashedItem>,
    },
    /// Permanently removed.
    Deleted {
        /// Path (top-level item).
        path: PathBuf,
    },
    /// Restored from the Recycle Bin.
    Restored {
        /// Path it was restored to.
        path: PathBuf,
    },
}

/// Final report of a job, attached to Done / Failed / Cancelled.
#[derive(Debug, Clone, PartialEq)]
pub struct JobReport {
    /// Job.
    pub job: JobId,
    /// Generation.
    pub generation: Generation,
    /// Kind.
    pub kind: JobKind,
    /// Top-level items that completed.
    pub items_done: usize,
    /// Items skipped because of a conflict resolution.
    pub items_skipped: usize,
    /// Bytes copied/moved.
    pub bytes_done: u64,
    /// Per-item errors.
    pub errors: Vec<ItemError>,
    /// What changed, in execution order.
    pub outcomes: Vec<Outcome>,
    /// Move sources that are gone because their content now lives at the destination,
    /// including sources whose destination was replaced or merged (those record no outcome).
    pub consumed_sources: Vec<PathBuf>,
    /// Wall time.
    pub elapsed: Duration,
}

/// Everything the engine tells the caller. Map 1:1 to `Msg` variants.
#[derive(Debug, Clone, PartialEq)]
pub enum JobEvent {
    /// Accepted but waiting for a free slot.
    Queued {
        /// Job.
        job: JobId,
        /// Generation.
        generation: Generation,
        /// Kind.
        kind: JobKind,
        /// Short description.
        description: String,
    },
    /// Execution began.
    Started {
        /// Job.
        job: JobId,
        /// Generation.
        generation: Generation,
        /// Kind.
        kind: JobKind,
        /// Short description.
        description: String,
    },
    /// Pre-scan running (size totals so far). Cancellable like everything else.
    Scanning {
        /// Job.
        job: JobId,
        /// Generation.
        generation: Generation,
        /// Files seen so far.
        files: u64,
        /// Bytes seen so far.
        bytes: u64,
    },
    /// Progress update (throttled to roughly 10 per second).
    Progress(Progress),
    /// A conflict needs an answer.
    ConflictPrompt(ConflictPrompt),
    /// Finished without any item error.
    Done(JobReport),
    /// Finished, but at least one item failed (`report.errors`); other items may have succeeded.
    Failed(JobReport),
    /// Cancelled by the caller; `report` holds what completed before.
    Cancelled(JobReport),
}

impl JobEvent {
    /// The job this event belongs to.
    pub fn job(&self) -> JobId {
        match self {
            JobEvent::Queued { job, .. }
            | JobEvent::Started { job, .. }
            | JobEvent::Scanning { job, .. } => *job,
            JobEvent::Progress(p) => p.job,
            JobEvent::ConflictPrompt(c) => c.job,
            JobEvent::Done(r) | JobEvent::Failed(r) | JobEvent::Cancelled(r) => r.job,
        }
    }

    /// The generation of the job.
    pub fn generation(&self) -> Generation {
        match self {
            JobEvent::Queued { generation, .. }
            | JobEvent::Started { generation, .. }
            | JobEvent::Scanning { generation, .. } => *generation,
            JobEvent::Progress(p) => p.generation,
            JobEvent::ConflictPrompt(c) => c.generation,
            JobEvent::Done(r) | JobEvent::Failed(r) | JobEvent::Cancelled(r) => r.generation,
        }
    }

    /// Whether this is the last event of the job.
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            JobEvent::Done(_) | JobEvent::Failed(_) | JobEvent::Cancelled(_)
        )
    }
}

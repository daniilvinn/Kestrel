//! File-operation engine.
//!
//! Self-contained: it does not depend on `ks-core`. The app wires it in by
//!
//! 1. creating one [`JobManager`] with an [`EventSink`] that turns each [`JobEvent`] into a `Msg`;
//! 2. building a [`JobSpec`] from the selection/yank register (`JobSpec::copy_into`,
//!    `move_into`, `rename`, `Trash`, `DeletePermanent`, `Create` via [`parse_create_input`],
//!    `Extract` for ZIP/RAR archives, bulk rename via [`BulkRename`]) and calling
//!    [`JobManager::submit`] with the current generation id;
//! 3. answering [`JobEvent::ConflictPrompt`] with [`JobManager::resolve`];
//! 4. calling [`JobManager::cancel`] / [`JobManager::undo`].
//!
//! # Threading
//! Each job runs on its own `std::thread`; manager methods never block on IO and never touch
//! the caller's thread. Cancellation uses [`tokio_util::sync::CancellationToken`] internally
//! (checked between 4 MiB copy chunks and between directory entries). The engine logs through
//! `tracing` only.
//!
//! # Link policy
//! Symlinks and junctions are never followed while recursing (copy, scan, delete). Copy
//! recreates the link as a symlink; if that is not permitted the link is skipped and reported
//! as an [`ItemError`]. Move on the same volume renames the link itself. Delete removes the
//! link only.
//!
//! # Conflict policy
//! Directory onto directory merges silently and asks per file. See [`ConflictPolicy`],
//! [`ConflictChoice`] and [`Resolution`].
//!
//! # Long paths and Unicode
//! `std::fs` switches to `\\?\` paths for absolute paths over `MAX_PATH`; the engine absolutizes
//! every path on submit. Use [`paths::display`] (dunce) for showing paths.

mod bulk;
pub mod extract;
mod manager;
mod names;
pub mod paths;
mod remove;
mod runner;
mod sink;
mod transfer;
mod types;
mod undo;

pub use bulk::{BulkError, BulkRename, RenameChange, RenamePlan};
pub use extract::ArchiveKind;
pub use manager::{JobInfo, JobManager, JobState};
pub use names::{
    confirmation_matches, confirmation_token, keep_both_name, keep_both_path, parse_create_input,
    validate_name, CreateTarget, NameError,
};
pub use paths::Fingerprint;
pub use remove::TRASH_RESTORE_SUPPORTED;
pub use sink::EventSink;
pub use types::{
    ConflictChoice, ConflictPolicy, ConflictPrompt, FileMeta, Generation, ItemError, JobEvent,
    JobId, JobKind, JobOptions, JobReport, JobSpec, Outcome, Progress, Resolution, TransferItem,
    TrashedItem,
};
pub use undo::{PathPair, UndoEntry, UndoError, UndoLog};

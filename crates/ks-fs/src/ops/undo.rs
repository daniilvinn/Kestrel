//! Session-scoped undo log.
//!
//! The [`super::JobManager`] records an entry when a job finishes (Done, Failed or Cancelled,
//! if anything changed). [`UndoLog::take_inverse`] pops the newest entry, checks that the world
//! still looks the way the job left it and returns the inverse [`JobSpec`].
//!
//! | recorded | inverse |
//! |---|---|
//! | rename / bulk rename | rename back (one entry for the whole bulk rename) |
//! | move | move back (parent directories are recreated) |
//! | create, extract | trash the topmost created entry |
//! | trash | restore from the Recycle Bin (Windows / Linux only) |
//! | copy | trash the copies |
//!
//! Copies that overwrote an existing file, and permanent deletes, are not undoable. Merged
//! directory copies record the individual new entries, not the pre-existing directory.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::paths::{display, path_key, Fingerprint};
use super::remove::{trash_list, TRASH_RESTORE_SUPPORTED};
use super::types::{JobKind, JobReport, JobSpec, Outcome, TransferItem, TrashedItem};

const MAX_ENTRIES: usize = 200;

/// Why an undo could not be performed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UndoError {
    /// Nothing to undo.
    #[error("nothing to undo")]
    Empty,
    /// The filesystem no longer matches what the operation left behind. The entry is dropped.
    #[error("cannot undo: {0}")]
    Changed(String),
    /// Not supported on this platform.
    #[error("cannot undo: {0}")]
    Unsupported(String),
}

/// A recorded path change. `to` is where the item ended up; `fingerprint` is its state then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPair {
    /// Where it was before.
    pub from: PathBuf,
    /// Where it is now.
    pub to: PathBuf,
    /// State of `to` right after the operation.
    pub fingerprint: Option<Fingerprint>,
}

/// One undoable operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoEntry {
    /// Renames in execution order (a bulk rename is one entry).
    Rename {
        /// Steps in execution order.
        pairs: Vec<PathPair>,
        /// Came from a bulk rename.
        bulk: bool,
    },
    /// Moves.
    Move {
        /// Moves.
        pairs: Vec<PathPair>,
    },
    /// Copies (`from` = source, `to` = the new copy).
    Copy {
        /// Copies.
        pairs: Vec<PathPair>,
    },
    /// Created entries (topmost new path, state right after creation).
    Create {
        /// Entries.
        items: Vec<(PathBuf, Option<Fingerprint>)>,
    },
    /// Items sent to the Recycle Bin.
    Trash {
        /// Items.
        items: Vec<TrashedItem>,
    },
}

impl UndoEntry {
    /// Build an entry from a finished job. `None` if nothing undoable happened.
    pub fn from_report(report: &JobReport) -> Option<Self> {
        let pair = |from: &Path, to: &Path| PathPair {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
            fingerprint: Fingerprint::of(to),
        };
        let mut renames = Vec::new();
        let mut moves = Vec::new();
        let mut copies = Vec::new();
        let mut created = Vec::new();
        let mut trashed = Vec::new();
        for o in &report.outcomes {
            match o {
                Outcome::Renamed { from, to } => renames.push(pair(from, to)),
                Outcome::Moved { src, dst } => moves.push(pair(src, dst)),
                Outcome::Copied { src, dst } => copies.push(pair(src, dst)),
                Outcome::Created { path, .. } => {
                    created.push((path.clone(), Fingerprint::of(path)))
                }
                Outcome::Trashed { item: Some(i), .. } => trashed.push(i.clone()),
                Outcome::Trashed { .. } | Outcome::Deleted { .. } | Outcome::Restored { .. } => {}
            }
        }
        match report.kind {
            JobKind::Rename | JobKind::BulkRename if !renames.is_empty() => {
                Some(UndoEntry::Rename {
                    pairs: renames,
                    bulk: report.kind == JobKind::BulkRename,
                })
            }
            JobKind::Move if !moves.is_empty() => Some(UndoEntry::Move { pairs: moves }),
            JobKind::Copy if !copies.is_empty() => Some(UndoEntry::Copy { pairs: copies }),
            JobKind::Create | JobKind::Extract if !created.is_empty() => {
                Some(UndoEntry::Create { items: created })
            }
            JobKind::Trash if !trashed.is_empty() => Some(UndoEntry::Trash { items: trashed }),
            _ => None,
        }
    }

    /// Text for the status line, e.g. "rename a.txt to b.txt".
    pub fn description(&self) -> String {
        let name = |p: &Path| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| display(p))
        };
        match self {
            UndoEntry::Rename { pairs, bulk: false } if pairs.len() == 1 => {
                format!("rename {} to {}", name(&pairs[0].from), name(&pairs[0].to))
            }
            UndoEntry::Rename { pairs, .. } => format!("rename of {} items", pairs.len()),
            UndoEntry::Move { pairs } => format!("move of {} item(s)", pairs.len()),
            UndoEntry::Copy { pairs } => format!("copy of {} item(s)", pairs.len()),
            UndoEntry::Create { items } => format!("create of {} item(s)", items.len()),
            UndoEntry::Trash { items } => format!("delete of {} item(s)", items.len()),
        }
    }

    /// Verify the filesystem state and build the inverse job.
    pub fn inverse(&self) -> Result<JobSpec, UndoError> {
        match self {
            UndoEntry::Rename { pairs, bulk } => {
                check_renames(pairs)?;
                let back: Vec<TransferItem> = pairs
                    .iter()
                    .rev()
                    .map(|p| TransferItem::new(&p.to, &p.from))
                    .collect();
                Ok(if *bulk {
                    JobSpec::BulkRename { steps: back }
                } else {
                    JobSpec::Rename { pairs: back }
                })
            }
            UndoEntry::Move { pairs } => {
                check_renames(pairs)?;
                Ok(JobSpec::Move {
                    items: pairs
                        .iter()
                        .rev()
                        .map(|p| TransferItem::new(&p.to, &p.from))
                        .collect(),
                })
            }
            UndoEntry::Copy { pairs } => {
                for p in pairs {
                    check_unchanged(&p.to, p.fingerprint)?;
                }
                Ok(JobSpec::Trash {
                    paths: pairs.iter().map(|p| p.to.clone()).collect(),
                })
            }
            UndoEntry::Create { items } => {
                for (p, fp) in items {
                    check_unchanged(p, *fp)?;
                }
                Ok(JobSpec::Trash {
                    paths: items.iter().map(|(p, _)| p.clone()).collect(),
                })
            }
            UndoEntry::Trash { items } => {
                if !TRASH_RESTORE_SUPPORTED {
                    return Err(UndoError::Unsupported(
                        "restoring from the trash is not supported on this platform".into(),
                    ));
                }
                let listing = trash_list()
                    .map_err(|e| UndoError::Changed(format!("cannot read the trash: {e}")))?;
                let ids: HashSet<_> = listing.iter().map(|i| &i.id).collect();
                for item in items {
                    if !ids.contains(&item.id) {
                        return Err(UndoError::Changed(format!(
                            "{} is no longer in the trash",
                            display(&item.original)
                        )));
                    }
                    if std::fs::symlink_metadata(&item.original).is_ok() {
                        return Err(UndoError::Changed(format!(
                            "{} exists again",
                            display(&item.original)
                        )));
                    }
                }
                Ok(JobSpec::Restore {
                    items: items.clone(),
                })
            }
        }
    }
}

fn check_unchanged(path: &Path, fp: Option<Fingerprint>) -> Result<(), UndoError> {
    match Fingerprint::of(path) {
        None => Err(UndoError::Changed(format!("{} is missing", display(path)))),
        Some(now) if fp.is_some_and(|f| f != now) => Err(UndoError::Changed(format!(
            "{} was modified",
            display(path)
        ))),
        Some(_) => Ok(()),
    }
}

/// Rename-like entries: validate the end state. Intermediate paths of bulk renames (temporary
/// names) are tolerated because a later step consumes them.
fn check_renames(pairs: &[PathPair]) -> Result<(), UndoError> {
    let targets: HashSet<String> = pairs.iter().map(|p| path_key(&p.to)).collect();
    for (i, p) in pairs.iter().enumerate() {
        let consumed_later = pairs[i + 1..]
            .iter()
            .any(|q| path_key(&q.from) == path_key(&p.to));
        if !consumed_later {
            check_unchanged(&p.to, p.fingerprint)?;
        }
        let case_only = path_key(&p.from) == path_key(&p.to);
        if !case_only
            && !targets.contains(&path_key(&p.from))
            && std::fs::symlink_metadata(&p.from).is_ok()
        {
            return Err(UndoError::Changed(format!(
                "{} exists again",
                display(&p.from)
            )));
        }
    }
    Ok(())
}

/// Bounded stack of undoable operations.
#[derive(Debug, Default)]
pub struct UndoLog {
    entries: Vec<UndoEntry>,
}

impl UndoLog {
    /// Empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an entry (oldest entries are dropped beyond 200).
    pub fn push(&mut self, entry: UndoEntry) {
        self.entries.push(entry);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// No entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Newest entry, if any.
    pub fn last(&self) -> Option<&UndoEntry> {
        self.entries.last()
    }

    /// Forget everything.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Pop the newest entry and return its inverse job. An entry whose state changed is
    /// dropped and reported as [`UndoError::Changed`].
    pub fn take_inverse(&mut self) -> Result<JobSpec, UndoError> {
        let entry = self.entries.pop().ok_or(UndoError::Empty)?;
        entry.inverse()
    }
}

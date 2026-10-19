//! Per-job execution state shared by all job implementations: progress, cancellation, conflicts.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::sink::EventSink;
use super::types::{
    ConflictChoice, ConflictPolicy, ConflictPrompt, FileMeta, Generation, ItemError, JobEvent,
    JobId, JobKind, JobReport, Outcome, Progress, Resolution,
};

/// Copy chunk size. Cancellation is checked between chunks.
pub(crate) const CHUNK: usize = 4 * 1024 * 1024;

const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const THROUGHPUT_WINDOW: Duration = Duration::from_secs(3);

/// The job was cancelled; unwind.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cancelled;

pub(crate) type Flow<T = ()> = Result<T, Cancelled>;

#[derive(Clone, Copy)]
enum Sticky {
    Overwrite,
    Skip,
    KeepBoth,
}

pub(crate) struct Runner {
    pub id: JobId,
    pub generation: Generation,
    pub kind: JobKind,
    token: CancellationToken,
    sink: Arc<dyn EventSink<JobEvent>>,
    resolutions: Receiver<Resolution>,
    policy: ConflictPolicy,
    sticky: Option<Sticky>,
    started: Instant,
    last_emit: Instant,
    window: VecDeque<(Instant, u64)>,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
    current: Option<PathBuf>,
    pub items_done: usize,
    pub items_skipped: usize,
    pub errors: Vec<ItemError>,
    pub outcomes: Vec<Outcome>,
    pub consumed_sources: Vec<PathBuf>,
    pub buf: Vec<u8>,
    tmp_counter: u64,
    /// Set by a cross-volume move once its copy phase finished (only the source delete remains).
    pub copy_complete: bool,
}

impl Runner {
    pub fn new(
        id: JobId,
        generation: Generation,
        kind: JobKind,
        token: CancellationToken,
        sink: Arc<dyn EventSink<JobEvent>>,
        resolutions: Receiver<Resolution>,
        policy: ConflictPolicy,
    ) -> Self {
        let now = Instant::now();
        Self {
            id,
            generation,
            kind,
            token,
            sink,
            resolutions,
            policy,
            sticky: None,
            started: now,
            last_emit: now.checked_sub(PROGRESS_INTERVAL).unwrap_or(now),
            window: VecDeque::new(),
            bytes_done: 0,
            bytes_total: 0,
            files_done: 0,
            files_total: 0,
            current: None,
            items_done: 0,
            items_skipped: 0,
            errors: Vec::new(),
            outcomes: Vec::new(),
            consumed_sources: Vec::new(),
            buf: Vec::new(),
            tmp_counter: 0,
            copy_complete: false,
        }
    }

    pub fn emit(&self, event: JobEvent) {
        self.sink.send(event);
    }

    pub fn check(&self) -> Flow {
        if self.token.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    pub fn error(&mut self, path: &Path, err: impl std::fmt::Display) {
        tracing::warn!(path = %path.display(), error = %err, "item error");
        self.errors.push(ItemError {
            path: path.to_path_buf(),
            message: err.to_string(),
        });
    }

    pub fn set_current(&mut self, path: &Path) {
        self.current = Some(path.to_path_buf());
    }

    pub fn add_bytes(&mut self, n: u64) {
        self.bytes_done += n;
        self.tick(false);
    }

    pub fn file_done(&mut self) {
        self.files_done += 1;
        self.tick(false);
    }

    /// Emit a progress event if due (or `force`).
    pub fn tick(&mut self, force: bool) {
        let now = Instant::now();
        if !force && now.duration_since(self.last_emit) < PROGRESS_INTERVAL {
            return;
        }
        if !force {
            self.last_emit = now;
        }
        self.window.push_back((now, self.bytes_done));
        while let Some(&(t, _)) = self.window.front() {
            if now.duration_since(t) > THROUGHPUT_WINDOW && self.window.len() > 2 {
                self.window.pop_front();
            } else {
                break;
            }
        }
        let throughput = match (self.window.front(), self.window.back()) {
            (Some(&(t0, b0)), Some(&(t1, b1))) if t1 > t0 => {
                ((b1 - b0) as f64 / t1.duration_since(t0).as_secs_f64()) as u64
            }
            _ => 0,
        };
        let remaining = self.bytes_total.saturating_sub(self.bytes_done);
        let eta = (throughput > 0 && remaining > 0)
            .then(|| Duration::from_secs_f64(remaining as f64 / throughput as f64));
        self.emit(JobEvent::Progress(Progress {
            job: self.id,
            generation: self.generation,
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
            files_done: self.files_done,
            files_total: self.files_total,
            current: self.current.clone(),
            throughput_bps: throughput,
            eta,
        }));
    }

    pub fn next_temp_name(&mut self) -> String {
        self.tmp_counter += 1;
        format!(".ks-{}-{}.part", self.id.0, self.tmp_counter)
    }

    /// Decide a conflict. Blocks (polling the cancel token) while waiting for the caller.
    pub fn resolve_conflict(
        &mut self,
        src: &Path,
        dst: &Path,
        src_meta: FileMeta,
        dst_meta: FileMeta,
    ) -> Flow<ConflictChoice> {
        if let Some(s) = self.sticky {
            return Ok(match s {
                Sticky::Overwrite => ConflictChoice::Overwrite,
                Sticky::Skip => ConflictChoice::Skip,
                Sticky::KeepBoth => ConflictChoice::KeepBoth,
            });
        }
        match self.policy {
            ConflictPolicy::Overwrite => return Ok(ConflictChoice::Overwrite),
            ConflictPolicy::Skip => return Ok(ConflictChoice::Skip),
            ConflictPolicy::KeepBoth => return Ok(ConflictChoice::KeepBoth),
            ConflictPolicy::Fail => {
                self.error(dst, "already exists");
                return Ok(ConflictChoice::Skip);
            }
            ConflictPolicy::Ask => {}
        }
        self.tick(true);
        while self.resolutions.try_recv().is_ok() {} // discard anything stale
        self.emit(JobEvent::ConflictPrompt(ConflictPrompt {
            job: self.id,
            generation: self.generation,
            src: src.to_path_buf(),
            dst: dst.to_path_buf(),
            src_meta,
            dst_meta,
        }));
        loop {
            self.check()?;
            match self.resolutions.recv_timeout(Duration::from_millis(40)) {
                Ok(Resolution {
                    choice,
                    apply_to_all,
                }) => {
                    if apply_to_all {
                        self.sticky = match choice {
                            ConflictChoice::Overwrite => Some(Sticky::Overwrite),
                            ConflictChoice::Skip => Some(Sticky::Skip),
                            ConflictChoice::KeepBoth => Some(Sticky::KeepBoth),
                            _ => None,
                        };
                    }
                    if choice == ConflictChoice::Cancel {
                        self.token.cancel();
                        return Err(Cancelled);
                    }
                    return Ok(choice);
                }
                Err(RecvTimeoutError::Timeout) => {}
                // The manager is gone; nobody can answer.
                Err(RecvTimeoutError::Disconnected) => return Err(Cancelled),
            }
        }
    }

    pub fn into_report(self) -> JobReport {
        JobReport {
            job: self.id,
            generation: self.generation,
            kind: self.kind,
            items_done: self.items_done,
            items_skipped: self.items_skipped,
            bytes_done: self.bytes_done,
            errors: self.errors,
            outcomes: self.outcomes,
            consumed_sources: self.consumed_sources,
            elapsed: self.started.elapsed(),
        }
    }
}

//! The job manager: queue, threads, cancellation, conflict answers and the undo log.

use std::collections::{HashMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::paths::absolutize;
use super::runner::{Flow, Runner};
use super::sink::EventSink;
use super::types::{
    Generation, JobEvent, JobId, JobKind, JobOptions, JobReport, JobSpec, Progress, Resolution,
    TransferItem,
};
use super::undo::{UndoEntry, UndoError, UndoLog};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Lifecycle of a job as seen by [`JobManager::jobs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Waiting for a free slot.
    Queued,
    /// Executing.
    Running,
    /// Paused on a conflict prompt.
    AwaitingConflict,
    /// Finished without errors.
    Done,
    /// Finished with item errors.
    Failed,
    /// Cancelled.
    Cancelled,
}

/// Snapshot of one job for the Jobs overlay.
#[derive(Debug, Clone)]
pub struct JobInfo {
    /// Job id.
    pub id: JobId,
    /// Generation given at submit.
    pub generation: Generation,
    /// Kind.
    pub kind: JobKind,
    /// Short description.
    pub description: String,
    /// State.
    pub state: JobState,
    /// Latest progress, if any was reported.
    pub progress: Option<Progress>,
}

struct Entry {
    id: JobId,
    generation: Generation,
    kind: JobKind,
    description: String,
    options: JobOptions,
    token: CancellationToken,
    resolve_tx: Sender<Resolution>,
    start: Mutex<Option<(JobSpec, std::sync::mpsc::Receiver<Resolution>)>>,
    info: Mutex<(JobState, Option<Progress>)>,
}

struct State {
    next_id: u64,
    jobs: HashMap<JobId, Arc<Entry>>,
    order: Vec<JobId>,
    pending: VecDeque<JobId>,
    running: usize,
}

struct Inner {
    sink: Arc<dyn EventSink<JobEvent>>,
    max_concurrent: usize,
    state: Mutex<State>,
    undo: Mutex<UndoLog>,
    idle: Condvar,
}

/// Runs file operations in the background.
///
/// **Threading:** every job runs on its own dedicated `std::thread` (named `ks-job-N`), at most
/// `max_concurrent` at a time (default 2), the rest wait in a FIFO queue. All methods on the
/// manager return immediately and never block on IO. Events are delivered through the
/// [`EventSink`] from the job threads. Dropping the manager cancels all jobs.
///
/// Typical wiring: create one manager with a sink that wraps the app's message channel
/// (`JobEvent::Progress` to `Msg::JobProgress`, `ConflictPrompt` to an overlay, the three final
/// events to a refresh plus a toast), `submit` jobs from effects, call `resolve` from the
/// conflict overlay, and `cancel` from the Jobs overlay.
pub struct JobManager {
    inner: Arc<Inner>,
}

impl JobManager {
    /// New manager delivering events to `sink`, running up to 2 jobs concurrently.
    pub fn new(sink: impl EventSink<JobEvent>) -> Self {
        Self::with_concurrency(sink, 2)
    }

    /// New manager with an explicit concurrency limit (minimum 1).
    pub fn with_concurrency(sink: impl EventSink<JobEvent>, max_concurrent: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                sink: Arc::new(sink),
                max_concurrent: max_concurrent.max(1),
                state: Mutex::new(State {
                    next_id: 1,
                    jobs: HashMap::new(),
                    order: Vec::new(),
                    pending: VecDeque::new(),
                    running: 0,
                }),
                undo: Mutex::new(UndoLog::new()),
                idle: Condvar::new(),
            }),
        }
    }

    /// Submit with default options (ask on conflicts, record undo).
    pub fn submit(&self, spec: JobSpec, generation: Generation) -> JobId {
        self.submit_with(spec, generation, JobOptions::default())
    }

    /// Submit a job. Relative paths in the spec are made absolute against the current
    /// directory. Returns immediately.
    pub fn submit_with(&self, spec: JobSpec, generation: Generation, options: JobOptions) -> JobId {
        let spec = absolutize_spec(spec);
        let (tx, rx) = channel();
        let description = spec.describe();
        let kind = spec.kind();
        let id;
        {
            let mut st = lock(&self.inner.state);
            id = JobId(st.next_id);
            st.next_id += 1;
            let entry = Arc::new(Entry {
                id,
                generation,
                kind,
                description: description.clone(),
                options,
                token: CancellationToken::new(),
                resolve_tx: tx,
                start: Mutex::new(Some((spec, rx))),
                info: Mutex::new((JobState::Queued, None)),
            });
            st.jobs.insert(id, entry);
            st.order.push(id);
        }
        // `Queued` is always the first event of a job: the job only becomes eligible to run
        // (enters `pending`) after it has been sent.
        self.inner.sink.send(JobEvent::Queued {
            job: id,
            generation,
            kind,
            description,
        });
        lock(&self.inner.state).pending.push_back(id);
        dispatch(&self.inner);
        id
    }

    /// Answer the pending [`JobEvent::ConflictPrompt`] of `job`. Returns false (and discards
    /// the answer) unless the job is currently awaiting a conflict answer, so stale or
    /// duplicate answers can never leak into a later prompt.
    pub fn resolve(&self, job: JobId, resolution: Resolution) -> bool {
        let Some(entry) = lock(&self.inner.state).jobs.get(&job).cloned() else {
            return false;
        };
        let mut info = lock(&entry.info);
        if info.0 != JobState::AwaitingConflict {
            return false;
        }
        info.0 = JobState::Running;
        entry.resolve_tx.send(resolution).is_ok()
    }

    /// Cancel one job. A queued job is cancelled immediately; a running one stops at the next
    /// chunk boundary. Returns false if the job is unknown or already finished.
    ///
    /// A `true` return means the request was registered, not that a `Cancelled` event will
    /// follow: a job that is about to finish may still report `Done`/`Failed`. Always wait
    /// for the job's final event.
    pub fn cancel(&self, job: JobId) -> bool {
        let entry = {
            let mut st = lock(&self.inner.state);
            let Some(entry) = st.jobs.get(&job).cloned() else {
                return false;
            };
            if let Some(pos) = st.pending.iter().position(|j| *j == job) {
                st.pending.remove(pos);
                drop(st);
                lock(&entry.info).0 = JobState::Cancelled;
                entry.token.cancel();
                let report = empty_report(&entry);
                self.inner.sink.send(JobEvent::Cancelled(report));
                self.inner.idle.notify_all();
                return true;
            }
            entry
        };
        let finished = matches!(
            lock(&entry.info).0,
            JobState::Done | JobState::Failed | JobState::Cancelled
        );
        if finished {
            return false;
        }
        entry.token.cancel();
        true
    }

    /// Cancel every queued and running job.
    pub fn cancel_all(&self) {
        let ids: Vec<JobId> = lock(&self.inner.state).order.clone();
        for id in ids {
            self.cancel(id);
        }
    }

    /// Snapshot of all known jobs in submission order (finished ones included until
    /// [`Self::clear_finished`]).
    pub fn jobs(&self) -> Vec<JobInfo> {
        let st = lock(&self.inner.state);
        st.order
            .iter()
            .filter_map(|id| st.jobs.get(id))
            .map(|e| {
                let info = lock(&e.info);
                JobInfo {
                    id: e.id,
                    generation: e.generation,
                    kind: e.kind,
                    description: e.description.clone(),
                    state: info.0,
                    progress: info.1.clone(),
                }
            })
            .collect()
    }

    /// Number of queued or running jobs.
    pub fn active_count(&self) -> usize {
        self.jobs()
            .iter()
            .filter(|j| {
                matches!(
                    j.state,
                    JobState::Queued | JobState::Running | JobState::AwaitingConflict
                )
            })
            .count()
    }

    /// Forget finished jobs (they stop appearing in [`Self::jobs`]).
    pub fn clear_finished(&self) {
        let mut st = lock(&self.inner.state);
        let finished: Vec<JobId> = st
            .jobs
            .iter()
            .filter(|(_, e)| {
                matches!(
                    lock(&e.info).0,
                    JobState::Done | JobState::Failed | JobState::Cancelled
                )
            })
            .map(|(id, _)| *id)
            .collect();
        for id in finished {
            st.jobs.remove(&id);
            st.order.retain(|j| *j != id);
        }
    }

    /// Block until no job is queued or running, or `timeout` elapses. Intended for tests and
    /// shutdown; never call it from the UI thread. Returns true if idle.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut st = lock(&self.inner.state);
        while st.running > 0 || !st.pending.is_empty() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            st = self
                .inner
                .idle
                .wait_timeout(st, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }

    /// Verify and submit the inverse of the newest undoable operation.
    ///
    /// The returned job is not itself recorded. On [`UndoError::Changed`] the entry has been
    /// dropped.
    pub fn undo(&self, generation: Generation) -> Result<JobId, UndoError> {
        let spec = lock(&self.inner.undo).take_inverse()?;
        Ok(self.submit_with(
            spec,
            generation,
            JobOptions {
                record_undo: false,
                ..JobOptions::default()
            },
        ))
    }

    /// Description of what [`Self::undo`] would undo, e.g. "rename a.txt to b.txt".
    pub fn undo_description(&self) -> Option<String> {
        lock(&self.inner.undo).last().map(UndoEntry::description)
    }

    /// Number of undo entries.
    pub fn undo_depth(&self) -> usize {
        lock(&self.inner.undo).len()
    }

    /// Record an undo entry for an operation that was not run through the manager.
    pub fn record_undo(&self, entry: UndoEntry) {
        lock(&self.inner.undo).push(entry);
    }
}

/// Dropping cancels every job but does **not** join the job threads: they finish asynchronously
/// (cleaning up partial files and emitting `Cancelled`) and keep the sink alive until then. Call
/// [`JobManager::wait_idle`] first when the caller must observe completion (tests, shutdown).
impl Drop for JobManager {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

fn absolutize_spec(spec: JobSpec) -> JobSpec {
    let items = |v: Vec<TransferItem>| -> Vec<TransferItem> {
        v.into_iter()
            .map(|i| TransferItem {
                src: absolutize(&i.src),
                dst: absolutize(&i.dst),
            })
            .collect()
    };
    match spec {
        JobSpec::Copy { items: v } => JobSpec::Copy { items: items(v) },
        JobSpec::Move { items: v } => JobSpec::Move { items: items(v) },
        JobSpec::Rename { pairs } => JobSpec::Rename {
            pairs: items(pairs),
        },
        JobSpec::BulkRename { steps } => JobSpec::BulkRename {
            steps: items(steps),
        },
        JobSpec::Trash { paths } => JobSpec::Trash {
            paths: paths.iter().map(|p| absolutize(p)).collect(),
        },
        JobSpec::DeletePermanent { paths } => JobSpec::DeletePermanent {
            paths: paths.iter().map(|p| absolutize(p)).collect(),
        },
        JobSpec::Create { mut entries } => {
            for e in &mut entries {
                e.path = absolutize(&e.path);
            }
            JobSpec::Create { entries }
        }
        JobSpec::Extract { archives, dest_dir } => JobSpec::Extract {
            archives: archives.iter().map(|p| absolutize(p)).collect(),
            dest_dir: absolutize(&dest_dir),
        },
        other @ JobSpec::Restore { .. } => other,
    }
}

fn empty_report(e: &Entry) -> JobReport {
    JobReport {
        job: e.id,
        generation: e.generation,
        kind: e.kind,
        items_done: 0,
        items_skipped: 0,
        bytes_done: 0,
        errors: Vec::new(),
        outcomes: Vec::new(),
        consumed_sources: Vec::new(),
        elapsed: Duration::ZERO,
    }
}

/// Stack size of job threads (virtual reservation; pages are committed on use).
const JOB_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Start queued jobs while slots are free.
fn dispatch(inner: &Arc<Inner>) {
    loop {
        let entry = {
            let mut st = lock(&inner.state);
            if st.running >= inner.max_concurrent {
                return;
            }
            let Some(id) = st.pending.pop_front() else {
                return;
            };
            let Some(entry) = st.jobs.get(&id).cloned() else {
                continue;
            };
            st.running += 1;
            entry
        };
        let inner2 = Arc::clone(inner);
        let spawn = std::thread::Builder::new()
            .name(format!("ks-job-{}", entry.id.0))
            // Tree walks recurse once per directory level; paths can be ~16k levels deep.
            .stack_size(JOB_STACK_BYTES)
            .spawn({
                let entry = Arc::clone(&entry);
                move || {
                    let _slot = SlotGuard {
                        inner: Arc::clone(&inner2),
                        entry: Arc::clone(&entry),
                    };
                    if catch_unwind(AssertUnwindSafe(|| run_job(inner2, entry))).is_err() {
                        tracing::error!("job thread panicked outside the job body");
                    }
                }
            });
        if let Err(e) = spawn {
            tracing::error!(error = %e, "cannot spawn job thread");
            let mut report = empty_report(&entry);
            report.errors.push(super::types::ItemError {
                path: Default::default(),
                message: format!("cannot start job thread: {e}"),
            });
            lock(&entry.info).0 = JobState::Failed;
            inner.sink.send(JobEvent::Failed(report));
            lock(&inner.state).running -= 1;
            inner.idle.notify_all();
        }
    }
}

fn run_job(inner: Arc<Inner>, entry: Arc<Entry>) {
    let Some((spec, resolutions)) = lock(&entry.start).take() else {
        return;
    };
    lock(&entry.info).0 = JobState::Running;

    // Wrap the sink to keep the per-job snapshot current.
    let sink: Arc<dyn EventSink<JobEvent>> = {
        let outer = Arc::clone(&inner.sink);
        let entry = Arc::clone(&entry);
        Arc::new(move |ev: JobEvent| {
            match &ev {
                JobEvent::Progress(p) => lock(&entry.info).1 = Some(p.clone()),
                JobEvent::ConflictPrompt(_) => lock(&entry.info).0 = JobState::AwaitingConflict,
                _ => {}
            }
            outer.send(ev);
        })
    };
    sink.send(JobEvent::Started {
        job: entry.id,
        generation: entry.generation,
        kind: entry.kind,
        description: entry.description.clone(),
    });

    let mut runner = Runner::new(
        entry.id,
        entry.generation,
        entry.kind,
        entry.token.clone(),
        Arc::clone(&sink),
        resolutions,
        entry.options.conflict,
    );
    let result = catch_unwind(AssertUnwindSafe(|| {
        execute(
            &mut runner,
            spec,
            entry.options.record_undo && entry.options.track_trash_ids,
        )
    }));
    let flow: Flow = match result {
        Ok(f) => f,
        Err(_) => {
            tracing::error!(job = entry.id.0, "job panicked");
            runner.error(std::path::Path::new(""), "internal error: the job panicked");
            Ok(())
        }
    };
    runner.tick(true);
    let cancelled = flow.is_err();
    let report = runner.into_report();

    if entry.options.record_undo {
        if let Some(undo) = UndoEntry::from_report(&report) {
            lock(&inner.undo).push(undo);
        }
    }
    let (state, event) = if cancelled {
        (JobState::Cancelled, JobEvent::Cancelled(report))
    } else if report.errors.is_empty() {
        (JobState::Done, JobEvent::Done(report))
    } else {
        (JobState::Failed, JobEvent::Failed(report))
    };
    lock(&entry.info).0 = state;
    sink.send(event);
}

/// Frees the concurrency slot when a job thread ends, even if it panicked (e.g. in the sink).
struct SlotGuard {
    inner: Arc<Inner>,
    entry: Arc<Entry>,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        {
            let mut info = lock(&self.entry.info);
            if matches!(info.0, JobState::Running | JobState::AwaitingConflict) {
                info.0 = JobState::Failed; // died without reporting
            }
        }
        lock(&self.inner.state).running -= 1;
        dispatch(&self.inner);
        self.inner.idle.notify_all();
    }
}

fn execute(r: &mut Runner, spec: JobSpec, track_ids: bool) -> Flow {
    match spec {
        JobSpec::Copy { items } => r.run_copy(items),
        JobSpec::Move { items } | JobSpec::Rename { pairs: items } => r.run_move(items),
        JobSpec::Trash { paths } => r.run_trash(paths, track_ids),
        JobSpec::DeletePermanent { paths } => r.run_delete(paths),
        JobSpec::Create { entries } => r.run_create(entries),
        JobSpec::BulkRename { steps } => r.run_steps(steps),
        JobSpec::Restore { items } => r.run_restore(items),
        JobSpec::Extract { archives, dest_dir } => r.run_extract(archives, dest_dir),
    }
}

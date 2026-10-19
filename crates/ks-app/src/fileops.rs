//! The file-operation half of the effect runner: owns the job engine and
//! the shell runs, maps the engine's events 1:1 to [`Msg`]s and executes the file effects.
//!
//! Everything blocking runs on `spawn_blocking` or on the engine's own threads; the engine's
//! sinks only push into an unbounded channel that a task forwards to the app's message queue,
//! so engine threads never wait for the UI and no sink ever blocks inside the async runtime.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ks_core::config::{EditorConfig, EditorTerminal};
use ks_core::fileops::{
    BulkPlanResult, BulkRetry, ConflictAnswer, ConflictInfo, ForegroundPurpose, ForegroundRequest,
    ForegroundSpec, ItemMeta, JobDone, JobMsg, JobProgress, JobRequest, JobStatus, OpKind,
    PathFormatKind, ShellMsg,
};
use ks_core::{Effect, JobId as CoreJobId, Level, Msg};
use ks_fs::launch::clipboard::{format_paths, Clipboard, PathFormat, SystemClipboard};
use ks_fs::launch::editor::is_terminal_editor;
use ks_fs::launch::{
    build_open_command, find_in_path, launch_handler, open_default, open_with_handlers_for_all,
    resolve_editor, spawn_detached, spawn_shell, split_command_line, CommandSpec, Editor, Env,
    ShellEvent, ShellHandle, ShellId, ShellRequest, ShellStream, SystemEnv,
};
use ks_fs::ops::{
    parse_create_input, BulkRename, ConflictChoice, ConflictPolicy, FileMeta, Generation,
    ItemError, JobEvent, JobId, JobKind, JobManager, JobOptions, JobReport, JobSpec, Outcome,
    Progress, Resolution, TransferItem,
};
use tokio::sync::mpsc::{unbounded_channel, Sender, UnboundedSender};
use tokio_util::sync::CancellationToken;

/// The built-in default of `[editor] open_at_line`; anything else is the user's template.
const DEFAULT_AT_LINE: &str = "code -g {file}:{line}";
/// A GUI editor that returns sooner than this without changing the file did not wait.
const IMMEDIATE_RETURN: Duration = Duration::from_millis(1500);
/// How long quitting waits for cancelled jobs to clean up after themselves.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(1500);
/// Editors of the VS Code family return immediately unless told to wait.
const WAIT_FLAG_EDITORS: &[&str] = &["code", "code-insiders", "codium", "cursor", "windsurf"];

/// What the runner needs from `[editor]` in the config.
#[derive(Clone, Debug, Default)]
pub struct EditorSettings {
    pub open: String,
    pub open_at_line: String,
    pub terminal: EditorTerminal,
}

impl EditorSettings {
    pub fn from_config(config: &EditorConfig) -> Self {
        Self {
            open: config.open.clone(),
            open_at_line: config.open_at_line.clone(),
            terminal: config.terminal,
        }
    }
}

/// An `Env` where `[editor] open` stands in for `$VISUAL`.
struct ConfigEnv<'a> {
    open: &'a str,
}

impl Env for ConfigEnv<'_> {
    fn var(&self, name: &str) -> Option<String> {
        if self.open.trim().is_empty() {
            return SystemEnv.var(name);
        }
        (name == "VISUAL").then(|| self.open.to_owned())
    }

    fn which(&self, name: &str) -> Option<PathBuf> {
        SystemEnv.which(name)
    }
}

fn stem(program: &str) -> String {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let lower = name.to_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(s) = lower.strip_suffix(ext) {
            return s.to_owned();
        }
    }
    lower
}

/// The command that opens `path` in the user's editor, and whether it needs the terminal.
pub fn editor_command(
    settings: &EditorSettings,
    path: &Path,
    line: Option<u32>,
) -> (CommandSpec, bool) {
    let mut editor = resolve_editor(&ConfigEnv {
        open: &settings.open,
    });
    apply_terminal_override(settings, &mut editor);
    if let Some(line) = line {
        if let Some(custom) = custom_at_line(settings, path, line) {
            let terminal = match settings.terminal {
                EditorTerminal::Always => true,
                EditorTerminal::Never => false,
                EditorTerminal::Auto => is_terminal_editor(&custom.program),
            };
            return (custom, terminal);
        }
    }
    let spec = build_open_command(&editor, path, line, None, None);
    (spec, editor.terminal)
}

fn apply_terminal_override(settings: &EditorSettings, editor: &mut Editor) {
    match settings.terminal {
        EditorTerminal::Always => editor.terminal = true,
        EditorTerminal::Never => editor.terminal = false,
        EditorTerminal::Auto => {}
    }
}

/// A user-written `open_at_line` template is a whole command line (`nvim +{line} {file}`).
fn custom_at_line(settings: &EditorSettings, path: &Path, line: u32) -> Option<CommandSpec> {
    let template = settings.open_at_line.trim();
    if template.is_empty() || template == DEFAULT_AT_LINE {
        return None;
    }
    let file = ks_fs::ops::paths::display(path);
    let mut tokens = split_command_line(template).into_iter();
    let program = tokens.next()?;
    let program = find_in_path(&program).map_or(program, |p| p.to_string_lossy().into_owned());
    let mut args: Vec<String> = tokens
        .map(|t| {
            t.replace("{file}", &file)
                .replace("{line}", &line.to_string())
                .replace("{col}", "1")
        })
        .collect();
    if !template.contains("{file}") {
        args.push(file);
    }
    Some(CommandSpec { program, args })
}

/// Makes VS Code style editors block until the file is closed.
fn ensure_wait_flag(spec: &mut CommandSpec) {
    if !WAIT_FLAG_EDITORS.contains(&stem(&spec.program).as_str()) {
        return;
    }
    if spec.args.iter().any(|a| a == "-w" || a == "--wait") {
        return;
    }
    spec.args.insert(0, "--wait".to_owned());
}

/// Runs a GUI program to completion without touching the terminal.
fn run_and_wait(spec: &CommandSpec) -> io::Result<ExitStatus> {
    let mut cmd = spec.to_command();
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.status()
}

// ----- engine event mapping -----

fn op_kind(kind: JobKind) -> OpKind {
    match kind {
        JobKind::Copy => OpKind::Copy,
        JobKind::Move => OpKind::Move,
        JobKind::Trash => OpKind::Trash,
        JobKind::DeletePermanent => OpKind::Delete,
        JobKind::Create => OpKind::Create,
        JobKind::Rename => OpKind::Rename,
        JobKind::BulkRename => OpKind::BulkRename,
        JobKind::Restore => OpKind::Restore,
        JobKind::Extract => OpKind::Extract,
    }
}

fn core_id(id: JobId) -> CoreJobId {
    CoreJobId(id.0)
}

fn plain(path: &Path) -> PathBuf {
    dunce::simplified(path).to_path_buf()
}

fn item_meta(meta: &FileMeta) -> ItemMeta {
    ItemMeta {
        is_dir: meta.is_dir,
        is_link: meta.is_link,
        len: meta.len,
        modified: meta.modified,
    }
}

fn progress(p: &Progress) -> JobProgress {
    JobProgress {
        bytes_done: p.bytes_done,
        bytes_total: p.bytes_total,
        files_done: p.files_done,
        files_total: p.files_total,
        current: p.current.as_deref().map(plain),
        throughput_bps: p.throughput_bps,
        eta_secs: p.eta.map(|d| d.as_secs()),
    }
}

fn error_line(error: &ItemError) -> String {
    let name = error.path.file_name().map_or_else(
        || error.path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    format!("{name}: {}", error.message)
}

fn done(report: &JobReport, status: JobStatus, is_undo: bool) -> JobDone {
    let mut created = Vec::new();
    let mut removed = Vec::new();
    for outcome in &report.outcomes {
        match outcome {
            Outcome::Copied { dst, .. } => created.push(plain(dst)),
            Outcome::Moved { src, dst } => {
                removed.push(plain(src));
                created.push(plain(dst));
            }
            Outcome::Renamed { from, to } => {
                removed.push(plain(from));
                created.push(plain(to));
            }
            Outcome::Created { path, .. } | Outcome::Restored { path } => created.push(plain(path)),
            Outcome::Trashed { path, .. } | Outcome::Deleted { path } => removed.push(plain(path)),
        }
    }
    JobDone {
        id: core_id(report.job),
        kind: op_kind(report.kind),
        status,
        items_done: report.items_done,
        items_skipped: report.items_skipped,
        bytes_done: report.bytes_done,
        errors: report.errors.iter().map(error_line).collect(),
        created,
        removed,
        consumed_sources: report.consumed_sources.iter().map(|p| plain(p)).collect(),
        is_undo,
        elapsed_ms: u64::try_from(report.elapsed.as_millis()).unwrap_or(u64::MAX),
    }
}

/// One engine event as one message.
pub fn map_job_event(event: JobEvent, is_undo: bool) -> Msg {
    let msg = match event {
        JobEvent::Queued {
            job,
            kind,
            description,
            ..
        } => JobMsg::Queued {
            id: core_id(job),
            kind: op_kind(kind),
            description,
        },
        JobEvent::Started {
            job,
            kind,
            description,
            ..
        } => JobMsg::Started {
            id: core_id(job),
            kind: op_kind(kind),
            description,
        },
        JobEvent::Scanning {
            job, files, bytes, ..
        } => JobMsg::Scanning {
            id: core_id(job),
            files,
            bytes,
        },
        JobEvent::Progress(p) => JobMsg::Progress {
            id: core_id(p.job),
            progress: progress(&p),
        },
        JobEvent::ConflictPrompt(c) => JobMsg::Conflict(ConflictInfo {
            job: core_id(c.job),
            src: plain(&c.src),
            dst: plain(&c.dst),
            src_meta: item_meta(&c.src_meta),
            dst_meta: item_meta(&c.dst_meta),
        }),
        JobEvent::Done(r) => JobMsg::Finished(done(&r, JobStatus::Done, is_undo)),
        JobEvent::Failed(r) => JobMsg::Finished(done(&r, JobStatus::Failed, is_undo)),
        JobEvent::Cancelled(r) => JobMsg::Finished(done(&r, JobStatus::Cancelled, is_undo)),
    };
    Msg::Job(msg)
}

/// One shell event as one message.
pub fn map_shell_event(event: ShellEvent) -> Msg {
    Msg::Shell(match event {
        ShellEvent::Line {
            id, text, stream, ..
        } => ShellMsg::Line {
            id: id.0,
            text,
            stderr: stream == ShellStream::Stderr,
        },
        ShellEvent::Exited { id, code, .. } => ShellMsg::Exited { id: id.0, code },
        ShellEvent::Cancelled { id, .. } => ShellMsg::Cancelled { id: id.0 },
        ShellEvent::SpawnFailed { id, error, .. } => ShellMsg::SpawnFailed { id: id.0, error },
    })
}

fn pairs(items: Vec<(PathBuf, PathBuf)>) -> Vec<TransferItem> {
    items
        .into_iter()
        .map(|(src, dst)| TransferItem::new(src, dst))
        .collect()
}

fn spec_of(request: JobRequest) -> JobSpec {
    match request {
        JobRequest::Copy { items } => JobSpec::Copy {
            items: pairs(items),
        },
        JobRequest::Move { items } => JobSpec::Move {
            items: pairs(items),
        },
        JobRequest::Trash { paths } => JobSpec::Trash { paths },
        JobRequest::DeletePermanent { paths } => JobSpec::DeletePermanent { paths },
        JobRequest::BulkRename { steps } => JobSpec::BulkRename {
            steps: pairs(steps),
        },
        JobRequest::Extract { archives, dest_dir } => JobSpec::Extract { archives, dest_dir },
    }
}

fn choice_of(answer: ConflictAnswer) -> ConflictChoice {
    match answer {
        ConflictAnswer::Overwrite => ConflictChoice::Overwrite,
        ConflictAnswer::Skip => ConflictChoice::Skip,
        ConflictAnswer::Rename(name) => ConflictChoice::Rename(name),
        ConflictAnswer::KeepBoth => ConflictChoice::KeepBoth,
        ConflictAnswer::Cancel => ConflictChoice::Cancel,
    }
}

// ----- the runner -----

/// The clipboard behind a lock, replaceable so tests never touch the real one.
type SharedClipboard = Arc<Mutex<Box<dyn Clipboard + Send>>>;

/// Generation of undo jobs; every other job uses `Generation(0)`.
const UNDO_GENERATION: Generation = Generation(1);

pub struct FileOpsRunner {
    jobs: Arc<JobManager>,
    tx: Sender<Msg>,
    /// Messages produced on engine threads, forwarded to `tx` by a task.
    engine_tx: UnboundedSender<Msg>,
    shells: HashMap<u64, ShellHandle>,
    editor: EditorSettings,
    clipboard: SharedClipboard,
}

impl FileOpsRunner {
    /// Must be called inside a tokio runtime (it spawns the forwarding task).
    pub fn new(tx: Sender<Msg>, shutdown: CancellationToken) -> Self {
        let (engine_tx, mut engine_rx) = unbounded_channel::<Msg>();
        let forward_tx = tx.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    msg = engine_rx.recv() => match msg {
                        Some(msg) => {
                            if forward_tx.send(msg).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                }
            }
        });
        let sink_tx = engine_tx.clone();
        let jobs = JobManager::new(move |event: JobEvent| {
            // Undo jobs carry their own generation, so no shared state is needed to tell.
            let is_undo = event.generation() == UNDO_GENERATION;
            let _ = sink_tx.send(map_job_event(event, is_undo));
        });
        Self {
            jobs: Arc::new(jobs),
            tx,
            engine_tx,
            shells: HashMap::new(),
            editor: EditorSettings::default(),
            clipboard: Arc::new(Mutex::new(Box::new(SystemClipboard))),
        }
    }

    /// Replaces the clipboard `Y` writes to (tests).
    pub fn set_clipboard(&mut self, clipboard: impl Clipboard + Send + 'static) {
        self.clipboard = Arc::new(Mutex::new(Box::new(clipboard)));
    }

    pub fn configure(&mut self, editor: EditorSettings) {
        self.editor = editor;
    }

    /// The job engine, for tests and diagnostics.
    pub fn manager(&self) -> &JobManager {
        &self.jobs
    }

    /// Cancels every job and shell command still running.
    pub fn shutdown(&mut self) {
        self.jobs.cancel_all();
        for (_, shell) in self.shells.drain() {
            shell.cancel();
        }
        // Job threads remove their partial files when they notice the cancel; give them a
        // moment, since the process may exit right after this.
        if !self.jobs.wait_idle(SHUTDOWN_GRACE) {
            tracing::warn!("jobs still running at exit");
        }
    }

    fn notify(tx: &Sender<Msg>, level: Level, text: impl Into<String>) {
        let _ = tx.blocking_send(Msg::Notify {
            level,
            text: text.into(),
        });
    }

    /// Runs one file-operation effect; other effects are not this runner's business.
    pub fn handle(&mut self, effect: Effect) {
        match effect {
            Effect::SubmitJob { request, keep_both } => self.submit(spec_of(request), keep_both),
            Effect::Rename { from, new_name } => match JobSpec::rename(&from, &new_name) {
                Ok(spec) => self.submit(spec, false),
                Err(error) => self.notify_now(Level::Error, format!("Cannot rename: {error}")),
            },
            Effect::Create { base, input } => match parse_create_input(&base, &input) {
                Ok(target) => self.submit(
                    JobSpec::Create {
                        entries: vec![target],
                    },
                    false,
                ),
                Err(error) => self.notify_now(Level::Error, format!("Cannot create: {error}")),
            },
            Effect::Undo => self.undo(),
            Effect::CancelJob(id) => {
                self.jobs.cancel(JobId(id.0));
            }
            Effect::ResolveConflict {
                job,
                answer,
                apply_to_all,
            } => {
                let resolution = Resolution {
                    choice: choice_of(answer),
                    apply_to_all,
                };
                if !self.jobs.resolve(JobId(job.0), resolution) {
                    tracing::debug!(job = job.0, "conflict answer for a job that is not waiting");
                }
            }
            Effect::OpenDefault { path } => self.open_default(path),
            Effect::OpenInEditor { path, line } => self.open_in_editor(path, line),
            Effect::ListOpenHandlers { paths } => self.list_handlers(paths),
            Effect::OpenWith { paths, handler } => self.open_with(paths, handler),
            Effect::CopyPaths { paths, format, cwd } => self.copy_paths(paths, format, cwd),
            Effect::RunShell { id, command, cwd } => self.run_shell(id, command, cwd),
            Effect::CancelShell(id) => {
                if let Some(shell) = self.shells.get(&id) {
                    shell.cancel();
                }
            }
            Effect::BulkRenameEdit { dir, names, reuse } => self.bulk_edit(dir, names, reuse),
            Effect::BulkRenameDiscard { file } => {
                tokio::task::spawn_blocking(move || {
                    if let Err(error) = std::fs::remove_file(&file) {
                        tracing::warn!(file = %file.display(), %error, "bulk rename temp file not removed");
                    }
                });
            }
            Effect::BulkRenamePlan {
                dir,
                names,
                file,
                error,
            } => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = plan_bulk(&dir, names, &file, error);
                    let _ = tx.blocking_send(Msg::BulkPlanned(result));
                });
            }
            other => tracing::warn!(?other, "effect is not a file operation"),
        }
    }

    /// Notification from the (async) caller context.
    fn notify_now(&self, level: Level, text: String) {
        let _ = self.engine_tx.send(Msg::Notify { level, text });
    }

    fn submit(&mut self, spec: JobSpec, keep_both: bool) {
        let options = JobOptions {
            conflict: if keep_both {
                ConflictPolicy::KeepBoth
            } else {
                ConflictPolicy::Ask
            },
            ..JobOptions::default()
        };
        let id = self.jobs.submit_with(spec, Generation(0), options);
        tracing::debug!(job = id.0, "job submitted");
    }

    fn undo(&mut self) {
        let jobs = Arc::clone(&self.jobs);
        let engine_tx = self.engine_tx.clone();
        // Undo verifies the file system before it starts, so it must not run on the loop.
        tokio::task::spawn_blocking(move || {
            if let Err(error) = jobs.undo(UNDO_GENERATION) {
                let _ = engine_tx.send(Msg::Notify {
                    level: Level::Warning,
                    text: error.to_string(),
                });
            }
        });
    }

    fn open_default(&self, path: PathBuf) {
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(error) = open_default(&path) {
                tracing::warn!(path = %path.display(), %error, "open failed");
                Self::notify(&tx, Level::Error, format!("Cannot open: {error}"));
            }
        });
    }

    fn open_in_editor(&self, path: PathBuf, line: Option<u32>) {
        let tx = self.tx.clone();
        let settings = self.editor.clone();
        tokio::task::spawn_blocking(move || {
            let (spec, terminal) = editor_command(&settings, &path, line);
            if terminal {
                let _ = tx.blocking_send(Msg::Foreground(ForegroundRequest {
                    spec: ForegroundSpec {
                        program: spec.program,
                        args: spec.args,
                    },
                    purpose: ForegroundPurpose::Editor,
                }));
            } else if let Err(error) = spawn_detached(&spec) {
                tracing::warn!(program = %spec.program, %error, "editor failed to start");
                Self::notify(
                    &tx,
                    Level::Error,
                    format!("Cannot start {}: {error}", stem(&spec.program)),
                );
            }
        });
    }

    fn list_handlers(&self, paths: Vec<PathBuf>) {
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            // The registry is read here, off the UI thread.
            let names = open_with_handlers_for_all(&paths)
                .into_iter()
                .map(|h| h.name)
                .collect();
            let _ = tx.blocking_send(Msg::OpenHandlers { paths, names });
        });
    }

    fn open_with(&self, paths: Vec<PathBuf>, handler: String) {
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let Some(found) = open_with_handlers_for_all(&paths)
                .into_iter()
                .find(|h| h.name == handler)
            else {
                Self::notify(&tx, Level::Warning, format!("{handler} is not available"));
                return;
            };
            for path in &paths {
                if let Err(error) = launch_handler(&found, path) {
                    Self::notify(
                        &tx,
                        Level::Error,
                        format!("Cannot start {handler}: {error}"),
                    );
                    break;
                }
            }
        });
    }

    fn copy_paths(&self, paths: Vec<PathBuf>, format: PathFormatKind, cwd: PathBuf) {
        let tx = self.tx.clone();
        let clipboard = Arc::clone(&self.clipboard);
        tokio::task::spawn_blocking(move || {
            let format = match format {
                PathFormatKind::Absolute => PathFormat::Absolute,
                PathFormatKind::Relative => PathFormat::RelativeToCwd,
                PathFormatKind::Name => PathFormat::Name,
            };
            let text = format_paths(&paths, format, &cwd);
            let result = clipboard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .set_text(&text);
            match result {
                Ok(()) => {
                    let noun = if paths.len() == 1 { "path" } else { "paths" };
                    Self::notify(
                        &tx,
                        Level::Info,
                        format!("Copied {} {noun} to the clipboard", paths.len()),
                    );
                }
                Err(error) => Self::notify(&tx, Level::Error, error.to_string()),
            }
        });
    }

    fn run_shell(&mut self, id: u64, command: String, cwd: PathBuf) {
        // Forget finished runs so the table stays small.
        self.shells.retain(|_, handle| !handle.is_finished());
        let engine_tx = self.engine_tx.clone();
        let handle = spawn_shell(
            ShellRequest {
                id: ShellId(id),
                generation: Generation(id),
                command,
                cwd,
                shell: None,
            },
            move |event: ShellEvent| {
                let _ = engine_tx.send(map_shell_event(event));
            },
        );
        self.shells.insert(id, handle);
    }

    fn bulk_edit(&self, dir: PathBuf, names: Vec<String>, reuse: Option<PathBuf>) {
        let tx = self.tx.clone();
        let settings = self.editor.clone();
        tokio::task::spawn_blocking(move || {
            let session = BulkRename::new(dir.clone(), names.clone());
            // A kept file that was removed meanwhile (temp cleaner, user) is written afresh.
            let reuse = reuse.filter(|file| file.is_file());
            let file = match reuse.map_or_else(|| session.write_edit_file(), Ok) {
                Ok(file) => file,
                Err(error) => {
                    Self::notify(
                        &tx,
                        Level::Error,
                        format!("Cannot create the edit file: {error}"),
                    );
                    return;
                }
            };
            let (mut spec, terminal) = editor_command(&settings, &file, None);
            if terminal {
                let _ = tx.blocking_send(Msg::Foreground(ForegroundRequest {
                    spec: ForegroundSpec {
                        program: spec.program,
                        args: spec.args,
                    },
                    purpose: ForegroundPurpose::BulkEdit { dir, names, file },
                }));
                return;
            }
            // A GUI editor: leave the TUI alone and wait until the file is closed.
            ensure_wait_flag(&mut spec);
            Self::notify(
                &tx,
                Level::Info,
                "Waiting for the editor - save and close the file to continue",
            );
            let started = std::time::Instant::now();
            let error = match run_and_wait(&spec) {
                Ok(status) if status.success() => None,
                Ok(status) => Some(format!("the editor exited with {status}")),
                Err(error) => Some(format!("cannot start {}: {error}", stem(&spec.program))),
            };
            let mut result = plan_bulk(&dir, names, &file, error);
            if result == BulkPlanResult::Unchanged && started.elapsed() < IMMEDIATE_RETURN {
                result = BulkPlanResult::Failed(
                    "the editor returned immediately; set $EDITOR to one that waits (code --wait)"
                        .to_owned(),
                );
            }
            let _ = tx.blocking_send(Msg::BulkPlanned(result));
        });
    }
}

/// Validates the edited file and removes it.
fn plan_bulk(dir: &Path, names: Vec<String>, file: &Path, error: Option<String>) -> BulkPlanResult {
    let session = BulkRename::new(dir, names.clone());
    let result = match error {
        Some(error) => BulkPlanResult::Failed(format!("cancelled: {error}")),
        None => match session.plan_from_file(file) {
            Ok(Ok(plan)) if plan.is_empty() => BulkPlanResult::Unchanged,
            Ok(Ok(plan)) => BulkPlanResult::Planned {
                preview: plan.preview(),
                steps: plan
                    .steps
                    .iter()
                    .map(|s| (s.src.clone(), s.dst.clone()))
                    .collect(),
            },
            Ok(Err(problems)) => {
                // Keep the file: the user gets to fix it instead of starting over.
                return BulkPlanResult::Invalid {
                    problems: problems.iter().map(ToString::to_string).collect(),
                    retry: Some(BulkRetry {
                        dir: dir.to_owned(),
                        names,
                        file: file.to_owned(),
                    }),
                };
            }
            Err(error) => BulkPlanResult::Failed(format!("cannot read the edited file: {error}")),
        },
    };
    if let Err(error) = std::fs::remove_file(file) {
        tracing::warn!(file = %file.display(), %error, "bulk rename temp file not removed");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_flag_is_added_for_vscode_family_only() {
        let mut code = CommandSpec {
            program: r"C:\Users\x\AppData\Local\Programs\Microsoft VS Code\bin\code.cmd".into(),
            args: vec!["a.txt".into()],
        };
        ensure_wait_flag(&mut code);
        assert_eq!(code.args, ["--wait", "a.txt"]);
        ensure_wait_flag(&mut code);
        assert_eq!(code.args, ["--wait", "a.txt"], "idempotent");

        let mut waits = CommandSpec {
            program: "code".into(),
            args: vec!["-w".into(), "a.txt".into()],
        };
        ensure_wait_flag(&mut waits);
        assert_eq!(waits.args, ["-w", "a.txt"]);

        let mut notepad = CommandSpec {
            program: "notepad.exe".into(),
            args: vec!["a.txt".into()],
        };
        ensure_wait_flag(&mut notepad);
        assert_eq!(notepad.args, ["a.txt"]);
    }

    #[test]
    fn terminal_override_beats_the_known_list() {
        let path = Path::new("a.txt");
        let mut settings = EditorSettings {
            open: "notepad".into(),
            terminal: EditorTerminal::Always,
            ..EditorSettings::default()
        };
        let (_, terminal) = editor_command(&settings, path, None);
        assert!(terminal);
        settings.terminal = EditorTerminal::Never;
        let (_, terminal) = editor_command(&settings, path, None);
        assert!(!terminal);
    }

    #[test]
    fn terminal_or_gui_classification_table() {
        // (program, [editor] terminal, runs attached to the terminal)
        use EditorTerminal::{Always, Auto, Never};
        let table = [
            ("nvim", Auto, true),
            ("hx", Auto, true),
            ("vim", Auto, true),
            ("code", Auto, false),
            ("notepad", Auto, false),
            ("notepad", Always, true),
            ("nvim", Never, false),
        ];
        for (program, terminal, want) in table {
            let settings = EditorSettings {
                open_at_line: format!("{program} {{file}}"),
                terminal,
                ..EditorSettings::default()
            };
            let (_, got) = editor_command(&settings, Path::new("a.rs"), Some(3));
            assert_eq!(got, want, "{program} with {terminal:?}");
        }
    }

    #[test]
    fn custom_open_at_line_template_is_a_whole_command() {
        let settings = EditorSettings {
            open_at_line: "nvim +{line} {file}".into(),
            ..EditorSettings::default()
        };
        let (spec, terminal) = editor_command(&settings, Path::new("src/a b.rs"), Some(42));
        assert!(stem(&spec.program) == "nvim" || spec.program == "nvim");
        assert_eq!(spec.args[0], "+42");
        assert!(spec.args[1].ends_with("a b.rs"), "{:?}", spec.args);
        assert!(terminal, "nvim is a terminal editor");
    }

    #[test]
    fn default_template_defers_to_the_editor_specific_one() {
        let settings = EditorSettings {
            open: "nvim".into(),
            ..EditorSettings::default()
        };
        // `nvim` may not be installed, in which case the fallback editor is used; the
        // template is then the fallback's, which is still not the literal default.
        let (spec, _) = editor_command(&settings, Path::new("a.rs"), Some(7));
        assert!(!spec.args.is_empty());
    }

    #[test]
    fn engine_events_map_one_to_one() {
        use ks_fs::ops::{ConflictPrompt, Generation as G};
        let generation = G(0);
        let msg = map_job_event(
            JobEvent::Queued {
                job: JobId(3),
                generation,
                kind: JobKind::DeletePermanent,
                description: "Deleting x".into(),
            },
            false,
        );
        assert_eq!(
            msg,
            Msg::Job(JobMsg::Queued {
                id: CoreJobId(3),
                kind: OpKind::Delete,
                description: "Deleting x".into()
            })
        );

        let prompt = ConflictPrompt {
            job: JobId(4),
            generation,
            src: PathBuf::from("a"),
            dst: PathBuf::from("b"),
            src_meta: FileMeta {
                is_dir: false,
                is_link: false,
                len: 1,
                modified: None,
            },
            dst_meta: FileMeta {
                is_dir: true,
                is_link: false,
                len: 0,
                modified: None,
            },
        };
        let Msg::Job(JobMsg::Conflict(info)) =
            map_job_event(JobEvent::ConflictPrompt(prompt), false)
        else {
            panic!("not a conflict");
        };
        assert_eq!(info.job, CoreJobId(4));
        assert!(info.dst_meta.is_dir);

        let report = JobReport {
            job: JobId(5),
            generation,
            kind: JobKind::Move,
            items_done: 1,
            items_skipped: 0,
            bytes_done: 9,
            errors: vec![ItemError {
                path: PathBuf::from("dir/x.txt"),
                message: "Access is denied.".into(),
            }],
            outcomes: vec![Outcome::Moved {
                src: PathBuf::from("a/x"),
                dst: PathBuf::from("b/x"),
            }],
            consumed_sources: Vec::new(),
            elapsed: Duration::from_millis(12),
        };
        let Msg::Job(JobMsg::Finished(finished)) = map_job_event(JobEvent::Failed(report), true)
        else {
            panic!("not finished");
        };
        assert_eq!(finished.status, JobStatus::Failed);
        assert_eq!(finished.created, [PathBuf::from("b/x")]);
        assert_eq!(finished.removed, [PathBuf::from("a/x")]);
        assert_eq!(finished.errors, ["x.txt: Access is denied."]);
        assert!(finished.is_undo);
    }

    #[test]
    fn shell_events_map_streams() {
        let msg = map_shell_event(ShellEvent::Line {
            id: ShellId(2),
            generation: Generation(2),
            stream: ShellStream::Stderr,
            text: "oops".into(),
        });
        assert_eq!(
            msg,
            Msg::Shell(ShellMsg::Line {
                id: 2,
                text: "oops".into(),
                stderr: true
            })
        );
    }
}

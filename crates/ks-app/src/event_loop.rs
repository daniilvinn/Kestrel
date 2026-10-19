//! The Elm-style main loop: events and messages in, `update`, effects out, redraw when dirty.
//!
//! Generic over the ratatui backend and the event source so it can be driven headlessly in
//! tests with a `TestBackend` and a scripted event stream.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Result};
use crossterm::event::Event;
use futures_util::{Stream, StreamExt};
use ks_core::{update, AppState, Effect, Level, Msg, QuitKind};
use ks_ui::Theme;
use ratatui::backend::Backend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio::time::{interval, interval_at, sleep, Instant, MissedTickBehavior};

use crate::claude::ClaudeOptions;
use crate::effects::EffectRunner;
use crate::foreground::Foreground;
use crate::input::{coalesce_burst, is_paste_key, translate_event, translate_key};
use crate::reload::Reloader;

/// Redraws are capped at roughly 60 frames per second.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// The job spinner animates at 10 Hz, and only while jobs are running.
pub const SPINNER_INTERVAL: Duration = Duration::from_millis(100);
/// How often the wall clock behind relative times ("3m ago") is refreshed.
pub const CLOCK_INTERVAL: Duration = Duration::from_secs(30);

const MSG_QUEUE: usize = 256;
/// How long quitting waits for the state files to be written.
const STATE_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct LoopOptions {
    /// Messages to process before the first frame (start-up notices).
    pub startup: Vec<Msg>,
    /// Panic right after the first frame (`--debug-panic`).
    pub panic_after_first_frame: bool,
    /// Runs programs attached to the terminal (terminal editors); `None` in headless runs.
    pub foreground: Option<Box<dyn Foreground>>,
    /// Where the Claude integration keeps its files.
    pub claude: ClaudeOptions,
}

/// Runs until the state requests quit, returning how. Config reloads, theme switches and
/// state persistence are unavailable (see [`run_with`] and [`run_with_state`]).
pub async fn run<B, E>(
    terminal: &mut Terminal<B>,
    events: E,
    state: &mut AppState,
    theme: &Theme,
    options: LoopOptions,
) -> Result<QuitKind>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
    E: Stream<Item = std::io::Result<Event>>,
{
    run_with(terminal, events, state, theme, options, None).await
}

/// Results of file work done off the loop thread (config and theme reads), delivered
/// through a side channel because `Msg` cannot carry a `Theme`.
pub(crate) enum Side {
    Msg(Msg),
    Theme {
        name: String,
        theme: Box<Theme>,
        announce: bool,
    },
}

/// Applies `msg` and everything it leads to. Effects that change the application itself
/// (reloading the config, switching the theme) are started here, on blocking threads,
/// because the result needs the loop's theme; the rest go to the effect runner.
fn dispatch<B: Backend>(
    state: &mut AppState,
    reloader: Option<&Arc<Reloader>>,
    side: &mpsc::UnboundedSender<Side>,
    runner: &mut EffectRunner,
    foreground: &mut Option<Box<dyn Foreground>>,
    terminal: &mut Terminal<B>,
    msg: Msg,
) {
    let mut queue = VecDeque::from([msg]);
    while let Some(msg) = queue.pop_front() {
        let mut for_runner = Vec::new();
        let reloaded = matches!(msg, Msg::ConfigReloaded { .. });
        let effects = update(state, msg);
        if reloaded {
            // `[editor]` settings live in the runner.
            runner.configure(&state.config);
        }
        for effect in effects {
            match effect {
                Effect::ReloadConfig => match reloader {
                    Some(reloader) => {
                        let (reloader, side) = (Arc::clone(reloader), side.clone());
                        tokio::task::spawn_blocking(move || {
                            let _ = side.send(Side::Msg(reloader.reload_config()));
                            let _ = side.send(Side::Msg(Msg::ThemesAvailable(reloader.themes())));
                            reloader.job_done(crate::reload::JobKind::Reload, &side);
                        });
                    }
                    None => queue.push_back(unavailable("Config reload")),
                },
                Effect::SetTheme { name, announce } => match reloader {
                    Some(reloader) => {
                        let (reloader, side) = (Arc::clone(reloader), side.clone());
                        tokio::task::spawn_blocking(move || {
                            let result = match reloader.load_theme(&name) {
                                Ok(theme) => Side::Theme {
                                    name,
                                    theme: Box::new(theme),
                                    announce,
                                },
                                Err(message) => Side::Msg(Msg::Notify {
                                    level: Level::Error,
                                    text: message,
                                }),
                            };
                            let _ = side.send(result);
                            reloader.job_done(crate::reload::JobKind::Theme, &side);
                        });
                    }
                    None => queue.push_back(unavailable("Theme switching")),
                },
                Effect::RunForeground { id, spec } => {
                    // Earlier effects of this batch start before the terminal is handed over.
                    runner.run(std::mem::take(&mut for_runner));
                    queue.extend(run_foreground(state, foreground, terminal, id, &spec));
                }
                other => for_runner.push(other),
            }
        }
        runner.run(for_runner);
    }
}

/// Turns a side-channel result into the message to apply next (adopting a loaded theme).
fn take_side(side: Side, theme: &mut Theme, tx: &mpsc::Sender<Msg>) -> Msg {
    match side {
        Side::Msg(msg) => msg,
        Side::Theme {
            name,
            theme: loaded,
            announce,
        } => {
            *theme = *loaded;
            if announce {
                let _ = tx.try_send(Msg::Notify {
                    level: Level::Info,
                    text: format!("Theme: {name}"),
                });
            }
            Msg::ThemeApplied(name)
        }
    }
}

fn unavailable(what: &str) -> Msg {
    Msg::Notify {
        level: Level::Warning,
        text: format!("{what} is not available here"),
    }
}

/// [`run`] with a [`Reloader`] that performs config reloads and theme switches.
pub async fn run_with<B, E>(
    terminal: &mut Terminal<B>,
    events: E,
    state: &mut AppState,
    theme: &Theme,
    options: LoopOptions,
    reloader: Option<Reloader>,
) -> Result<QuitKind>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
    E: Stream<Item = std::io::Result<Event>>,
{
    run_with_state(terminal, events, state, theme, options, reloader, None).await
}

/// [`run_with`], persisting bookmarks and frecency in `state_dir` (loaded in the background
/// after start, the last changes flushed on exit).
pub async fn run_with_state<B, E>(
    terminal: &mut Terminal<B>,
    events: E,
    state: &mut AppState,
    theme: &Theme,
    options: LoopOptions,
    reloader: Option<Reloader>,
    state_dir: Option<std::path::PathBuf>,
) -> Result<QuitKind>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
    E: Stream<Item = std::io::Result<Event>>,
{
    let mut theme = theme.clone();
    let reloader = reloader.map(Arc::new);
    let reloader = reloader.as_ref();
    let (tx, mut rx) = mpsc::channel::<Msg>(MSG_QUEUE);
    let (side_tx, mut side_rx) = mpsc::unbounded_channel::<Side>();
    let loop_tx = tx.clone();
    let mut runner = EffectRunner::new(tx.clone()).with_claude(options.claude);
    if let Some(dir) = state_dir {
        runner.set_state_dir(dir);
    }
    runner.configure(&state.config);
    let mut foreground = options.foreground;
    runner.set_preview_palette(theme.syntax_palette());
    // A background image encode finished: ask for a redraw. `try_send` because the notifier
    // runs on a worker thread and a full queue already guarantees a redraw.
    let image_tx = tx.clone();
    ks_ui::images::set_notifier(move || {
        let _ = image_tx.try_send(Msg::ImageReady);
    });
    spawn_interrupt_listener(tx);
    let mut events = std::pin::pin!(events);

    let mut spinner = interval(SPINNER_INTERVAL);
    spinner.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut clock = interval_at(Instant::now() + CLOCK_INTERVAL, CLOCK_INTERVAL);
    clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_draw: Option<Instant> = None;
    let mut panic_armed = options.panic_after_first_frame;
    let mut first_frame_done = false;

    if let Some(reloader) = reloader {
        let (reloader, side) = (Arc::clone(reloader), side_tx.clone());
        tokio::task::spawn_blocking(move || {
            let _ = side.send(Side::Msg(Msg::ThemesAvailable(reloader.themes())));
        });
    }
    for msg in options.startup {
        dispatch(
            state,
            reloader,
            &side_tx,
            &mut runner,
            &mut foreground,
            terminal,
            msg,
        );
    }

    let outcome: Result<QuitKind> = loop {
        if state.dirty && last_draw.is_none_or(|t| t.elapsed() >= FRAME_INTERVAL) {
            if let Err(e) = terminal.draw(|frame| ks_ui::render(frame, state, &theme)) {
                break Err(anyhow::Error::new(e).context("drawing a frame failed"));
            }
            state.dirty = false;
            last_draw = Some(Instant::now());
            crate::terminal::set_title(&ks_ui::render::window_title(state));
            if !first_frame_done {
                first_frame_done = true;
                tracing::info!(
                    elapsed_ms = crate::startup::since_process_start()
                        .map_or(-1, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
                    "first frame"
                );
                if let Some(fg) = foreground.as_mut() {
                    fg.first_frame_drawn();
                }
            }
            // Terminal graphics (Sixel) stay on screen until overpainted: when the image
            // moved or went away, wipe the screen and draw again.
            if ks_ui::images::finish_frame() {
                if let Err(e) = terminal.clear() {
                    break Err(anyhow::Error::new(e).context("clearing the screen failed"));
                }
                state.dirty = true;
                // Redraw at once: the throttle must not leave the wiped screen up.
                last_draw = None;
            }
            if std::mem::take(&mut panic_armed) {
                panic!("--debug-panic: forced panic after the first frame");
            }
        }
        if let Some(kind) = state.quit {
            // Whatever the last messages changed is drawn once more, throttle or not.
            if state.dirty {
                let _ = terminal.draw(|frame| ks_ui::render(frame, state, &theme));
                state.dirty = false;
            }
            break Ok(kind);
        }

        // Wake for the next frame only when something is waiting to be drawn.
        let frame_due = state.dirty.then(|| {
            last_draw.map_or(Duration::ZERO, |t| {
                FRAME_INTERVAL.saturating_sub(t.elapsed())
            })
        });

        let first = tokio::select! {
            event = events.next() => match event {
                Some(Ok(event)) => translate_event(event),
                Some(Err(e)) => break Err(anyhow!(e).context("reading terminal input failed")),
                None => break Err(anyhow!("terminal input stream closed")),
            },
            Some(msg) = rx.recv() => Some(msg),
            Some(side) = side_rx.recv() => {
                let msg = take_side(side, &mut theme, &loop_tx);
                // A theme switch changes the syntax colours of text previews.
                runner.set_preview_palette(theme.syntax_palette());
                Some(msg)
            }
            _ = spinner.tick(), if state.has_running_jobs() || state.search.is_busy() => Some(Msg::Tick),
            _ = clock.tick() => Some(Msg::Clock {
                now: SystemTime::now(),
                utc_offset_secs: ks_fs::utc_offset_secs(),
            }),
            () = sleep(frame_due.unwrap_or_default()), if frame_due.is_some() => None,
        };

        // Windows never reports bracketed pastes: with the Claude pane focused, a run of text
        // keys that are all ready at once is turned into a paste (see `coalesce_burst`).
        let mut ordered: std::collections::VecDeque<Msg> = std::collections::VecDeque::new();
        match first {
            Some(Msg::Key(chord)) if is_paste_key(chord) && ks_core::claude::has_focus(state) => {
                let mut run = vec![chord];
                let mut after = None;
                while let Some(Some(Ok(event))) = ready_event(&mut events).await {
                    tracing::trace!(?event, "terminal event");
                    match event {
                        Event::Key(key) => match translate_key(key) {
                            Some(c) if is_paste_key(c) => run.push(c),
                            Some(c) => {
                                after = Some(Msg::Key(c));
                                break;
                            }
                            None => {}
                        },
                        other => {
                            after = translate_event(other);
                            break;
                        }
                    }
                }
                ordered.extend(coalesce_burst(run));
                ordered.extend(after);
            }
            other => ordered.extend(other),
        }

        // Apply everything that is already waiting before drawing, so a burst of input
        // costs one frame.
        loop {
            while let Some(msg) = ordered.pop_front() {
                dispatch(
                    state,
                    reloader,
                    &side_tx,
                    &mut runner,
                    &mut foreground,
                    terminal,
                    msg,
                );
            }
            match rx.try_recv() {
                Ok(msg) => ordered.push_back(msg),
                Err(_) => break,
            }
        }
    };

    // Also on error exits: the last visits and bookmarks are still inside the save debounce,
    // and jobs and watchers must be cancelled.
    // The last visits are still inside the save debounce: write them now.
    runner.run(ks_core::pending_writes(state));
    runner.shutdown();
    let finish = tokio::task::spawn_blocking(move || {
        runner.finish_state_writes(STATE_FLUSH_TIMEOUT);
    });
    let _ = finish.await;
    outcome
}

/// The next item of `events` if one is ready right now, without waiting.
///
/// Polls with this task's waker. crossterm's `EventStream` hands the waker of a pending poll
/// to its wake-up thread and ignores later wakers until that thread fires, so polling it with
/// a no-op waker (`now_or_never`) would leave the next key presses unnoticed until something
/// else woke the loop.
async fn ready_event<S: Stream + Unpin>(events: &mut S) -> Option<Option<S::Item>> {
    std::future::poll_fn(|cx| {
        Poll::Ready(match events.poll_next_unpin(cx) {
            Poll::Ready(item) => Some(item),
            Poll::Pending => None,
        })
    })
    .await
}

/// Runs a program attached to the terminal: the TUI is suspended, the program runs, the TUI
/// comes back with a full redraw, and the result is the `Msg::ForegroundDone` to apply next.
fn run_foreground<B: Backend>(
    state: &mut AppState,
    foreground: &mut Option<Box<dyn Foreground>>,
    terminal: &mut Terminal<B>,
    id: u64,
    spec: &ks_core::fileops::ForegroundSpec,
) -> Vec<Msg> {
    let guard = ForegroundGuard::enter();
    let outcome = match foreground.as_mut() {
        Some(host) => host.run(spec),
        None => Err("no terminal available to run programs in the foreground".to_owned()),
    };
    drop(guard);
    let error = match outcome {
        Ok(Some(0) | None) => None,
        Ok(Some(code)) => Some(format!("{} exited with code {code}", spec.program)),
        Err(error) => Some(error),
    };
    // The screen was left behind; draw everything again.
    if let Err(error) = terminal.clear() {
        tracing::warn!(%error, "cannot clear the terminal after a foreground program");
    }
    state.dirty = true;
    // The window may have been resized while the program owned the console; those events
    // were drained unseen. Re-sync before anything reacts to the program's result.
    let mut msgs = Vec::new();
    match terminal.size() {
        Ok(size) => {
            msgs.extend(resize_after_foreground(
                state.size,
                (size.width, size.height),
            ));
        }
        Err(error) => {
            tracing::warn!(%error, "cannot read the terminal size after a foreground program")
        }
    }
    msgs.push(Msg::ForegroundDone { id, error });
    msgs
}

/// The `Msg::Resize` owed when the terminal is `now` (columns, rows) but the state still has
/// `known`.
fn resize_after_foreground(known: ks_core::Size, now: (u16, u16)) -> Option<Msg> {
    let size = ks_core::Size::new(now.0, now.1);
    (size != known && size.width > 0 && size.height > 0).then_some(Msg::Resize(size))
}

/// Forwards process-level interrupts (ctrl-c outside raw mode, console control events) as a
/// message. In raw mode ctrl-c is an ordinary key event and is handled by the keymap.
///
/// Interrupts that arrive while a foreground program owns the console are meant for that
/// program (the console is in cooked mode then, so the control event reaches us too) and are
/// ignored.
fn spawn_interrupt_listener(tx: mpsc::Sender<Msg>) {
    tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            if FOREGROUND_ACTIVE.load(Ordering::SeqCst) {
                continue;
            }
            let _ = tx.send(Msg::Interrupt).await;
            break;
        }
    });
}

/// True while a foreground program runs; see [`spawn_interrupt_listener`].
static FOREGROUND_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Marks a foreground run for as long as it lives, also when the host panics.
struct ForegroundGuard;

impl ForegroundGuard {
    fn enter() -> Self {
        FOREGROUND_ACTIVE.store(true, Ordering::SeqCst);
        Self
    }
}

impl Drop for ForegroundGuard {
    fn drop(&mut self) {
        FOREGROUND_ACTIVE.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use futures_util::stream;
    use ks_core::{Config, JobId, Keymap, Level, Places, Size, StartParams};
    use ks_ui::ColorDepth;
    use ratatui::backend::TestBackend;

    use super::*;

    fn key(c: char, kind: KeyEventKind) -> std::io::Result<Event> {
        Ok(Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
            kind,
            state: KeyEventState::NONE,
        }))
    }

    fn state() -> AppState {
        AppState::new(StartParams {
            cwd: PathBuf::from("/home/me/code"),
            home: Some(PathBuf::from("/home/me")),
            places: Places::default(),
            config: Config::default(),
            keymap: Keymap::defaults(),
            size: Size::new(80, 24),
            now: SystemTime::UNIX_EPOCH,
            utc_offset_secs: 0,
        })
    }

    fn options() -> LoopOptions {
        LoopOptions {
            startup: Vec::new(),
            panic_after_first_frame: false,
            foreground: None,
            claude: ClaudeOptions::default(),
        }
    }

    /// Runs the loop on scripted events that never end (so only `quit` can stop it).
    async fn drive(
        events: Vec<std::io::Result<Event>>,
        state: &mut AppState,
        options: LoopOptions,
    ) -> (Result<QuitKind>, Terminal<TestBackend>) {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
        let stream = stream::iter(events).chain(stream::pending());
        let result = run(&mut terminal, stream, state, &theme, options).await;
        (result, terminal)
    }

    fn screen(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Keeps only the waker of the first pending poll, like crossterm's `EventStream`.
    #[derive(Default)]
    struct FirstWakerStream {
        items: std::collections::VecDeque<u32>,
        waker: Option<std::task::Waker>,
    }

    impl Stream for FirstWakerStream {
        type Item = u32;
        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> Poll<Option<u32>> {
            if let Some(item) = self.items.pop_front() {
                self.waker = None;
                return Poll::Ready(Some(item));
            }
            if self.waker.is_none() {
                self.waker = Some(cx.waker().clone());
            }
            Poll::Pending
        }
    }

    #[tokio::test]
    async fn checking_for_ready_events_keeps_the_loop_wakeable() {
        let shared = Arc::new(std::sync::Mutex::new(FirstWakerStream::default()));
        struct Handle(Arc<std::sync::Mutex<FirstWakerStream>>);
        impl Stream for Handle {
            type Item = u32;
            fn poll_next(
                self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
            ) -> Poll<Option<u32>> {
                std::pin::Pin::new(&mut *self.0.lock().unwrap()).poll_next(cx)
            }
        }
        let mut events = Handle(Arc::clone(&shared));
        assert_eq!(ready_event(&mut events).await, None);
        let feeder = Arc::clone(&shared);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let mut s = feeder.lock().unwrap();
            s.items.push_back(7);
            if let Some(w) = s.waker.take() {
                w.wake();
            }
        });
        let start = std::time::Instant::now();
        let next = tokio::time::timeout(Duration::from_secs(5), events.next()).await;
        assert_eq!(next.ok(), Some(Some(7)));
        // A lost wake-up still ends at the timeout, which re-polls the stream.
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "the key press must wake the loop at once"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn q_quits_and_the_first_frame_was_drawn() {
        let mut state = state();
        let (result, terminal) =
            drive(vec![key('q', KeyEventKind::Press)], &mut state, options()).await;
        assert_eq!(result.unwrap(), QuitKind::Exit);
        assert!(screen(&terminal).contains("~/code"));
    }

    #[tokio::test(start_paused = true)]
    async fn the_last_changes_are_drawn_before_quitting() {
        let mut state = state();
        // Both keys arrive in one batch, so the quit lands inside the frame throttle.
        let events = vec![key('.', KeyEventKind::Press), key('q', KeyEventKind::Press)];
        let (result, terminal) = drive(events, &mut state, options()).await;
        assert_eq!(result.unwrap(), QuitKind::Exit);
        assert!(!state.dirty, "the final state was drawn");
        assert!(screen(&terminal).contains("~/code"));
    }

    #[tokio::test(start_paused = true)]
    async fn capital_q_asks_for_cd() {
        let mut state = state();
        let (result, _) = drive(vec![key('Q', KeyEventKind::Press)], &mut state, options()).await;
        assert_eq!(result.unwrap(), QuitKind::ChangeDir);
    }

    #[tokio::test(start_paused = true)]
    async fn key_releases_do_not_double_fire() {
        let mut state = state();
        // `.` (press + release) then `q`. The release must be ignored, so the hidden-files
        // toggle flips exactly once and the loop still quits on `q`.
        let before = state.view.show_hidden;
        let events = vec![
            key('.', KeyEventKind::Press),
            key('.', KeyEventKind::Release),
            key('q', KeyEventKind::Press),
            key('q', KeyEventKind::Release),
        ];
        let (result, _) = drive(events, &mut state, options()).await;
        assert_eq!(result.unwrap(), QuitKind::Exit);
        assert_ne!(state.view.show_hidden, before);
    }

    #[tokio::test(start_paused = true)]
    async fn resize_events_update_the_state() {
        let mut state = state();
        let events = vec![Ok(Event::Resize(100, 30)), key('q', KeyEventKind::Press)];
        let (result, _) = drive(events, &mut state, options()).await;
        assert!(result.is_ok());
        assert_eq!(state.size, Size::new(100, 30));
    }

    #[tokio::test(start_paused = true)]
    async fn startup_notices_reach_the_status_line() {
        let mut state = state();
        let options = LoopOptions {
            startup: vec![Msg::Notify {
                level: Level::Error,
                text: "config.toml: bad".into(),
            }],
            panic_after_first_frame: false,
            foreground: None,
            claude: ClaudeOptions::default(),
        };
        let (result, terminal) =
            drive(vec![key('q', KeyEventKind::Press)], &mut state, options).await;
        assert!(result.is_ok());
        assert!(screen(&terminal).contains("config.toml: bad"));
    }

    #[tokio::test(start_paused = true)]
    async fn status_toast_expires_via_the_effect_runner() {
        let mut state = state();
        let options = LoopOptions {
            startup: vec![Msg::Notify {
                level: Level::Info,
                text: "hello".into(),
            }],
            panic_after_first_frame: false,
            foreground: None,
            claude: ClaudeOptions::default(),
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
        // After 4 virtual seconds a `q` arrives; by then the 3 s toast must have expired.
        let events = stream::once(async {
            sleep(Duration::from_secs(4)).await;
            key('q', KeyEventKind::Press)
        })
        .chain(stream::pending());
        let result = run(&mut terminal, events, &mut state, &theme, options).await;
        assert_eq!(result.unwrap(), QuitKind::Exit);
        assert!(state.status.is_none(), "toast should have expired");
    }

    #[tokio::test(start_paused = true)]
    async fn spinner_ticks_only_while_jobs_run() {
        let mut state = state();
        state.jobs.insert(JobId(1));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
        let events = stream::once(async {
            sleep(Duration::from_millis(550)).await;
            key('q', KeyEventKind::Press)
        })
        // Quitting with a running job asks first; accept.
        .chain(stream::once(async {
            Ok(Event::Key(KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }))
        }))
        .chain(stream::pending());
        run(&mut terminal, events, &mut state, &theme, options())
            .await
            .unwrap();
        // 10 Hz for 550 ms: roughly five or six frames of animation.
        assert!(
            (4..=7).contains(&state.spinner_frame),
            "{}",
            state.spinner_frame
        );

        let mut idle = self::state();
        let events = stream::once(async {
            sleep(Duration::from_millis(550)).await;
            key('q', KeyEventKind::Press)
        })
        .chain(stream::pending());
        run(&mut terminal, events, &mut idle, &theme, options())
            .await
            .unwrap();
        assert_eq!(idle.spinner_frame, 0, "no ticks without running jobs");
    }

    /// Records what it was asked to run and answers with a canned outcome.
    struct FakeHost {
        runs: std::sync::Arc<std::sync::Mutex<Vec<ks_core::fileops::ForegroundSpec>>>,
        outcome: Result<Option<i32>, String>,
    }

    impl Foreground for FakeHost {
        fn run(&mut self, spec: &ks_core::fileops::ForegroundSpec) -> Result<Option<i32>, String> {
            self.runs.lock().unwrap().push(spec.clone());
            self.outcome.clone()
        }
    }

    /// `e` on a file with a terminal editor: the loop suspends (host), runs the editor on
    /// the file, redraws, and reports a failing exit code.
    async fn edit_with_host(outcome: Result<Option<i32>, String>) -> (AppState, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state();
        state.cwd = dir.path().to_owned();
        state.config.editor.open = "notepad".into();
        state.config.editor.terminal = ks_core::config::EditorTerminal::Always;
        let rows = state.viewport_height();
        state
            .view
            .push_chunk(vec![ks_core::Entry::file("a.txt", 1)], rows);
        state.view.finish_loading();

        let runs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let host = FakeHost {
            runs: runs.clone(),
            outcome,
        };
        let waiting = runs.clone();
        let events = stream::iter([key('e', KeyEventKind::Press)])
            .chain(stream::once(async move {
                for _ in 0..100 {
                    if !waiting.lock().unwrap().is_empty() {
                        break;
                    }
                    sleep(Duration::from_millis(50)).await;
                }
                // Give the message that follows the foreground run time to be processed.
                sleep(Duration::from_millis(200)).await;
                key('q', KeyEventKind::Press)
            }))
            .chain(stream::pending());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
        let options = LoopOptions {
            startup: Vec::new(),
            panic_after_first_frame: false,
            foreground: Some(Box::new(host)),
            claude: ClaudeOptions::default(),
        };
        let result = run(&mut terminal, events, &mut state, &theme, options).await;
        assert_eq!(result.unwrap(), QuitKind::Exit);
        let programs = runs
            .lock()
            .unwrap()
            .iter()
            .map(|spec| format!("{} {}", spec.program, spec.args.join(" ")))
            .collect();
        drop(dir);
        (state, programs)
    }

    #[test]
    fn a_resize_during_a_foreground_program_is_resynced() {
        let known = Size::new(80, 24);
        assert_eq!(resize_after_foreground(known, (80, 24)), None);
        assert_eq!(
            resize_after_foreground(known, (120, 40)),
            Some(Msg::Resize(Size::new(120, 40)))
        );
        assert_eq!(resize_after_foreground(known, (0, 0)), None);
    }

    #[tokio::test(start_paused = true)]
    async fn the_state_size_follows_the_terminal_after_a_foreground_program() {
        let mut state = state();
        state.size = Size::new(80, 24);
        // The window was resized while the editor ran: the backend reports the new size.
        let mut terminal = Terminal::new(TestBackend::new(132, 43)).unwrap();
        let mut runner = crate::effects::EffectRunner::new(tokio::sync::mpsc::channel(8).0);
        let mut host: Option<Box<dyn Foreground>> = Some(Box::new(FakeHost {
            runs: Default::default(),
            outcome: Ok(Some(0)),
        }));
        let request = ks_core::fileops::ForegroundRequest {
            spec: ks_core::fileops::ForegroundSpec {
                program: "vim".into(),
                args: vec![],
            },
            purpose: ks_core::fileops::ForegroundPurpose::Editor,
        };
        let (side, _side_rx) = mpsc::unbounded_channel::<Side>();
        dispatch(
            &mut state,
            None,
            &side,
            &mut runner,
            &mut host,
            &mut terminal,
            Msg::Foreground(request),
        );
        assert_eq!(state.size, Size::new(132, 43));
    }

    #[tokio::test]
    async fn terminal_editor_runs_through_the_foreground_host() {
        let (state, runs) = edit_with_host(Ok(Some(0))).await;
        assert_eq!(runs.len(), 1, "{runs:?}");
        assert!(runs[0].contains("a.txt"), "{runs:?}");
        assert!(state.status.is_none(), "{:?}", state.status);
    }

    #[tokio::test]
    async fn foreground_failures_reach_the_status_line() {
        let (state, runs) = edit_with_host(Ok(Some(3))).await;
        assert_eq!(runs.len(), 1);
        let text = state.status.unwrap().text;
        assert!(
            text.contains("Editor failed") && text.contains("code 3"),
            "{text}"
        );

        let (state, _) = edit_with_host(Err("cannot run vim: not found".into())).await;
        assert!(state.status.unwrap().text.contains("not found"));
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_host_foreground_programs_are_refused() {
        let mut state = state();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut runner = crate::effects::EffectRunner::new(tokio::sync::mpsc::channel(8).0);
        let mut host: Option<Box<dyn Foreground>> = None;
        let request = ks_core::fileops::ForegroundRequest {
            spec: ks_core::fileops::ForegroundSpec {
                program: "vim".into(),
                args: vec![],
            },
            purpose: ks_core::fileops::ForegroundPurpose::Editor,
        };
        let (side, _side_rx) = mpsc::unbounded_channel::<Side>();
        dispatch(
            &mut state,
            None,
            &side,
            &mut runner,
            &mut host,
            &mut terminal,
            Msg::Foreground(request),
        );
        assert!(state.status.unwrap().text.contains("no terminal available"));
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "--debug-panic")]
    async fn debug_panic_fires_after_the_first_frame() {
        let mut state = state();
        let options = LoopOptions {
            startup: Vec::new(),
            panic_after_first_frame: true,
            foreground: None,
            claude: ClaudeOptions::default(),
        };
        let _ = drive(vec![], &mut state, options).await;
    }

    #[tokio::test(start_paused = true)]
    async fn input_stream_errors_surface() {
        let mut state = state();
        let (result, _) = drive(
            vec![Err(std::io::Error::other("console gone"))],
            &mut state,
            options(),
        )
        .await;
        let message = format!("{:#}", result.unwrap_err());
        assert!(message.contains("console gone"), "{message}");
    }

    fn special(code: KeyCode) -> std::io::Result<Event> {
        Ok(Event::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }))
    }

    fn typed(text: &str) -> Vec<std::io::Result<Event>> {
        text.chars().map(|c| key(c, KeyEventKind::Press)).collect()
    }

    /// A reloader that, after a theme job, sends `Interrupt` through the same FIFO
    /// channel as the job's results: the loop always applies the results before it quits.
    fn quitting_reloader(paths: crate::paths::AppPaths) -> Reloader {
        let mut reloader = Reloader::new(paths, ColorDepth::TrueColor);
        // Every scenario ends in a theme job (a reload that changes the theme starts one).
        reloader.after_job = Some(Arc::new(
            |kind: crate::reload::JobKind, side: &mpsc::UnboundedSender<Side>| {
                if kind == crate::reload::JobKind::Theme {
                    let _ = side.send(Side::Msg(Msg::Interrupt));
                }
            },
        ));
        reloader
    }

    /// Start-up messages that stand in for the (asynchronous) start-up theme listing.
    fn themes_startup(names: &[&str]) -> LoopOptions {
        LoopOptions {
            startup: vec![Msg::ThemesAvailable(
                names.iter().map(|n| (*n).to_owned()).collect(),
            )],
            ..options()
        }
    }

    /// A `TestBackend` that publishes a copy of every drawn frame, so a test can wait for
    /// a condition on screen instead of sleeping.
    struct Probe {
        inner: TestBackend,
        frame: Frames,
    }

    impl Backend for Probe {
        type Error = <TestBackend as Backend>::Error;

        fn draw<'a, I>(&mut self, content: I) -> std::result::Result<(), Self::Error>
        where
            I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
        {
            self.inner.draw(content)
        }
        fn hide_cursor(&mut self) -> std::result::Result<(), Self::Error> {
            self.inner.hide_cursor()
        }
        fn show_cursor(&mut self) -> std::result::Result<(), Self::Error> {
            self.inner.show_cursor()
        }
        fn get_cursor_position(
            &mut self,
        ) -> std::result::Result<ratatui::layout::Position, Self::Error> {
            self.inner.get_cursor_position()
        }
        fn set_cursor_position<P: Into<ratatui::layout::Position>>(
            &mut self,
            position: P,
        ) -> std::result::Result<(), Self::Error> {
            self.inner.set_cursor_position(position)
        }
        fn clear(&mut self) -> std::result::Result<(), Self::Error> {
            self.inner.clear()
        }
        fn clear_region(
            &mut self,
            clear_type: ratatui::backend::ClearType,
        ) -> std::result::Result<(), Self::Error> {
            self.inner.clear_region(clear_type)
        }
        fn size(&self) -> std::result::Result<ratatui::layout::Size, Self::Error> {
            self.inner.size()
        }
        fn window_size(
            &mut self,
        ) -> std::result::Result<ratatui::backend::WindowSize, Self::Error> {
            self.inner.window_size()
        }
        fn flush(&mut self) -> std::result::Result<(), Self::Error> {
            // `flush` ends every draw: publish the frame as it now stands.
            *self.frame.lock().unwrap() = Some(self.inner.buffer().clone());
            self.inner.flush()
        }
    }

    type Frames = Arc<std::sync::Mutex<Option<ratatui::buffer::Buffer>>>;

    /// Resolves once the latest drawn frame satisfies `ready`.
    async fn wait_for_frame(frames: &Frames, ready: impl Fn(&ratatui::buffer::Buffer) -> bool) {
        loop {
            if frames.lock().unwrap().as_ref().is_some_and(&ready) {
                return;
            }
            sleep(Duration::from_millis(5)).await;
        }
    }

    fn screen_has(buffer: &ratatui::buffer::Buffer, needle: &str) -> bool {
        (0..buffer.area.height).any(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .contains(needle)
        })
    }

    fn header_is_paper(buffer: &ratatui::buffer::Buffer) -> bool {
        let paper = Theme::builtin("paper", ColorDepth::TrueColor).unwrap();
        (0..buffer.area.width).any(|x| buffer[(x, 0)].fg == paper.fg_dim)
    }

    /// Like `drive`, but with a reloader over a temporary config directory. Keys are sent
    /// in order; `gates` holds `(index, text)`: before the key at `index` is sent, the
    /// screen must show `text` (waiting on frames, never on the clock). The loop quits
    /// once the screen satisfies `done`.
    async fn drive_with_config(
        config_toml: Option<&str>,
        events: Vec<std::io::Result<Event>>,
        gates: &[(usize, &'static str)],
        done: fn(&ratatui::buffer::Buffer) -> bool,
        state: &mut AppState,
    ) -> (Result<QuitKind>, Terminal<Probe>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::AppPaths::under(dir.path());
        if let Some(text) = config_toml {
            std::fs::create_dir_all(paths.config_dir()).unwrap();
            std::fs::write(paths.config_file(), text).unwrap();
        }
        let frames: Frames = Arc::default();
        let probe = Probe {
            inner: TestBackend::new(80, 24),
            frame: Arc::clone(&frames),
        };
        let mut terminal = Terminal::new(probe).unwrap();
        let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
        let gates: Vec<(usize, &'static str)> = gates.to_vec();
        let keys = stream::iter(events.into_iter().enumerate()).then({
            let frames = Arc::clone(&frames);
            move |(i, event)| {
                let frames = Arc::clone(&frames);
                let gate = gates.iter().find(|(at, _)| *at == i).map(|(_, t)| *t);
                async move {
                    match gate {
                        Some(text) => wait_for_frame(&frames, |b| screen_has(b, text)).await,
                        // The first key also waits for the first frame, i.e. for start-up.
                        None if i == 0 => wait_for_frame(&frames, |_| true).await,
                        None => {}
                    }
                    event
                }
            }
        });
        let quit = stream::once({
            let frames = Arc::clone(&frames);
            async move {
                wait_for_frame(&frames, done).await;
                Ok(Event::Key(KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers: KeyModifiers::CONTROL,
                    kind: KeyEventKind::Press,
                    state: KeyEventState::NONE,
                }))
            }
        });
        let stream = keys.chain(quit).chain(stream::pending());
        let result = run_with(
            &mut terminal,
            stream,
            state,
            &theme,
            themes_startup(&["ember", "paper"]),
            Some(quitting_reloader(paths)),
        );
        let result = tokio::time::timeout(Duration::from_secs(60), result)
            .await
            .expect("loop did not quit (no theme job ran after the reload?)");
        (result, terminal, dir)
    }

    // Waits on drawn frames, not the clock: config and theme reads run on blocking threads.
    #[tokio::test]
    async fn palette_reload_config_applies_keymap_icons_and_theme() {
        let mut state = state();
        let mut events = typed(":reload config");
        events.push(special(KeyCode::Enter));
        let config = "theme = \"paper\"
icons = \"ascii\"
[keymap.normal]
\"q\" = \"none\"
\"T\" = \"file.trash\"
";
        let (result, terminal, _dir) =
            drive_with_config(Some(config), events, &[], header_is_paper, &mut state).await;
        assert_eq!(result.unwrap(), QuitKind::Exit);
        assert_eq!(state.config.theme, "paper");
        assert_eq!(state.config.icons, ks_core::config::IconSet::Ascii);
        assert_eq!(state.themes, ["ember", "paper"]);
        assert!(state
            .keymap
            .keys_for(ks_core::KeyContext::Normal, ks_core::Action::Trash)
            .contains(&"T".to_owned()));
        assert!(state
            .keymap
            .keys_for(ks_core::KeyContext::Normal, ks_core::Action::Quit)
            .iter()
            .all(|k| k != "q"));
        // The frame was redrawn with the paper theme (light text colour differs from ember).
        assert!(header_is_paper(terminal.backend().inner.buffer()));
    }

    // Waits on drawn frames, not the clock: config and theme reads run on blocking threads.
    #[tokio::test]
    async fn palette_switch_theme_changes_the_drawn_colours() {
        let mut state = state();
        let mut events = typed(":switch theme");
        events.push(special(KeyCode::Enter));
        // The theme picker lists "paper" only once the start-up theme listing arrived.
        let down = events.len();
        events.push(special(KeyCode::Down));
        events.push(special(KeyCode::Enter));
        let (result, terminal, _dir) = drive_with_config(
            None,
            events,
            &[(down, "paper")],
            header_is_paper,
            &mut state,
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(state.config.theme, "paper");
        assert!(header_is_paper(terminal.backend().inner.buffer()));
    }

    // Real time: config and theme reads run on blocking threads.
    #[tokio::test]
    async fn a_broken_user_theme_is_reported_and_the_old_theme_kept() {
        let mut state = state();
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::AppPaths::under(dir.path());
        std::fs::create_dir_all(paths.themes_dir()).unwrap();
        std::fs::write(
            paths.themes_dir().join("broken.toml"),
            "colors = 5
",
        )
        .unwrap();
        let mut events = typed(":switch theme");
        events.push(special(KeyCode::Enter));
        events.extend(typed("broken"));
        events.push(special(KeyCode::Enter));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
        let stream = stream::iter(events).chain(stream::pending());
        let reloader = quitting_reloader(paths);
        let result = run_with(
            &mut terminal,
            stream,
            &mut state,
            &theme,
            themes_startup(&["ember", "broken"]),
            Some(reloader),
        );
        tokio::time::timeout(Duration::from_secs(20), result)
            .await
            .expect("loop did not quit (no theme job ran after the reload?)")
            .unwrap();
        assert_eq!(state.config.theme, "ember", "theme unchanged");
        let status = state.status.as_ref().unwrap();
        assert_eq!(status.level, Level::Error);
        assert!(status.text.contains("broken"), "{}", status.text);
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_reloader_switching_reports_instead_of_failing() {
        let mut state = state();
        let mut events = typed(":reload config");
        events.push(special(KeyCode::Enter));
        events.push(key('q', KeyEventKind::Press));
        let (result, _) = drive(events, &mut state, options()).await;
        assert!(result.is_ok());
        assert!(state
            .status
            .as_ref()
            .unwrap()
            .text
            .contains("not available here"));
    }

    fn ctrl(c: char) -> std::io::Result<Event> {
        Ok(Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }))
    }

    #[tokio::test(start_paused = true)]
    async fn palette_ctrl_n_ctrl_p_move_selection() {
        let mut state = state();
        let mut events = vec![key(':', KeyEventKind::Press)];
        events.extend([ctrl('n'), ctrl('n'), ctrl('n'), ctrl('p')]);
        events.push(special(KeyCode::Esc));
        events.push(key('q', KeyEventKind::Press));
        let (result, _) = drive(events, &mut state, options()).await;
        assert!(result.is_ok());
        assert_eq!(state.palette.selected, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_which_key_popup_appears_after_the_delay_through_the_loop() {
        let mut state = state();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::builtin("ember", ColorDepth::TrueColor).unwrap();
        let stream = stream::iter(vec![key('g', KeyEventKind::Press)]).chain(stream::pending());
        // The loop never quits on its own; cut it off once the timer should have fired.
        let early = tokio::time::timeout(
            Duration::from_millis(100),
            run(&mut terminal, stream, &mut state, &theme, options()),
        )
        .await;
        assert!(early.is_err(), "loop keeps running");
        assert!(!state.which_key_visible, "not before the delay");
        assert!(!screen(&terminal).contains("Go to home"));

        let stream = stream::iter(vec![key('g', KeyEventKind::Press)]).chain(stream::pending());
        let mut state2 = self::state();
        let late = tokio::time::timeout(
            Duration::from_millis(600),
            run(&mut terminal, stream, &mut state2, &theme, options()),
        )
        .await;
        assert!(late.is_err());
        assert!(state2.which_key_visible);
        assert!(
            screen(&terminal).contains("Go to home"),
            "{}",
            screen(&terminal)
        );
    }
}

//! Raw mode, the alternate screen and the panic hook that undoes them.
//!
//! Mouse capture is intentionally never enabled.

use std::io::{self, Stdout};
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crossterm::cursor::Show;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// True while the terminal is in TUI mode; makes [`restore`] idempotent so the panic hook
/// and `Drop` can both call it.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// True while bracketed paste is enabled (only while the Claude pane has focus, so other
/// text inputs keep receiving pasted text as ordinary key events).
static BRACKETED_PASTE: AtomicBool = AtomicBool::new(false);

/// The window title before Kestrel changed it (`None` when it cannot be read).
static ORIGINAL_TITLE: OnceLock<Option<String>> = OnceLock::new();

/// The title Kestrel last set; re-sent after a foreground program had the terminal.
static CURRENT_TITLE: Mutex<String> = Mutex::new(String::new());

/// Sets the terminal window (tab) title when it changed. A no-op outside TUI mode.
pub fn set_title(title: &str) {
    if !ACTIVE.load(Ordering::SeqCst) {
        return;
    }
    let mut current = CURRENT_TITLE.lock().unwrap_or_else(|e| e.into_inner());
    if *current == title {
        return;
    }
    ORIGINAL_TITLE.get_or_init(ks_fs::console_title);
    if execute!(io::stdout(), SetTitle(title)).is_ok() {
        title.clone_into(&mut current);
    }
}

/// Re-sends the title Kestrel set (a foreground program may have replaced it).
fn resend_title() {
    let current = CURRENT_TITLE.lock().unwrap_or_else(|e| e.into_inner());
    if !current.is_empty() {
        let _ = execute!(io::stdout(), SetTitle(current.as_str()));
    }
}

/// Puts the shell's title back, if Kestrel changed it and knows what it was.
fn restore_title() {
    let mut current = CURRENT_TITLE.lock().unwrap_or_else(|e| e.into_inner());
    if current.is_empty() {
        return;
    }
    current.clear();
    if let Some(Some(original)) = ORIGINAL_TITLE.get() {
        let _ = execute!(io::stdout(), SetTitle(original.as_str()));
    }
}

/// Turns bracketed paste on or off. A no-op outside TUI mode (tests, `ks hook`).
///
/// Not enabled on Windows: crossterm's console reader never parses the paste markers, so they
/// would arrive as raw key events (`Esc [ 2 0 0 ~`). There a paste is detected as a burst of
/// keys instead (`input::coalesce_burst`).
pub fn set_bracketed_paste(on: bool) {
    if cfg!(windows)
        || !ACTIVE.load(Ordering::SeqCst)
        || BRACKETED_PASTE.swap(on, Ordering::SeqCst) == on
    {
        return;
    }
    let _ = if on {
        execute!(io::stdout(), EnableBracketedPaste)
    } else {
        execute!(io::stdout(), DisableBracketedPaste)
    };
}

/// Owns the terminal for the lifetime of the TUI. Dropping it restores the terminal, so
/// every exit path (return, `?`, unwinding) leaves the shell usable.
pub struct TerminalSession {
    terminal: Tui,
}

impl TerminalSession {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        ACTIVE.store(true, Ordering::SeqCst);
        let built = execute!(io::stdout(), EnterAlternateScreen)
            .and_then(|()| Terminal::new(CrosstermBackend::new(io::stdout())));
        match built {
            Ok(terminal) => Ok(Self { terminal }),
            Err(e) => {
                restore();
                Err(e)
            }
        }
    }

    pub fn terminal(&mut self) -> &mut Tui {
        &mut self.terminal
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        restore();
    }
}

/// Leaves the alternate screen, shows the cursor and disables raw mode. Safe to call more
/// than once and from a panic hook; failures are ignored because there is nothing left to
/// do about them.
pub fn restore() {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        if BRACKETED_PASTE.swap(false, Ordering::SeqCst) {
            let _ = execute!(io::stdout(), DisableBracketedPaste);
        }
        let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
        restore_title();
        let _ = disable_raw_mode();
    }
}

/// Hands the terminal to a foreground program: leaves the alternate screen, shows the cursor
/// and disables raw mode, but keeps the panic hook armed (`ACTIVE` stays set).
pub fn suspend() {
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
}

/// Takes the terminal back after [`suspend`]. The caller must redraw everything.
pub fn resume() -> io::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    resend_title();
    Ok(())
}

/// Installs a panic hook that first runs `cleanup` (restoring the terminal), then logs the
/// panic with a backtrace, then, for UI-thread panics only, chains to the previous hook,
/// which prints it to stderr.
///
/// `cleanup` runs only for panics on the calling (UI) thread: a panic in a background task
/// is contained by tokio and the app keeps running, so it must not tear the terminal down.
pub fn install_panic_hook(cleanup: impl Fn() + Send + Sync + 'static) {
    let ui_thread = std::thread::current().id();
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let on_ui = std::thread::current().id() == ui_thread;
        if on_ui {
            cleanup();
        }
        tracing::error!(
            panic = %info,
            backtrace = %std::backtrace::Backtrace::force_capture(),
            "panic"
        );
        // The default hook writes to stderr, which is the live screen while the TUI runs: only
        // the UI thread's panic (after the terminal was restored above) may print. A worker
        // panic is contained and only goes to the log file.
        if on_ui {
            previous(info);
        }
    }));
}

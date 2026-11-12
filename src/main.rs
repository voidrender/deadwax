use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use senritsu::app::App;
use senritsu::player::mpv::{MpvBackend, mpv_available};
use senritsu::player::{UrlFor, spawn_engine};
use senritsu::runtime::{self, Ctx};
use senritsu::{logging, paths};
use std::io::{Stdout, stdout};
use std::process::ExitCode;
use std::sync::Arc;

const MPV_MISSING: &str =
    "Senritsu needs mpv. Install it with: brew install mpv (macOS) or sudo pacman -S mpv (Arch)";

type Term = Terminal<CrosstermBackend<Stdout>>;

#[tokio::main]
async fn main() -> ExitCode {
    let _guard = logging::init(&paths::cache_dir());

    if !mpv_available() {
        eprintln!("{MPV_MISSING}");
        return ExitCode::FAILURE;
    }

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("fatal: {e:#}");
            eprintln!("senritsu: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let (backend, backend_events) = MpvBackend::spawn(100).await?;
    // Placeholder until the Subsonic client is wired in at login.
    let url_for: UrlFor = Arc::new(|_| String::new());
    let engine = spawn_engine(backend, backend_events, url_for);
    let ctx = Ctx::new(None, engine.commands, engine.snapshot, engine.events);

    install_panic_hook();
    let mut terminal = enter_terminal()?;
    let result = runtime::run(&mut terminal, App::new(), ctx).await;
    restore_terminal();
    result
}

/// Restores the terminal before the default hook prints the panic, so the
/// message lands on a usable screen. It also flags the runtime, which then
/// shuts down even when the panic was in a spawned task.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        runtime::note_panic();
        previous(info);
    }));
}

fn enter_terminal() -> anyhow::Result<Term> {
    enable_raw_mode()?;
    if let Err(e) = execute!(stdout(), EnterAlternateScreen) {
        restore_terminal();
        return Err(e.into());
    }
    match Terminal::new(CrosstermBackend::new(stdout())) {
        Ok(t) => Ok(t),
        Err(e) => {
            restore_terminal();
            Err(e.into())
        }
    }
}

/// Best effort: each step runs even if an earlier one failed.
fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
}

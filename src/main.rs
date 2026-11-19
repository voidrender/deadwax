use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use deadwax::config::Config;
use deadwax::credentials::{CredentialStore, KeyringBackend};
use deadwax::player::mpv::{MpvBackend, mpv_available};
use deadwax::player::{UrlFor, spawn_engine};
use deadwax::runtime::{self, Ctx, SharedClient};
use deadwax::ui::art::{self, ProtocolChoice};
use deadwax::{instance, logging, paths};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{Stdout, stdout};
use std::process::ExitCode;
use std::sync::{Arc, RwLock};

const MPV_MISSING: &str =
    "Deadwax needs mpv. Install it with: brew install mpv (macOS) or sudo pacman -S mpv (Arch)";

type Term = Terminal<CrosstermBackend<Stdout>>;

const USAGE: &str = "\
A terminal music player for Navidrome and other Subsonic servers.

Usage: deadwax [OPTIONS]

Options:
  -h, --help     Print this help
  -V, --version  Print the version

Run without options to start the player. Press ? inside it for keys.";

#[tokio::main]
async fn main() -> ExitCode {
    if let Some(arg) = std::env::args().nth(1) {
        return match arg.as_str() {
            "-V" | "--version" => {
                println!("deadwax {}", env!("CARGO_PKG_VERSION"));
                ExitCode::SUCCESS
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                ExitCode::SUCCESS
            }
            _ => {
                eprintln!("deadwax: unknown option '{arg}'\n\n{USAGE}");
                ExitCode::from(2)
            }
        };
    }

    let _guard = logging::init(&paths::cache_dir());

    // Two instances would overwrite each other's queue and scrobble files.
    let _lock = match instance::lock(&paths::data_dir().join("deadwax.lock")) {
        Ok(Some(file)) => Some(file),
        Ok(None) => {
            eprintln!("Deadwax is already running.");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            tracing::warn!("couldn't take the instance lock: {e}");
            None
        }
    };

    if !mpv_available() {
        eprintln!("{MPV_MISSING}");
        return ExitCode::FAILURE;
    }

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("fatal: {e:#}");
            eprintln!("deadwax: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let (backend, backend_events) = MpvBackend::spawn(100).await?;
    let (mut app, client) = runtime::startup(
        &runtime::config_path(),
        &CredentialStore::new(KeyringBackend, runtime::credentials_path()),
    );
    let shared: SharedClient = Arc::new(RwLock::new(client));
    // Reads the client at call time so a login or logout takes effect
    // without respawning the engine. Empty while logged out.
    let url_for: UrlFor = {
        let shared = shared.clone();
        Arc::new(move |song| {
            runtime::current_client(&shared)
                .map(|c| c.stream_url(&song.id))
                .unwrap_or_default()
        })
    };
    let engine = spawn_engine(backend, backend_events, url_for);
    let ctx = Ctx::new(shared, engine.commands, engine.snapshot, engine.events);

    install_panic_hook();
    let mut terminal = enter_terminal()?;
    // The query reads the terminal's replies from stdin, so it runs in raw
    // mode and before the runtime starts reading events.
    let choice = protocol_choice();
    let picker = tokio::task::spawn_blocking(move || art::make_picker(choice)).await;
    if let Ok(picker) = picker {
        app.art.set_picker(picker);
    }
    // Wipe anything a terminal echoed back from the query.
    let _ = terminal.clear();
    let result = runtime::run(&mut terminal, app, ctx).await;
    restore_terminal();
    result
}

/// `DEADWAX_IMAGE_PROTOCOL`, else `image_protocol` from the config.
fn protocol_choice() -> ProtocolChoice {
    let config = Config::load(&runtime::config_path())
        .ok()
        .flatten()
        .and_then(|c| c.image_protocol);
    let env = std::env::var(art::PROTOCOL_ENV).ok();
    art::parse_protocol(art::protocol_setting(env, config).as_deref())
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
    if let Err(e) = execute!(stdout(), EnterAlternateScreen, EnableMouseCapture) {
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
    let _ = execute!(
        stdout(),
        DisableMouseCapture,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
}

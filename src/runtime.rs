use crate::app::login::login_error_message;
use crate::app::{ApiResult, App, AppEvent, Effect};
use crate::config::Config;
use crate::credentials::{CredentialStore, KeyringBackend, SecretBackend};
use crate::paths;
use crate::player::{EngineEvent, PlayerCommand, PlayerSnapshot};
use crate::subsonic::{Auth, SubsonicClient};
use crate::ui;
use crossterm::event::{Event, EventStream};
use futures::{Stream, StreamExt};
use ratatui::Terminal;
use ratatui::backend::Backend;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::time::MissedTickBehavior;

/// Set by the panic hook. A panic in a spawned task does not end the
/// process, so the loop checks this and shuts down instead of drawing into a
/// terminal the hook already restored.
static PANICKED: AtomicBool = AtomicBool::new(false);

/// Records that a panic happened. Call it from the panic hook.
pub fn note_panic() {
    PANICKED.store(true, Ordering::SeqCst);
}

#[cfg(test)]
fn clear_panic() {
    PANICKED.store(false, Ordering::SeqCst);
}

fn check_panicked() -> anyhow::Result<()> {
    if PANICKED.load(Ordering::SeqCst) {
        anyhow::bail!("a background task panicked");
    }
    Ok(())
}

const TICK: Duration = Duration::from_millis(250);
/// How long quitting waits for the engine to stop mpv.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// The logged-in client, shared with the engine's `url_for`. `None` while
/// logged out.
pub type SharedClient = Arc<RwLock<Option<SubsonicClient>>>;

/// Reads the shared client. A poisoned lock still holds a usable value.
pub fn current_client(shared: &SharedClient) -> Option<SubsonicClient> {
    shared
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .cloned()
}

fn set_client(shared: &SharedClient, client: Option<SubsonicClient>) {
    *shared.write().unwrap_or_else(|e| e.into_inner()) = client;
}

pub fn config_path() -> PathBuf {
    paths::config_dir().join("config.toml")
}

pub fn credentials_path() -> PathBuf {
    paths::config_dir().join("credentials.toml")
}

fn credential_store(path: &Path) -> CredentialStore<KeyringBackend> {
    CredentialStore::new(KeyringBackend, path.to_path_buf())
}

/// Everything the runtime loop and effect executor need.
pub struct Ctx {
    /// `None` until a server is configured and logged in.
    pub client: SharedClient,
    pub config_path: PathBuf,
    pub credentials_path: PathBuf,
    pub player: mpsc::UnboundedSender<PlayerCommand>,
    /// Spawned work reports back through this sender.
    pub tx: mpsc::UnboundedSender<AppEvent>,
    pub rx: mpsc::UnboundedReceiver<AppEvent>,
    pub snapshot: watch::Receiver<PlayerSnapshot>,
    pub engine_events: mpsc::UnboundedReceiver<EngineEvent>,
}

impl Ctx {
    pub fn new(
        client: SharedClient,
        player: mpsc::UnboundedSender<PlayerCommand>,
        snapshot: watch::Receiver<PlayerSnapshot>,
        engine_events: mpsc::UnboundedReceiver<EngineEvent>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            client,
            config_path: config_path(),
            credentials_path: credentials_path(),
            player,
            tx,
            rx,
            snapshot,
            engine_events,
        }
    }
}

/// Runs the UI until the app quits, then shuts the engine down (also when
/// the loop fails).
pub async fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    app: App,
    mut ctx: Ctx,
) -> anyhow::Result<()>
where
    B::Error: Send + Sync + 'static,
{
    let result = event_loop(terminal, app, &mut ctx, EventStream::new()).await;
    shutdown_engine(&mut ctx).await;
    result
}

/// The loop behind `run`. Terminal input is a parameter so tests can drive
/// it without a tty.
async fn event_loop<B, I>(
    terminal: &mut Terminal<B>,
    mut app: App,
    ctx: &mut Ctx,
    mut input: I,
) -> anyhow::Result<()>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
    I: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut snapshot_open = true;
    let mut engine_open = true;

    check_panicked()?;
    app.player = ctx.snapshot.borrow_and_update().clone();
    terminal.draw(|f| ui::draw(f, &mut app))?;

    while !app.should_quit {
        check_panicked()?;
        let ev = tokio::select! {
            term = input.next() => match term {
                Some(Ok(Event::Key(key))) => AppEvent::Key(key),
                Some(Ok(Event::Resize(..))) => AppEvent::Resize,
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
            _ = tick.tick() => AppEvent::Tick,
            Some(ev) = ctx.rx.recv() => ev,
            changed = ctx.snapshot.changed(), if snapshot_open => match changed {
                Ok(()) => AppEvent::Snapshot(ctx.snapshot.borrow_and_update().clone()),
                Err(_) => {
                    snapshot_open = false;
                    continue;
                }
            },
            ev = ctx.engine_events.recv(), if engine_open => match ev {
                Some(ev) => AppEvent::Engine(ev),
                None => {
                    engine_open = false;
                    continue;
                }
            },
        };
        if let AppEvent::Api(ApiResult::LoggedIn { client, .. }) = &ev {
            set_client(&ctx.client, Some(client.clone()));
        }
        for effect in app.handle_event(ev) {
            execute(effect, &mut app, ctx);
        }
        check_panicked()?;
        terminal.draw(|f| ui::draw(f, &mut app))?;
    }
    Ok(())
}

/// Carries out one effect. Player commands are sent right away; async work
/// must be spawned and report back through `ctx.tx`, never awaited here.
fn execute(effect: Effect, app: &mut App, ctx: &Ctx) {
    match effect {
        Effect::Player(cmd) => {
            if ctx.player.send(cmd).is_err() {
                app.set_status("The player has stopped");
            }
        }
        Effect::Quit => app.should_quit = true,
        Effect::Login {
            server,
            username,
            password,
        } => {
            let tx = ctx.tx.clone();
            let config_path = ctx.config_path.clone();
            let credentials_path = ctx.credentials_path.clone();
            tokio::spawn(async move {
                let result = login(&config_path, &credentials_path, server, username, password)
                    .await
                    .unwrap_or_else(ApiResult::LoginFailed);
                let _ = tx.send(AppEvent::Api(result));
            });
        }
        Effect::Logout => logout(app, ctx),
    }
}

/// Checks the credentials against the server, then stores them. Errors are
/// user-facing messages and never carry the password or a URL.
async fn login(
    config_path: &Path,
    credentials_path: &Path,
    server: String,
    username: String,
    password: String,
) -> Result<ApiResult, String> {
    let client = SubsonicClient::new(&server, &username, Auth::Password(password.clone()))
        .map_err(|e| login_error_message(&e))?;
    client.ping().await.map_err(|e| login_error_message(&e))?;

    let credentials_path = credentials_path.to_path_buf();
    let (save_server, save_user) = (server.clone(), username.clone());
    let saved_to = tokio::task::spawn_blocking(move || {
        credential_store(&credentials_path).save(&save_server, &save_user, &password)
    })
    .await
    .map_err(|_| "Couldn't save the login".to_string())?
    .map_err(|e| {
        tracing::warn!("saving credentials failed: {e:#}");
        "Couldn't save the login".to_string()
    })?;

    save_config(config_path, &server, &username).map_err(|e| {
        tracing::warn!("saving config failed: {e:#}");
        "Couldn't save the config file".to_string()
    })?;
    Ok(ApiResult::LoggedIn { client, saved_to })
}

/// Writes the server and username, keeping any other settings already in the
/// file.
fn save_config(path: &Path, server: &str, username: &str) -> anyhow::Result<()> {
    let existing = Config::load(path).unwrap_or_else(|e| {
        tracing::warn!("ignoring unreadable config: {e:#}");
        None
    });
    let config = match existing {
        Some(c) => Config {
            server_url: server.to_string(),
            username: username.to_string(),
            ..c
        },
        None => Config {
            server_url: server.to_string(),
            username: username.to_string(),
            discord_app_id: None,
            discord_default_image: "senritsu".to_string(),
            image_protocol: None,
        },
    };
    config.save(path)
}

/// Stops playback, forgets the stored login and the client, and returns to
/// the login screen. The server URL stays in the config for next time.
fn logout(app: &mut App, ctx: &Ctx) {
    let _ = ctx.player.send(PlayerCommand::Clear);
    let config = Config::load(&ctx.config_path).ok().flatten();
    let (server, username) = match current_client(&ctx.client) {
        Some(c) => (c.server_url().to_string(), c.username().to_string()),
        None => config
            .as_ref()
            .map(|c| (c.server_url.clone(), c.username.clone()))
            .unwrap_or_default(),
    };
    let mut problem = None;
    if let Err(e) = credential_store(&ctx.credentials_path).clear(&server, &username) {
        tracing::warn!("clearing credentials failed: {e:#}");
        problem = Some("Couldn't remove the saved login");
    }
    set_client(&ctx.client, None);
    if let Some(c) = config
        && let Err(e) = (Config {
            username: String::new(),
            ..c
        })
        .save(&ctx.config_path)
    {
        tracing::warn!("updating config failed: {e:#}");
        problem = Some("Couldn't update the config file");
    }
    app.show_login(&server, "");
    if let Some(msg) = problem {
        app.set_status(msg);
    }
}

/// Picks the first screen. A saved server, username and login that builds a
/// client skips the login form. Anything else opens it, prefilled with what
/// the config remembers.
pub fn startup<S: SecretBackend>(
    config_path: &Path,
    store: &CredentialStore<S>,
) -> (App, Option<SubsonicClient>) {
    let config = Config::load(config_path).unwrap_or_else(|e| {
        tracing::warn!("ignoring unreadable config: {e:#}");
        None
    });
    let Some(config) = config else {
        return (App::login_screen("", ""), None);
    };
    if !config.username.is_empty() {
        let auth = store
            .load(&config.server_url, &config.username)
            .unwrap_or_else(|e| {
                tracing::warn!("loading credentials failed: {e:#}");
                None
            });
        if let Some(auth) = auth
            && let Ok(client) = SubsonicClient::new(&config.server_url, &config.username, auth)
        {
            return (App::logged_in(client.server_url()), Some(client));
        }
    }
    (
        App::login_screen(&config.server_url, &config.username),
        None,
    )
}

/// Asks the engine to stop and waits briefly for it to finish, which drops
/// the mpv backend and kills the process.
async fn shutdown_engine(ctx: &mut Ctx) {
    if ctx.player.send(PlayerCommand::Shutdown).is_err() {
        return;
    }
    let drained = tokio::time::timeout(SHUTDOWN_GRACE, async {
        while ctx.engine_events.recv().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        tracing::warn!("player engine did not stop in time");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::SecretError;
    use ratatui::backend::TestBackend;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MemSecrets(RefCell<HashMap<String, String>>);

    impl SecretBackend for MemSecrets {
        fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
            Ok(self.0.borrow().get(account).cloned())
        }
        fn set(&self, account: &str, secret: &str) -> Result<(), SecretError> {
            self.0.borrow_mut().insert(account.into(), secret.into());
            Ok(())
        }
        fn delete(&self, account: &str) -> Result<(), SecretError> {
            self.0.borrow_mut().remove(account);
            Ok(())
        }
    }

    fn config(server: &str, user: &str) -> Config {
        Config {
            server_url: server.into(),
            username: user.into(),
            discord_app_id: Some("42".into()),
            discord_default_image: "custom".into(),
            image_protocol: Some("kitty".into()),
        }
    }

    #[test]
    fn startup_without_config_opens_empty_login() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(MemSecrets::default(), dir.path().join("c.toml"));
        let (app, client) = startup(&dir.path().join("config.toml"), &store);
        assert_eq!(app.screen, crate::app::Screen::Login);
        assert!(client.is_none());
        assert!(app.login.server.is_empty());
    }

    #[test]
    fn startup_with_config_and_credentials_goes_to_main() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        config("https://m.example", "alice").save(&path).unwrap();
        let store = CredentialStore::new(MemSecrets::default(), dir.path().join("c.toml"));
        store.save("https://m.example", "alice", "pw").unwrap();
        let (app, client) = startup(&path, &store);
        assert_eq!(app.screen, crate::app::Screen::Main);
        assert_eq!(app.server, "https://m.example");
        assert_eq!(client.unwrap().username(), "alice");
    }

    #[test]
    fn startup_without_credentials_prefills_login() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        config("https://m.example", "alice").save(&path).unwrap();
        let store = CredentialStore::new(MemSecrets::default(), dir.path().join("c.toml"));
        let (app, client) = startup(&path, &store);
        assert!(client.is_none());
        assert_eq!(app.screen, crate::app::Screen::Login);
        assert_eq!(app.login.server, "https://m.example");
        assert_eq!(app.login.username, "alice");
    }

    #[test]
    fn startup_after_logout_keeps_server_and_asks_for_username() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        config("https://m.example", "").save(&path).unwrap();
        let store = CredentialStore::new(MemSecrets::default(), dir.path().join("c.toml"));
        let (app, _) = startup(&path, &store);
        assert_eq!(app.login.server, "https://m.example");
        assert_eq!(app.login.focus, crate::app::login::LoginField::Username);
    }

    #[test]
    fn save_config_keeps_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        config("https://old.example", "bob").save(&path).unwrap();
        save_config(&path, "https://new.example", "alice").unwrap();
        assert_eq!(
            Config::load(&path).unwrap().unwrap(),
            config("https://new.example", "alice")
        );
    }

    #[test]
    fn save_config_creates_defaults_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.toml");
        save_config(&path, "https://m.example", "alice").unwrap();
        let c = Config::load(&path).unwrap().unwrap();
        assert_eq!(c.discord_default_image, "senritsu");
        assert_eq!(c.username, "alice");
    }

    async fn run_loop_briefly() -> anyhow::Result<()> {
        let (player, _player_rx) = mpsc::unbounded_channel();
        let (_snap_tx, snapshot) = watch::channel(PlayerSnapshot::default());
        let (_events_tx, engine_events) = mpsc::unbounded_channel();
        let mut ctx = Ctx::new(SharedClient::default(), player, snapshot, engine_events);
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let input = futures::stream::pending();
        tokio::time::timeout(
            Duration::from_secs(1),
            event_loop(&mut terminal, App::new(), &mut ctx, input),
        )
        .await
        .expect("loop kept running")
    }

    /// One test covers both cases because the flag is process-wide.
    #[tokio::test]
    async fn loop_stops_with_error_after_a_panic_elsewhere() {
        note_panic();
        let before_start = run_loop_briefly().await;
        clear_panic();

        let later = tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            note_panic();
        });
        let while_running = run_loop_briefly().await;
        later.await.unwrap();
        clear_panic();

        for result in [before_start, while_running] {
            let err = result.unwrap_err();
            assert!(err.to_string().contains("panicked"), "{err:#}");
        }
    }
}

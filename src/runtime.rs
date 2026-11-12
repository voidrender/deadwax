use crate::app::login::login_error_message;
use crate::app::playlists::songs_label;
use crate::app::search::SEARCH_LIMIT;
use crate::app::{ApiResult, App, AppEvent, Effect};
use crate::config::Config;
use crate::credentials::{CredentialStore, KeyringBackend, SecretBackend};
use crate::paths;
use crate::player::{EngineEvent, PlayerCommand, PlayerSnapshot};
use crate::subsonic::{Auth, SubsonicClient, SubsonicError};
use crate::ui;
use crossterm::event::{Event, EventStream};
use futures::{Stream, StreamExt};
use ratatui::Terminal;
use ratatui::backend::Backend;
use std::collections::VecDeque;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
/// How long a search waits for further typing before it goes out.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);
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
    /// Sequence of the newest search effect, so superseded ones can be dropped.
    pub latest_search: Arc<AtomicU64>,
    /// Playlist mutations go to one worker so they reach the server in order.
    playlist_tx: mpsc::UnboundedSender<Job>,
    playlist_rx: Option<mpsc::UnboundedReceiver<Job>>,
    /// The app's session, mirrored so the worker can drop stale jobs.
    session: Arc<AtomicU64>,
}

impl Ctx {
    pub fn new(
        client: SharedClient,
        player: mpsc::UnboundedSender<PlayerCommand>,
        snapshot: watch::Receiver<PlayerSnapshot>,
        engine_events: mpsc::UnboundedReceiver<EngineEvent>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (playlist_tx, playlist_rx) = mpsc::unbounded_channel();
        Self {
            client,
            config_path: config_path(),
            credentials_path: credentials_path(),
            player,
            tx,
            rx,
            snapshot,
            engine_events,
            latest_search: Arc::new(AtomicU64::new(0)),
            playlist_tx,
            playlist_rx: Some(playlist_rx),
            session: Arc::new(AtomicU64::new(0)),
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
    if let Some(rx) = ctx.playlist_rx.take() {
        tokio::spawn(playlist_worker(
            rx,
            ctx.client.clone(),
            Arc::clone(&ctx.session),
            ctx.tx.clone(),
        ));
    }
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
    for effect in app.on_start() {
        execute(effect, &mut app, ctx);
    }
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
        let effects = app.handle_event(ev);
        ctx.session.store(app.session, Ordering::SeqCst);
        for effect in effects {
            execute(effect, &mut app, ctx);
        }
        ctx.session.store(app.session, Ordering::SeqCst);
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
        Effect::LoadArtists => spawn_api(
            ctx,
            |c| async move { c.get_artists().await },
            ApiResult::Artists,
        ),
        Effect::LoadArtist(id) => {
            let key = id.clone();
            spawn_api(
                ctx,
                |c| async move { c.get_artist(&id).await.map(|a| a.albums) },
                move |result| ApiResult::Artist { id: key, result },
            );
        }
        Effect::LoadAlbum(id) => {
            let key = id.clone();
            spawn_api(
                ctx,
                |c| async move { c.get_album(&id).await.map(|a| a.songs) },
                move |result| ApiResult::Album { id: key, result },
            );
        }
        Effect::LoadFavorites => spawn_api(
            ctx,
            |c| async move { c.get_starred_songs().await },
            ApiResult::Favorites,
        ),
        Effect::PlayRandom(n) => spawn_api(
            ctx,
            move |c| async move { c.get_random_songs(n).await },
            ApiResult::Random,
        ),
        Effect::ToggleStar { song_id, starred } => {
            let key = song_id.clone();
            spawn_api(
                ctx,
                move |c| async move {
                    if starred {
                        c.star(&song_id).await
                    } else {
                        c.unstar(&song_id).await
                    }
                },
                move |result| ApiResult::Starred {
                    song_id: key,
                    starred,
                    result,
                },
            );
        }
        Effect::Search { query, seq } => {
            ctx.latest_search.store(seq, Ordering::SeqCst);
            let latest = Arc::clone(&ctx.latest_search);
            let client = current_client(&ctx.client);
            let tx = ctx.tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(SEARCH_DEBOUNCE).await;
                if latest.load(Ordering::SeqCst) != seq {
                    return;
                }
                let result = match client {
                    Some(c) => c
                        .search(&query, SEARCH_LIMIT)
                        .await
                        .map_err(|e| e.to_string()),
                    None => Err(NOT_LOGGED_IN.to_string()),
                };
                let _ = tx.send(AppEvent::Api(ApiResult::Search { seq, result }));
            });
        }
        job @ (Effect::LoadPlaylists
        | Effect::LoadPlaylist(_)
        | Effect::CreatePlaylist { .. }
        | Effect::RenamePlaylist { .. }
        | Effect::DeletePlaylist(_)
        | Effect::AddToPlaylist { .. }
        | Effect::RemoveFromPlaylist { .. }
        | Effect::ReplacePlaylistSongs { .. }) => {
            let is_mutation = job.is_playlist_mutation();
            ctx.session.store(app.session, Ordering::SeqCst);
            let sent = ctx.playlist_tx.send(Job {
                session: app.session,
                effect: job,
            });
            if sent.is_err() {
                app.set_status("Playlist updates have stopped");
                if is_mutation {
                    // Settle the pending count so reloads are not held back.
                    let result = ApiResult::Session {
                        session: app.session,
                        inner: Box::new(ApiResult::PlaylistMutated {
                            result: Err("the playlist worker has stopped".to_string()),
                            id: None,
                        }),
                    };
                    app.handle_event(AppEvent::Api(result));
                }
            }
        }
        Effect::AppendAlbum(id) => spawn_album_queue(ctx, id, false),
        Effect::PlayNextAlbum(id) => spawn_album_queue(ctx, id, true),
    }
}

const NOT_LOGGED_IN: &str = "Not logged in";

/// Runs one client call off the UI task and reports `wrap(result)` back.
/// Errors become their (URL-free) display text.
fn spawn_api<T, F, Fut>(
    ctx: &Ctx,
    call: F,
    wrap: impl FnOnce(Result<T, String>) -> ApiResult + Send + 'static,
) where
    T: Send + 'static,
    F: FnOnce(SubsonicClient) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, SubsonicError>> + Send + 'static,
{
    let client = current_client(&ctx.client);
    let tx = ctx.tx.clone();
    tokio::spawn(async move {
        let result = match client {
            Some(c) => call(c).await.map_err(|e| e.to_string()),
            None => Err(NOT_LOGGED_IN.to_string()),
        };
        let _ = tx.send(AppEvent::Api(wrap(result)));
    });
}

/// A playlist read or mutation, stamped with the session it was issued in.
struct Job {
    session: u64,
    effect: Effect,
}

impl Job {
    /// The playlist a mutation edits, if it edits one.
    fn target(&self) -> Option<&str> {
        match &self.effect {
            Effect::RenamePlaylist { id, .. }
            | Effect::AddToPlaylist { id, .. }
            | Effect::RemoveFromPlaylist { id, .. }
            | Effect::ReplacePlaylistSongs { id, .. } => Some(id),
            _ => None,
        }
    }
}

/// Runs playlist reads and mutations one at a time, in the order they were
/// sent, so a read never overtakes a queued write. Jobs from an earlier
/// session are dropped.
async fn playlist_worker(
    mut rx: mpsc::UnboundedReceiver<Job>,
    client: SharedClient,
    session: Arc<AtomicU64>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    let mut buffered: VecDeque<Job> = VecDeque::new();
    loop {
        let job = match buffered.pop_front() {
            Some(job) => job,
            None => match rx.recv().await {
                Some(job) => job,
                None => return,
            },
        };
        if job.session != session.load(Ordering::SeqCst) {
            continue;
        }
        let send = |r: ApiResult| {
            let _ = tx.send(AppEvent::Api(ApiResult::Session {
                session: job.session,
                inner: Box::new(r),
            }));
        };
        run_playlist_job(&job, &client, &send, &mut |failed_id| {
            // Later jobs for the failed playlist were computed on state the
            // server does not have, so the ones already queued are skipped.
            while let Ok(next) = rx.try_recv() {
                buffered.push_back(next);
            }
            let (skipped, kept): (Vec<_>, Vec<_>) = buffered
                .drain(..)
                .partition(|j| j.session == job.session && j.target() == Some(failed_id));
            buffered.extend(kept);
            for _ in skipped {
                send(ApiResult::PlaylistMutated {
                    result: Ok(String::new()),
                    id: None,
                });
            }
        })
        .await;
    }
}

/// A finished mutation: its result, the playlist to reload, whether the list
/// changed, and the status text once the playlist name is known.
type JobOutcome = (
    Result<(), SubsonicError>,
    Option<String>,
    bool,
    Box<dyn FnOnce(&str) -> String + Send>,
);

/// Runs one job and reports it. After a mutation it reloads what changed.
/// The reload happens after a failure too, so the optimistic local state is
/// corrected from the server. `on_failure` runs with the failed playlist's
/// id before the reload is reported.
async fn run_playlist_job(
    job: &Job,
    client: &SharedClient,
    send: &impl Fn(ApiResult),
    on_failure: &mut impl FnMut(&str),
) {
    let Some(c) = current_client(client) else {
        match &job.effect {
            Effect::LoadPlaylists => send(ApiResult::Playlists(Err(NOT_LOGGED_IN.to_string()))),
            Effect::LoadPlaylist(id) => send(ApiResult::Playlist {
                id: id.clone(),
                result: Err(NOT_LOGGED_IN.to_string()),
            }),
            _ => send(ApiResult::PlaylistMutated {
                result: Err(NOT_LOGGED_IN.to_string()),
                id: None,
            }),
        }
        return;
    };
    let (outcome, id, reload_list, message): JobOutcome = match job.effect.clone() {
        Effect::LoadPlaylists => {
            send(ApiResult::Playlists(
                c.get_playlists().await.map_err(|e| e.to_string()),
            ));
            return;
        }
        Effect::LoadPlaylist(id) => {
            send(ApiResult::Playlist {
                result: c
                    .get_playlist(&id)
                    .await
                    .map(|p| p.songs)
                    .map_err(|e| e.to_string()),
                id,
            });
            return;
        }
        Effect::CreatePlaylist { name, song_ids } => {
            let n = song_ids.len();
            match c.create_playlist(&name, &song_ids).await {
                Ok(p) => (
                    Ok(()),
                    Some(p.id),
                    true,
                    Box::new(move |_: &str| match n {
                        0 => format!("Created {name}"),
                        n => format!("Created {name} with {}", songs_label(n)),
                    }),
                ),
                Err(e) => (Err(e), None, true, Box::new(|_: &str| String::new())),
            }
        }
        Effect::RenamePlaylist { id, name } => (
            c.rename_playlist(&id, &name).await,
            Some(id),
            true,
            Box::new(move |_: &str| format!("Renamed to {name}")),
        ),
        Effect::DeletePlaylist(id) => (
            c.delete_playlist(&id).await,
            None,
            true,
            Box::new(|_: &str| "Deleted the playlist".to_string()),
        ),
        Effect::AddToPlaylist { id, song_ids } => {
            let n = song_ids.len();
            (
                c.add_to_playlist(&id, &song_ids).await,
                Some(id),
                true,
                Box::new(move |name: &str| format!("Added {} to {name}", songs_label(n))),
            )
        }
        Effect::RemoveFromPlaylist { id, index } => (
            c.remove_from_playlist(&id, &[index]).await,
            Some(id),
            false,
            Box::new(|name: &str| format!("Removed from {name}")),
        ),
        Effect::ReplacePlaylistSongs { id, song_ids } => (
            c.replace_playlist_songs(&id, &song_ids).await,
            Some(id),
            false,
            Box::new(|_: &str| String::new()),
        ),
        _ => return,
    };
    let reloaded = match &id {
        Some(id) => Some(c.get_playlist(id).await),
        None => None,
    };
    let name = reloaded
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .map_or("the playlist", |p| p.playlist.name.as_str())
        .to_string();
    let failed = outcome.is_err();
    send(ApiResult::PlaylistMutated {
        result: outcome.map(|()| message(&name)).map_err(|e| e.to_string()),
        id: id.clone(),
    });
    if failed && let Some(id) = &id {
        on_failure(id);
    }
    if let (Some(id), Some(r)) = (id, reloaded) {
        send(ApiResult::PlaylistReloaded {
            id,
            result: r.map(|p| p.songs).map_err(|e| e.to_string()),
        });
    }
    if reload_list {
        send(ApiResult::Playlists(
            c.get_playlists().await.map_err(|e| e.to_string()),
        ));
    }
}

fn spawn_album_queue(ctx: &Ctx, id: String, next: bool) {
    spawn_api(
        ctx,
        |c| async move { c.get_album(&id).await.map(|a| a.songs) },
        move |result| ApiResult::AlbumQueued { next, result },
    );
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

/// Stops playback, drops the client, and returns to the login screen right
/// away. Removing the stored login is keychain I/O, so it runs on a blocking
/// thread and reports back as `ApiResult::LoggedOut`. The config username is
/// blanked even if the secret could not be removed, so the next start does not
/// log back in silently.
fn logout(app: &mut App, ctx: &Ctx) {
    let _ = ctx.player.send(PlayerCommand::Clear);
    let (server, username) = match current_client(&ctx.client) {
        Some(c) => (c.server_url().to_string(), c.username().to_string()),
        None => Config::load(&ctx.config_path)
            .ok()
            .flatten()
            .map(|c| (c.server_url, c.username))
            .unwrap_or_default(),
    };
    set_client(&ctx.client, None);
    app.show_login(&server, "");

    let tx = ctx.tx.clone();
    let config_path = ctx.config_path.clone();
    let credentials_path = ctx.credentials_path.clone();
    tokio::task::spawn_blocking(move || {
        let store = credential_store(&credentials_path);
        let result = logout_blocking(&store, &config_path, &server, &username);
        let _ = tx.send(AppEvent::Api(ApiResult::LoggedOut(result)));
    });
}

fn logout_blocking<S: SecretBackend>(
    store: &CredentialStore<S>,
    config_path: &Path,
    server: &str,
    username: &str,
) -> Result<(), String> {
    let mut problems = Vec::new();
    if let Err(e) = store.clear(server, username) {
        tracing::warn!("clearing credentials failed: {e:#}");
        problems.push("the saved login could not be removed");
    }
    match Config::load(config_path) {
        Ok(Some(c)) => {
            let blanked = Config {
                username: String::new(),
                ..c
            };
            if let Err(e) = blanked.save(config_path) {
                tracing::warn!("updating config failed: {e:#}");
                problems.push("the config file could not be updated");
            }
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!("reading config failed: {e:#}");
            problems.push("the config file could not be updated");
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!("Logged out, but {}.", problems.join(" and ")))
    }
}

/// Picks the first screen. A saved server, username and login that builds a
/// client skips the login form. Anything else opens it, prefilled with what
/// the config remembers.
pub fn startup<S: SecretBackend>(
    config_path: &Path,
    store: &CredentialStore<S>,
) -> (App, Option<SubsonicClient>) {
    let config = match Config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("unreadable config: {e:#}");
            let mut app = App::login_screen("", "");
            app.login.warning = Some(UNREADABLE_CONFIG.to_string());
            return (app, None);
        }
    };
    let Some(config) = config else {
        return (App::login_screen("", ""), None);
    };
    let mut warning = None;
    if !config.username.is_empty() {
        match store.load(&config.server_url, &config.username) {
            Ok(Some(auth)) => {
                match SubsonicClient::new(&config.server_url, &config.username, auth) {
                    Ok(client) => return (App::logged_in(client.server_url()), Some(client)),
                    Err(e) => {
                        tracing::warn!("saved login unusable: {e}");
                        warning = Some(UNUSABLE_LOGIN);
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("loading credentials failed: {e:#}");
                warning = Some(UNUSABLE_LOGIN);
            }
        }
    }
    let mut app = App::login_screen(&config.server_url, &config.username);
    app.login.warning = warning.map(str::to_string);
    (app, None)
}

const UNREADABLE_CONFIG: &str = "Couldn't read the config file. Please log in again.";
const UNUSABLE_LOGIN: &str = "Couldn't read the saved login. Please log in again.";

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
    fn startup_explains_an_unreadable_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "not = [valid").unwrap();
        let store = CredentialStore::new(MemSecrets::default(), dir.path().join("c.toml"));
        let (app, client) = startup(&path, &store);
        assert!(client.is_none());
        assert_eq!(app.login.warning.as_deref(), Some(UNREADABLE_CONFIG));
    }

    #[test]
    fn startup_explains_credential_and_client_failures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // A corrupt token file makes the credential load fail.
        let bad = dir.path().join("c.toml");
        std::fs::write(&bad, "garbage = [").unwrap();
        config("https://m.example", "alice").save(&path).unwrap();
        let store = CredentialStore::new(MemSecrets::default(), bad);
        let (app, _) = startup(&path, &store);
        assert_eq!(app.login.warning.as_deref(), Some(UNUSABLE_LOGIN));

        // A stored login with an invalid server URL cannot build a client.
        config("not a url", "alice").save(&path).unwrap();
        let store = CredentialStore::new(MemSecrets::default(), dir.path().join("none.toml"));
        store.save("not a url", "alice", "pw").unwrap();
        let (app, client) = startup(&path, &store);
        assert!(client.is_none());
        assert_eq!(app.login.warning.as_deref(), Some(UNUSABLE_LOGIN));
    }

    struct FailingDelete;
    impl SecretBackend for FailingDelete {
        fn get(&self, _: &str) -> Result<Option<String>, SecretError> {
            Ok(None)
        }
        fn set(&self, _: &str, _: &str) -> Result<(), SecretError> {
            Ok(())
        }
        fn delete(&self, _: &str) -> Result<(), SecretError> {
            Err(SecretError::Other("denied".into()))
        }
    }

    #[test]
    fn logout_clears_login_and_blanks_config_username() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        config("https://m.example", "alice").save(&path).unwrap();
        let store = CredentialStore::new(MemSecrets::default(), dir.path().join("c.toml"));
        store.save("https://m.example", "alice", "pw").unwrap();
        logout_blocking(&store, &path, "https://m.example", "alice").unwrap();
        assert!(store.load("https://m.example", "alice").unwrap().is_none());
        let c = Config::load(&path).unwrap().unwrap();
        assert_eq!(c.username, "");
        assert_eq!(c.server_url, "https://m.example");
    }

    #[test]
    fn logout_reports_a_failed_clear_but_still_blanks_the_username() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        config("https://m.example", "alice").save(&path).unwrap();
        let store = CredentialStore::new(FailingDelete, dir.path().join("c.toml"));
        let err = logout_blocking(&store, &path, "https://m.example", "alice").unwrap_err();
        assert!(err.contains("saved login could not be removed"), "{err}");
        assert_eq!(Config::load(&path).unwrap().unwrap().username, "");
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

    fn wire(body: serde_json::Value) -> wiremock::ResponseTemplate {
        wiremock::ResponseTemplate::new(200).set_body_json(body)
    }

    /// A server that accepts every playlist call. With `fail_remove`,
    /// removals fail.
    async fn playlist_server(fail_remove: bool) -> wiremock::MockServer {
        use serde_json::json;
        use wiremock::matchers::{path, query_param};
        use wiremock::{Mock, MockServer};

        let server = MockServer::start().await;
        let ok = json!({"subsonic-response":{"status":"ok","version":"1.16.1"}});
        let failed = json!({"subsonic-response":{"status":"failed","version":"1.16.1",
            "error":{"code":0,"message":"nope"}}});
        let playlist = json!({"subsonic-response":{"status":"ok","version":"1.16.1",
            "playlist":{"id":"pl1","name":"Mix","songCount":0,"duration":0}}});
        let lists = json!({"subsonic-response":{"status":"ok","version":"1.16.1",
            "playlists":{"playlist":[]}}});
        if fail_remove {
            Mock::given(path("/rest/updatePlaylist"))
                .and(query_param("songIndexToRemove", "1"))
                .respond_with(wire(failed))
                .mount(&server)
                .await;
        }
        for (p, body) in [
            ("updatePlaylist", ok.clone()),
            ("createPlaylist", ok),
            ("getPlaylist", playlist),
            ("getPlaylists", lists),
        ] {
            Mock::given(path(format!("/rest/{p}")))
                .respond_with(wire(body))
                .mount(&server)
                .await;
        }
        server
    }

    fn shared_for(server: &wiremock::MockServer) -> SharedClient {
        let shared = SharedClient::default();
        set_client(
            &shared,
            Some(SubsonicClient::new(&server.uri(), "alice", Auth::Password("p".into())).unwrap()),
        );
        shared
    }

    /// Runs the jobs through the worker and returns every event it sent.
    async fn run_jobs(shared: SharedClient, current: u64, jobs: Vec<Job>) -> Vec<ApiResult> {
        let (job_tx, job_rx) = mpsc::unbounded_channel();
        let (tx, mut rx) = mpsc::unbounded_channel();
        for job in jobs {
            job_tx.send(job).unwrap();
        }
        drop(job_tx);
        playlist_worker(job_rx, shared, Arc::new(AtomicU64::new(current)), tx).await;
        let mut out = Vec::new();
        while let Ok(AppEvent::Api(ApiResult::Session { inner, .. })) = rx.try_recv() {
            out.push(*inner);
        }
        out
    }

    fn job(session: u64, effect: Effect) -> Job {
        Job { session, effect }
    }

    fn remove(id: &str, index: usize) -> Effect {
        Effect::RemoveFromPlaylist {
            id: id.into(),
            index,
        }
    }

    fn mutated_results(events: &[ApiResult]) -> Vec<Result<String, String>> {
        events
            .iter()
            .filter_map(|e| match e {
                ApiResult::PlaylistMutated { result, .. } => Some(result.clone()),
                _ => None,
            })
            .collect()
    }

    async fn paths(server: &wiremock::MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| {
                format!(
                    "{}?{}",
                    r.url.path().trim_start_matches("/rest/"),
                    r.url.query().unwrap_or_default()
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn playlist_worker_runs_jobs_in_order_and_reports() {
        let server = playlist_server(false).await;
        let events = run_jobs(
            shared_for(&server),
            1,
            vec![
                job(1, remove("pl1", 0)),
                job(
                    1,
                    Effect::ReplacePlaylistSongs {
                        id: "pl1".into(),
                        song_ids: vec!["a".into(), "b".into()],
                    },
                ),
                job(
                    1,
                    Effect::AddToPlaylist {
                        id: "pl1".into(),
                        song_ids: vec!["a".into(), "b".into(), "c".into()],
                    },
                ),
            ],
        )
        .await;
        assert_eq!(
            mutated_results(&events),
            [
                Ok("Removed from Mix".to_string()),
                Ok(String::new()),
                Ok("Added 3 songs to Mix".to_string()),
            ]
        );
        let writes: Vec<String> = paths(&server)
            .await
            .into_iter()
            .filter(|p| !p.starts_with("getPlaylist"))
            .collect();
        assert_eq!(writes.len(), 3);
        assert!(writes[0].contains("songIndexToRemove=0"), "{writes:?}");
        assert!(writes[1].contains("songId=a&songId=b"), "{writes:?}");
        assert!(writes[2].contains("songIdToAdd=c"), "{writes:?}");
    }

    #[tokio::test]
    async fn playlist_reads_run_after_queued_writes() {
        let server = playlist_server(false).await;
        let events = run_jobs(
            shared_for(&server),
            1,
            vec![
                job(1, remove("pl1", 0)),
                job(1, Effect::LoadPlaylist("pl1".into())),
            ],
        )
        .await;
        let calls = paths(&server).await;
        let write = calls.iter().position(|p| p.starts_with("updatePlaylist"));
        let loads: Vec<_> = calls
            .iter()
            .enumerate()
            .filter(|(_, p)| p.starts_with("getPlaylist?"))
            .map(|(i, _)| i)
            .collect();
        assert!(write.unwrap() < *loads.last().unwrap(), "{calls:?}");
        assert!(matches!(
            events.last(),
            Some(ApiResult::Playlist { result: Ok(_), .. })
        ));
    }

    #[tokio::test]
    async fn a_failed_job_skips_queued_jobs_for_that_playlist() {
        let server = playlist_server(true).await;
        let events = run_jobs(
            shared_for(&server),
            1,
            vec![
                job(1, remove("pl1", 1)),
                job(
                    1,
                    Effect::ReplacePlaylistSongs {
                        id: "pl1".into(),
                        song_ids: vec!["x".into()],
                    },
                ),
                job(1, remove("pl2", 0)),
            ],
        )
        .await;
        // Every job reports once, so the app's pending count settles.
        let results = mutated_results(&events);
        assert_eq!(results.len(), 3, "{results:?}");
        assert!(results[0].is_err());
        assert_eq!(results[1], Ok(String::new()));
        let calls = paths(&server).await;
        assert!(
            !calls.iter().any(|p| p.contains("songId=x")),
            "skipped job reached the server: {calls:?}"
        );
        assert!(
            calls.iter().any(|p| p.contains("playlistId=pl2")),
            "{calls:?}"
        );
        // The skip reports land before the reload of the failed playlist.
        let reload = events
            .iter()
            .position(|e| matches!(e, ApiResult::PlaylistReloaded { .. }))
            .unwrap();
        let last_skip = events
            .iter()
            .position(
                |e| matches!(e, ApiResult::PlaylistMutated { result: Ok(m), .. } if m.is_empty()),
            )
            .unwrap();
        assert!(last_skip < reload);
    }

    #[tokio::test]
    async fn jobs_from_an_earlier_session_are_dropped() {
        let server = playlist_server(false).await;
        let events = run_jobs(
            shared_for(&server),
            2,
            vec![job(1, Effect::DeletePlaylist("pl1".into()))],
        )
        .await;
        assert!(events.is_empty());
        assert!(paths(&server).await.is_empty());
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

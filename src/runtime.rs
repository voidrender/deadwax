use crate::app::login::login_error_message;
use crate::app::playlists::songs_label;
use crate::app::search::SEARCH_LIMIT;
use crate::app::settings::Settings;
use crate::app::sort::Sorts;
use crate::app::{ApiResult, App, AppEvent, Effect};
use crate::art::{ArtQuery, CoverCache, Endpoints, PublicArtResolver};
use crate::config::{Config, DEFAULT_DISCORD_APP_ID};
use crate::credentials::{CredentialStore, KeyringBackend, SecretBackend};
use crate::integrations::discord::{self, PresenceMsg};
use crate::integrations::hub::{Hub, HubPorts, MbidLookup};
use crate::integrations::notify::{self, Notifier, TrackNotification};
use crate::paths;
use crate::player::persist::PersistedQueue;
use crate::player::{EngineEvent, PlayerCommand, PlayerSnapshot};
use crate::scrobble::{Outbox, PendingScrobble, ScrobbleAction};
use crate::subsonic::{Auth, SubsonicClient, SubsonicError};
use crate::ui;
use crate::ui::art::ResizeJob;
use crossterm::event::{Event, EventStream};
use futures::future::BoxFuture;
use futures::{Stream, StreamExt};
use image::DynamicImage;
use ratatui::Terminal;
use ratatui::backend::Backend;
use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
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
/// Pixel size requested for cover art.
const COVER_SIZE: u32 = 600;
/// How often the queue is saved while it has songs.
const SAVE_INTERVAL: Duration = Duration::from_secs(30);
/// How often scrobbles waiting in the outbox are retried.
const OUTBOX_INTERVAL: Duration = Duration::from_secs(60);

/// The logged-in client, shared with the engine's `url_for`. `None` while
/// logged out.
pub type SharedClient = Arc<RwLock<Option<SubsonicClient>>>;

/// Reads the shared client. A poisoned lock still holds a usable value.
pub fn current_client(shared: &SharedClient) -> Option<SubsonicClient> {
    shared
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .cloned()
}

fn set_client(shared: &SharedClient, client: Option<SubsonicClient>) {
    *shared
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = client;
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
    /// Engine events are copied here for the integration hub, stamped with
    /// the unix time they arrived. `None` until `run` starts it.
    hub: Option<mpsc::UnboundedSender<HubMsg>>,
    /// Switches the settings menu flips while the app runs.
    live: Arc<LiveSettings>,
    /// The Discord presence worker. `None` until `run` starts it.
    presence: Option<mpsc::UnboundedSender<PresenceMsg>>,
    /// Where the queue is saved. `None` turns saving and restoring off.
    state_path: Option<PathBuf>,
    /// Where the sort orders are saved. `None` turns saving off.
    sorts_path: Option<PathBuf>,
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
            hub: None,
            live: Arc::default(),
            presence: None,
            state_path: None,
            sorts_path: None,
        }
    }
}

/// Settings that services read on every use, so the settings menu can
/// change them while the app runs.
#[derive(Debug)]
struct LiveSettings {
    discord: AtomicBool,
    notifications: AtomicBool,
}

impl Default for LiveSettings {
    fn default() -> Self {
        Self {
            discord: AtomicBool::new(true),
            notifications: AtomicBool::new(true),
        }
    }
}

/// Messages for the integration hub.
enum HubMsg {
    /// An engine event and the unix time it arrived.
    Engine(EngineEvent, i64),
    /// Presence was turned back on, so show the current track again.
    RefreshPresence,
}

/// The settings in the config file, or the defaults when there is none.
fn load_settings(config_path: &Path) -> Settings {
    match Config::load(config_path) {
        Ok(Some(c)) => Settings::from_config(&c),
        Ok(None) => Settings::default(),
        Err(e) => {
            tracing::warn!("reading settings failed: {e:#}");
            Settings::default()
        }
    }
}

/// Gives the app the saved settings and points the live switches at them.
fn sync_settings(app: &mut App, ctx: &Ctx) {
    app.settings = load_settings(&ctx.config_path);
    ctx.live
        .discord
        .store(app.settings.discord, Ordering::SeqCst);
    ctx.live
        .notifications
        .store(app.settings.notifications, Ordering::SeqCst);
}

/// Applies settings from the menu, then writes them to the config.
fn save_settings(settings: &Settings, app: &mut App, ctx: &Ctx) {
    let was_on = ctx.live.discord.swap(settings.discord, Ordering::SeqCst);
    ctx.live
        .notifications
        .store(settings.notifications, Ordering::SeqCst);
    if was_on && !settings.discord {
        if let Some(presence) = &ctx.presence {
            let _ = presence.send(PresenceMsg::Disconnect);
        }
    } else if !was_on
        && settings.discord
        && let Some(hub) = &ctx.hub
    {
        let _ = hub.send(HubMsg::RefreshPresence);
    }
    let saved = Config::load(&ctx.config_path).and_then(|existing| {
        let Some(mut config) = existing else {
            anyhow::bail!("there is no config file yet");
        };
        settings.apply_to(&mut config);
        config.save(&ctx.config_path)
    });
    if let Err(e) = saved {
        tracing::warn!("saving settings failed: {e:#}");
        app.set_status(format!("Couldn't save settings: {e}"));
    }
}

/// Runs the UI until the app quits, then shuts the engine down (also when
/// the loop fails).
pub async fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    mut app: App,
    mut ctx: Ctx,
) -> anyhow::Result<()>
where
    B::Error: Send + Sync + 'static,
{
    let (art_tx, art_rx) = mpsc::unbounded_channel();
    tokio::spawn(art_resizer(art_rx, ctx.tx.clone()));
    app.art.set_resizer(art_tx);
    if let Some(rx) = ctx.playlist_rx.take() {
        tokio::spawn(playlist_worker(
            rx,
            Arc::clone(&ctx.session),
            ctx.tx.clone(),
        ));
    }
    let sorts_path = paths::data_dir().join("sorts.json");
    app.set_sorts(Sorts::load(&sorts_path));
    ctx.sorts_path = Some(sorts_path);
    sync_settings(&mut app, &ctx);
    app.image_protocol_at_start = app.settings.image_protocol;
    let services = start_services(&mut ctx);
    let result = event_loop(
        terminal,
        app,
        &mut ctx,
        EventStream::new(),
        termination_signal(),
    )
    .await;
    shutdown_engine(&mut ctx).await;
    services.stop().await;
    result
}

/// Resolves when the process is asked to stop with SIGTERM, SIGHUP or
/// SIGINT. Handling these lets `run` stop mpv and save the queue, which the
/// default action (exit on the spot) would skip, leaving mpv playing.
async fn termination_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let kinds = [
        SignalKind::terminate(),
        SignalKind::hangup(),
        SignalKind::interrupt(),
    ];
    let mut streams = Vec::new();
    for kind in kinds {
        match signal(kind) {
            Ok(s) => streams.push(s),
            Err(e) => tracing::warn!("can't listen for {kind:?}: {e}"),
        }
    }
    if streams.is_empty() {
        return std::future::pending().await;
    }
    let waits = streams.iter_mut().map(|s| Box::pin(s.recv()));
    futures::future::select_all(waits).await;
    tracing::info!("stop signal received, shutting down");
}

/// The loop behind `run`. Terminal input and the stop signal are parameters
/// so tests can drive it without a tty.
async fn event_loop<B, I, Q>(
    terminal: &mut Terminal<B>,
    mut app: App,
    ctx: &mut Ctx,
    mut input: I,
    quit: Q,
) -> anyhow::Result<()>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
    I: Stream<Item = std::io::Result<Event>> + Unpin,
    Q: Future<Output = ()>,
{
    let mut quit = std::pin::pin!(quit);
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut snapshot_open = true;
    let mut engine_open = true;

    check_panicked()?;
    app.player = ctx.snapshot.borrow_and_update().clone();
    for effect in app.on_start() {
        execute(effect, &mut app, ctx);
    }
    if current_client(&ctx.client).is_some() {
        spawn_restore(ctx);
    }
    draw(terminal, &mut app)?;

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
            () = &mut quit => break,
            _ = tick.tick() => AppEvent::Tick,
            Some(ev) = ctx.rx.recv() => ev,
            changed = ctx.snapshot.changed(), if snapshot_open => if let Ok(()) = changed { AppEvent::Snapshot(ctx.snapshot.borrow_and_update().clone()) } else {
                snapshot_open = false;
                continue;
            },
            ev = ctx.engine_events.recv(), if engine_open => if let Some(ev) = ev {
                if let Some(hub) = &ctx.hub {
                    let _ = hub.send(HubMsg::Engine(ev.clone(), unix_ms()));
                }
                AppEvent::Engine(ev)
            } else {
                engine_open = false;
                continue;
            },
        };
        if let AppEvent::Api(ApiResult::LoggedIn { client, .. }) = &ev {
            set_client(&ctx.client, Some(client.clone()));
            spawn_restore(ctx);
            sync_settings(&mut app, ctx);
        }
        let effects = app.handle_event(ev);
        ctx.session.store(app.session, Ordering::SeqCst);
        for effect in effects {
            execute(effect, &mut app, ctx);
        }
        ctx.session.store(app.session, Ordering::SeqCst);
        check_panicked()?;
        draw(terminal, &mut app)?;
    }
    Ok(())
}

/// Draws a frame. When a sixel or iTerm2 image moved or went away, its
/// pixels are still on screen, so the terminal is cleared and drawn again.
fn draw<B>(terminal: &mut Terminal<B>, app: &mut App) -> anyhow::Result<()>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
{
    terminal.draw(|f| ui::draw(f, app))?;
    if app.art.take_needs_clear() {
        terminal.clear()?;
        terminal.draw(|f| ui::draw(f, app))?;
    }
    Ok(())
}

/// Runs cover resizes on the blocking pool and hands the results back.
async fn art_resizer(
    mut rx: mpsc::UnboundedReceiver<ResizeJob>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    while let Some(job) = rx.recv().await {
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(AppEvent::ArtResized(job.run()));
        });
    }
}

/// Gets a cover through the disk cache and decodes it on a blocking thread.
/// A file that does not decode is removed so the next try fetches it again.
async fn load_cover(cache: CoverCache, cover_id: &str) -> anyhow::Result<DynamicImage> {
    let path = cache.get(cover_id, COVER_SIZE).await?;
    tokio::task::spawn_blocking(move || {
        let decoded = std::fs::read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| Ok(image::load_from_memory(&bytes)?));
        if decoded.is_err() {
            let _ = std::fs::remove_file(&path);
        }
        decoded
    })
    .await?
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
            app.session,
            |c| async move { c.get_artists().await },
            ApiResult::Artists,
        ),
        Effect::LoadArtist(id) => {
            let key = id.clone();
            spawn_api(
                ctx,
                app.session,
                |c| async move { c.get_artist(&id).await.map(|a| a.albums) },
                move |result| ApiResult::Artist { id: key, result },
            );
        }
        Effect::LoadAlbum(id) => {
            let key = id.clone();
            spawn_api(
                ctx,
                app.session,
                |c| async move { c.get_album(&id).await.map(|a| a.songs) },
                move |result| ApiResult::Album { id: key, result },
            );
        }
        Effect::LoadFavorites => spawn_api(
            ctx,
            app.session,
            |c| async move { c.get_starred_songs().await },
            ApiResult::Favorites,
        ),
        Effect::PlayRandom { count, seq } => spawn_api(
            ctx,
            app.session,
            move |c| async move { c.get_random_songs(count).await },
            move |result| ApiResult::Random { seq, result },
        ),
        Effect::ToggleStar { song_id, starred } => {
            let key = song_id.clone();
            spawn_api(
                ctx,
                app.session,
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
            let session = app.session;
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
                let _ = tx.send(AppEvent::Api(ApiResult::Session {
                    session,
                    inner: Box::new(ApiResult::Search { seq, result }),
                }));
            });
        }
        job @ (Effect::LoadPlaylists
        | Effect::LoadPlaylist(_)
        | Effect::ResyncPlaylist(_)
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
                client: current_client(&ctx.client),
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
        // A few bytes, written in order so the last press wins.
        Effect::SaveSettings(settings) => save_settings(&settings, app, ctx),
        Effect::SaveSorts(sorts) => {
            if let Some(path) = &ctx.sorts_path
                && let Err(e) = sorts.save(path)
            {
                tracing::warn!("saving sort orders failed: {e:#}");
            }
        }
        Effect::AppendAlbum(id) => spawn_album_queue(ctx, app.session, id, false),
        Effect::PlayNextAlbum(id) => spawn_album_queue(ctx, app.session, id, true),
        Effect::LoadCover(id) => {
            let Some(client) = current_client(&ctx.client) else {
                let _ = ctx.tx.send(AppEvent::CoverFailed { cover_id: id });
                return;
            };
            let cache = CoverCache::new(paths::cache_dir().join("art"), client);
            let tx = ctx.tx.clone();
            tokio::spawn(async move {
                match load_cover(cache, &id).await {
                    Ok(image) => {
                        let _ = tx.send(AppEvent::CoverReady {
                            cover_id: id,
                            image,
                        });
                    }
                    Err(e) => {
                        tracing::warn!("cover {id} unavailable: {e:#}");
                        let _ = tx.send(AppEvent::CoverFailed { cover_id: id });
                    }
                }
            });
        }
    }
}

const NOT_LOGGED_IN: &str = "Not logged in";

/// Runs one client call off the UI task and reports `wrap(result)` back,
/// tagged with `session` so a result from an earlier login is dropped.
/// Errors become their (URL-free) display text.
fn spawn_api<T, F, Fut>(
    ctx: &Ctx,
    session: u64,
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
        let _ = tx.send(AppEvent::Api(ApiResult::Session {
            session,
            inner: Box::new(wrap(result)),
        }));
    });
}

/// A playlist read or mutation, stamped with the session it was issued in
/// and the client of that session.
struct Job {
    session: u64,
    client: Option<SubsonicClient>,
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
///
/// When a mutation of playlist X fails, X is fenced: every later mutation of
/// X is skipped, because the app computed it against a copy the server does
/// not have, until the app's `ResyncPlaylist(X)` job comes through the queue.
async fn playlist_worker(
    mut rx: mpsc::UnboundedReceiver<Job>,
    session: Arc<AtomicU64>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    let mut fenced: HashSet<(u64, String)> = HashSet::new();
    while let Some(job) = rx.recv().await {
        if job.session != session.load(Ordering::SeqCst) {
            continue;
        }
        let send = |r: ApiResult| {
            let _ = tx.send(AppEvent::Api(ApiResult::Session {
                session: job.session,
                inner: Box::new(r),
            }));
        };
        if let Some(target) = job.target()
            && fenced.contains(&(job.session, target.to_string()))
        {
            let (note, list_changed) = match &job.effect {
                Effect::AddToPlaylist { .. } => (
                    "Skipped adding songs to a playlist that is refreshing",
                    true,
                ),
                Effect::RenamePlaylist { .. } => {
                    ("Skipped renaming a playlist that is refreshing", true)
                }
                _ => ("", false),
            };
            send(ApiResult::PlaylistMutated {
                result: Ok(note.to_string()),
                id: None,
            });
            if list_changed && let Some(c) = &job.client {
                send(ApiResult::Playlists(
                    c.get_playlists().await.map_err(|e| e.to_string()),
                ));
            }
            continue;
        }
        if let Effect::ResyncPlaylist(id) = &job.effect {
            fenced.remove(&(job.session, id.clone()));
        }
        run_playlist_job(&job, &send, &mut |failed_id| {
            fenced.insert((job.session, failed_id.to_string()));
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
async fn run_playlist_job(job: &Job, send: &impl Fn(ApiResult), on_failure: &mut impl FnMut(&str)) {
    let Some(c) = job.client.clone() else {
        match &job.effect {
            Effect::LoadPlaylists => send(ApiResult::Playlists(Err(NOT_LOGGED_IN.to_string()))),
            Effect::LoadPlaylist(id) | Effect::ResyncPlaylist(id) => send(ApiResult::Playlist {
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
        Effect::LoadPlaylist(id) | Effect::ResyncPlaylist(id) => {
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

fn spawn_album_queue(ctx: &Ctx, session: u64, id: String, next: bool) {
    spawn_api(
        ctx,
        session,
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
            discord: true,
            notifications: true,
            discord_app_id: None,
            discord_default_image: "deadwax".to_string(),
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
    let (server, username) = match current_client(&ctx.client) {
        Some(c) => (c.server_url().to_string(), c.username().to_string()),
        None => Config::load(&ctx.config_path)
            .ok()
            .flatten()
            .map(|c| (c.server_url, c.username))
            .unwrap_or_default(),
    };
    // The client goes before the queue is cleared. A restore that checked
    // the login earlier sent its queue first, so Clear removes it, and one
    // that checks now finds no login.
    set_client(&ctx.client, None);
    let _ = ctx.player.send(PlayerCommand::Clear);
    app.show_login_after_logout(&server);

    let tx = ctx.tx.clone();
    let config_path = ctx.config_path.clone();
    let credentials_path = ctx.credentials_path.clone();
    let cleanup = tokio::task::spawn_blocking(move || {
        let store = credential_store(&credentials_path);
        logout_blocking(&store, &config_path, &server, &username)
    });
    // The form stays locked until this arrives, so a panic must report too.
    tokio::spawn(async move {
        let result = cleanup.await.unwrap_or_else(|e| {
            tracing::warn!("logout cleanup failed: {e}");
            Err(LOGOUT_INCOMPLETE.to_string())
        });
        let _ = tx.send(AppEvent::Api(ApiResult::LoggedOut(result)));
    });
}

const LOGOUT_INCOMPLETE: &str = "Logged out, but the saved login may not have been removed.";

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

const OUTBOX_UNAVAILABLE: &str =
    "Couldn't open the scrobble outbox. Scrobbles made offline will be lost.";

/// Names a login the way the keychain does: `"{username}@{server_url}"`.
fn account_key(client: &SubsonicClient) -> String {
    format!("{}@{}", client.username(), client.server_url())
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Background work stopped after the UI quits: the outbox retry timer, the
/// queue saver, and Discord presence. The hub task ends on its own when
/// `Ctx` drops its sender.
struct Services {
    presence: Option<mpsc::UnboundedSender<PresenceMsg>>,
    flusher: JoinHandle<()>,
    saver_stop: oneshot::Sender<()>,
    saver: JoinHandle<()>,
}

impl Services {
    /// Runs after the engine has stopped, so the saver writes the final
    /// queue and position.
    async fn stop(self) {
        self.flusher.abort();
        let _ = self.saver_stop.send(());
        if tokio::time::timeout(SHUTDOWN_GRACE, self.saver)
            .await
            .is_err()
        {
            tracing::warn!("saving the queue did not finish in time");
        }
        if let Some(presence) = &self.presence {
            let _ = presence.send(PresenceMsg::Shutdown);
        }
    }
}

fn start_services(ctx: &mut Ctx) -> Services {
    let config = Config::load(&ctx.config_path).ok().flatten();
    // The worker only connects to Discord once it has something to show,
    // so it starts even with presence off. The settings menu can turn
    // presence on later.
    // Without a config yet (a first login), the defaults apply.
    let (app_id, image) = config.as_ref().map_or_else(
        || (DEFAULT_DISCORD_APP_ID.to_string(), "deadwax".to_string()),
        |c| {
            (
                c.presence_app_id().to_string(),
                c.discord_default_image.clone(),
            )
        },
    );
    let presence = Some(discord::spawn_presence(app_id, image));
    ctx.presence.clone_from(&presence);
    let data = paths::data_dir();
    let cache = paths::cache_dir();
    let outbox = match Outbox::open(data.join("outbox.json")) {
        Ok(o) => Some(o),
        Err(e) => {
            tracing::warn!("scrobble outbox unavailable: {e:#}");
            let _ = ctx
                .tx
                .send(AppEvent::Status(OUTBOX_UNAVAILABLE.to_string()));
            None
        }
    };
    let outbox: SharedOutbox = Arc::new(tokio::sync::Mutex::new(outbox));
    let ports = LivePorts {
        client: ctx.client.clone(),
        covers: cache.join("art"),
        public: PublicArtResolver::new(Endpoints::public(), cache.join("public_art.json")),
        notifier: notify::platform_notifier().into(),
        notify_failed: Arc::new(AtomicBool::new(false)),
        presence: presence.clone(),
        live: Arc::clone(&ctx.live),
        outbox: Arc::clone(&outbox),
        tx: ctx.tx.clone(),
    };
    let (hub_tx, hub_rx) = mpsc::unbounded_channel();
    tokio::spawn(hub_task(Hub::new(ports), hub_rx));
    ctx.hub = Some(hub_tx);

    let flusher = tokio::spawn(outbox_flusher(ctx.client.clone(), outbox));

    let state_path = data.join("state.json");
    ctx.state_path = Some(state_path.clone());
    let (saver_stop, stop_rx) = oneshot::channel();
    let saver = tokio::spawn(queue_saver(
        state_path,
        ctx.snapshot.clone(),
        ctx.client.clone(),
        stop_rx,
    ));
    Services {
        presence,
        flusher,
        saver_stop,
        saver,
    }
}

/// Feeds engine events to the hub one at a time, in order. Slow lookups
/// only hold up the hub, never the UI.
async fn hub_task<P: HubPorts + Sync + 'static>(
    mut hub: Hub<P>,
    mut rx: mpsc::UnboundedReceiver<HubMsg>,
) {
    while let Some(msg) = rx.recv().await {
        match msg {
            HubMsg::Engine(ev, at_ms) => hub.on_event(&ev, at_ms),
            HubMsg::RefreshPresence => hub.refresh_presence(unix_ms()),
        }
    }
}

/// `None` when the outbox file could not be opened; scrobbles then go
/// straight to the server with no offline queue.
type SharedOutbox = Arc<tokio::sync::Mutex<Option<Outbox>>>;

/// The hub's real connections. Server calls skip quietly while logged out.
struct LivePorts {
    client: SharedClient,
    covers: PathBuf,
    public: PublicArtResolver,
    notifier: Arc<dyn Notifier>,
    /// A notification has failed this session, so later failures stay quiet.
    notify_failed: Arc<AtomicBool>,
    /// `None` when there was no config to start the worker from.
    presence: Option<mpsc::UnboundedSender<PresenceMsg>>,
    live: Arc<LiveSettings>,
    outbox: SharedOutbox,
    tx: mpsc::UnboundedSender<AppEvent>,
}

impl HubPorts for LivePorts {
    fn scrobble(&self, a: ScrobbleAction) {
        if let Some(client) = current_client(&self.client) {
            tokio::spawn(send_scrobble(client, Arc::clone(&self.outbox), a));
        }
    }

    fn notify(&self, n: TrackNotification) {
        if !self.live.notifications.load(Ordering::SeqCst) {
            return;
        }
        let notifier = Arc::clone(&self.notifier);
        let failed = Arc::clone(&self.notify_failed);
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let Err(e) = notifier.notify(&n) else {
                return;
            };
            if failed.swap(true, Ordering::SeqCst) {
                tracing::debug!("notification failed: {e:#}");
            } else {
                tracing::warn!("notification failed: {e:#}");
                let _ = tx.send(AppEvent::Status(format!("Notifications unavailable: {e}")));
            }
        });
    }

    fn presence(&self, m: PresenceMsg) {
        // Updates stop while presence is off. Clears still go through, and
        // turning presence off already disconnected the worker.
        if matches!(m, PresenceMsg::Update(_)) && !self.live.discord.load(Ordering::SeqCst) {
            return;
        }
        if let Some(tx) = &self.presence {
            let _ = tx.send(m);
        }
    }

    /// Only Discord presence uses the MBID, so nothing is fetched while it
    /// is off.
    fn album_mbid(&self, album_id: &str) -> BoxFuture<'_, Option<MbidLookup>> {
        if !self.live.discord.load(Ordering::SeqCst) {
            return Box::pin(async { None });
        }
        let client = current_client(&self.client);
        let id = album_id.to_string();
        Box::pin(async move {
            let client = client?;
            match client.get_album(&id).await {
                Ok(a) => Some(MbidLookup {
                    account: account_key(&client),
                    mbid: a.album.music_brainz_id.filter(|m| !m.trim().is_empty()),
                }),
                Err(e) => {
                    tracing::warn!("album {id} lookup failed: {e}");
                    None
                }
            }
        })
    }

    fn cover_path(&self, cover_id: &str) -> BoxFuture<'_, Option<PathBuf>> {
        let client = current_client(&self.client);
        let dir = self.covers.clone();
        let id = cover_id.to_string();
        Box::pin(async move {
            match CoverCache::new(dir, client?).get(&id, COVER_SIZE).await {
                Ok(path) => Some(path),
                Err(e) => {
                    tracing::warn!("cover {id} unavailable for the notification: {e:#}");
                    None
                }
            }
        })
    }

    fn account(&self) -> String {
        current_client(&self.client)
            .map(|c| account_key(&c))
            .unwrap_or_default()
    }

    /// Only Discord presence shows public art. While it is off, the artist
    /// and album are not sent to the public art services.
    fn public_art(&self, q: ArtQuery) -> BoxFuture<'_, Option<String>> {
        if !self.live.discord.load(Ordering::SeqCst) {
            return Box::pin(async { None });
        }
        Box::pin(async move { self.public.resolve(&q).await })
    }
}

/// Sends one scrobble. A submission the server could not be reached for
/// waits in the outbox; any success retries what is waiting there.
async fn send_scrobble(client: SubsonicClient, outbox: SharedOutbox, action: ScrobbleAction) {
    let (song_id, time_ms, submission) = match action {
        ScrobbleAction::NowPlaying { song_id } => (song_id, unix_ms(), false),
        ScrobbleAction::Submit { song_id, time_ms } => (song_id, time_ms, true),
    };
    match client.scrobble(&song_id, time_ms, submission).await {
        Ok(()) => flush_outbox(&client, &outbox).await,
        Err(SubsonicError::Network(e)) if submission => {
            tracing::info!(%song_id, "server unreachable, keeping the scrobble: {e}");
            let mut guard = outbox.lock().await;
            if let Some(ob) = guard.as_mut() {
                let pending = PendingScrobble {
                    song_id,
                    time_ms,
                    account: account_key(&client),
                };
                if let Err(e) = ob.push(pending) {
                    tracing::warn!("saving the scrobble outbox failed: {e:#}");
                }
            } else {
                tracing::warn!(%song_id, "no outbox, scrobble dropped");
            }
        }
        Err(e) => tracing::warn!(%song_id, submission, "scrobble failed: {e}"),
    }
}

async fn flush_outbox(client: &SubsonicClient, outbox: &SharedOutbox) {
    let mut guard = outbox.lock().await;
    let Some(ob) = guard.as_mut().filter(|ob| !ob.is_empty()) else {
        return;
    };
    let result = ob
        .flush(&account_key(client), |p| {
            let c = client.clone();
            async move { c.scrobble(&p.song_id, p.time_ms, true).await }
        })
        .await;
    match result {
        Ok(0) => {}
        Ok(n) => tracing::info!("sent {n} queued scrobbles"),
        Err(e) => tracing::warn!("flushing the scrobble outbox failed: {e:#}"),
    }
}

/// Retries the outbox at startup and then every `OUTBOX_INTERVAL`.
async fn outbox_flusher(client: SharedClient, outbox: SharedOutbox) {
    let mut tick = tokio::time::interval(OUTBOX_INTERVAL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if let Some(c) = current_client(&client) {
            flush_outbox(&c, &outbox).await;
        }
    }
}

/// Saves the queue every `SAVE_INTERVAL` while it has songs, right away
/// when it is emptied, and once more when told to stop. A queue that never
/// had songs this run is never written, so quitting from the login screen
/// keeps the saved queue. Saves are tagged with the logged-in account, or
/// the last one seen when quitting after a logout.
async fn queue_saver(
    path: PathBuf,
    mut snapshot: watch::Receiver<PlayerSnapshot>,
    client: SharedClient,
    mut stop: oneshot::Receiver<()>,
) {
    let mut account = String::new();
    let start = tokio::time::Instant::now() + SAVE_INTERVAL;
    let mut tick = tokio::time::interval_at(start, SAVE_INTERVAL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    // The queue had songs since it was last saved empty.
    let mut live = false;
    let mut open = true;
    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = tick.tick() => {
                let snap = snapshot.borrow().clone();
                if !snap.items.is_empty() {
                    save_queue(&path, &snap, &client, &mut account).await;
                }
            }
            changed = snapshot.changed(), if open => match changed {
                Ok(()) => {
                    let empty = snapshot.borrow_and_update().items.is_empty();
                    if !empty {
                        live = true;
                    } else if live {
                        live = false;
                        save_queue(&path, &PlayerSnapshot::default(), &client, &mut account)
                            .await;
                    }
                }
                Err(_) => open = false,
            },
        }
    }
    let snap = snapshot.borrow().clone();
    if live || !snap.items.is_empty() {
        save_queue(&path, &snap, &client, &mut account).await;
    }
}

async fn save_queue(
    path: &Path,
    snap: &PlayerSnapshot,
    client: &SharedClient,
    account: &mut String,
) {
    if let Some(c) = current_client(client) {
        *account = account_key(&c);
    }
    let state = PersistedQueue {
        account: account.clone(),
        ..PersistedQueue::from_snapshot(snap)
    };
    let path = path.to_path_buf();
    match tokio::task::spawn_blocking(move || state.save(&path)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("saving the queue failed: {e:#}"),
        Err(e) => tracing::warn!("saving the queue failed: {e}"),
    }
}

/// Restores the saved queue, paused, unless something is already queued.
fn spawn_restore(ctx: &Ctx) {
    if let (Some(path), Some(client)) = (ctx.state_path.clone(), current_client(&ctx.client)) {
        tokio::spawn(restore_queue(
            path,
            account_key(&client),
            ctx.client.clone(),
            ctx.snapshot.clone(),
            ctx.player.clone(),
        ));
    }
}

/// A missing or unreadable state file is logged and skipped, and so is a
/// queue saved by a different login. `client` is checked again before the
/// restore, in case the login changed while the file was read.
async fn restore_queue(
    path: PathBuf,
    account: String,
    client: SharedClient,
    snapshot: watch::Receiver<PlayerSnapshot>,
    player: mpsc::UnboundedSender<PlayerCommand>,
) {
    let saved = match tokio::task::spawn_blocking(move || PersistedQueue::load(&path)).await {
        Ok(Ok(Some(saved)))
            if !saved.songs.is_empty() && !saved.account.is_empty() && saved.account == account =>
        {
            saved
        }
        Ok(Ok(_)) => return,
        Ok(Err(e)) => {
            tracing::warn!("ignoring the saved queue: {e:#}");
            return;
        }
        Err(e) => {
            tracing::warn!("loading the saved queue failed: {e}");
            return;
        }
    };
    // The read lock is held through the send, so logout can't clear the
    // queue between the login check and the restore.
    let guard = client
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let still_active = guard.as_ref().is_some_and(|c| account_key(c) == account);
    if still_active && snapshot.borrow().items.is_empty() {
        let _ = player.send(PlayerCommand::Restore(saved));
    }
    drop(guard);
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests compare exact, known values")]
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
            discord: true,
            notifications: true,
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
        assert_eq!(c.discord_default_image, "deadwax");
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
        for mut job in jobs {
            job.client = job.client.or_else(|| current_client(&shared));
            job_tx.send(job).unwrap();
        }
        drop(job_tx);
        playlist_worker(job_rx, Arc::new(AtomicU64::new(current)), tx).await;
        let mut out = Vec::new();
        while let Ok(AppEvent::Api(ApiResult::Session { inner, .. })) = rx.try_recv() {
            out.push(*inner);
        }
        out
    }

    fn job(session: u64, effect: Effect) -> Job {
        Job {
            session,
            client: None,
            effect,
        }
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
    }

    #[tokio::test]
    async fn edits_stay_fenced_until_the_resync_job() {
        let server = playlist_server(true).await;
        let events = run_jobs(
            shared_for(&server),
            1,
            vec![
                job(1, remove("pl1", 1)),
                // Sent after the failure but before the app's fence.
                job(1, remove("pl1", 1)),
                job(
                    1,
                    Effect::AddToPlaylist {
                        id: "pl1".into(),
                        song_ids: vec!["n".into()],
                    },
                ),
                job(1, Effect::ResyncPlaylist("pl1".into())),
                job(1, remove("pl1", 0)),
            ],
        )
        .await;
        let calls = paths(&server).await;
        let count = |needle: &str| calls.iter().filter(|p| p.contains(needle)).count();
        assert_eq!(count("songIndexToRemove=1"), 1, "{calls:?}");
        assert_eq!(count("songIdToAdd"), 0, "{calls:?}");
        assert_eq!(count("songIndexToRemove=0"), 1, "{calls:?}");
        let results = mutated_results(&events);
        assert_eq!(results.len(), 4, "{results:?}");
        assert!(results[0].is_err());
        assert_eq!(results[1], Ok(String::new()));
        assert!(matches!(&results[2], Ok(m) if m.starts_with("Skipped adding")));
        assert!(results[3].is_ok());
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ApiResult::Playlist { result: Ok(_), .. }))
        );
    }

    #[tokio::test]
    async fn a_job_uses_the_client_it_was_sent_with() {
        let sent_with = playlist_server(false).await;
        let current = playlist_server(false).await;
        let mut delete = job(1, Effect::DeletePlaylist("pl1".into()));
        delete.client = current_client(&shared_for(&sent_with));
        run_jobs(shared_for(&current), 1, vec![delete]).await;
        assert!(!paths(&sent_with).await.is_empty());
        assert!(paths(&current).await.is_empty());
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
        run_loop_until(std::future::pending()).await
    }

    async fn run_loop_until(quit: impl Future<Output = ()>) -> anyhow::Result<()> {
        let (player, _player_rx) = mpsc::unbounded_channel();
        let (_snap_tx, snapshot) = watch::channel(PlayerSnapshot::default());
        let (_events_tx, engine_events) = mpsc::unbounded_channel();
        let mut ctx = Ctx::new(SharedClient::default(), player, snapshot, engine_events);
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let input = futures::stream::pending();
        tokio::time::timeout(
            Duration::from_secs(1),
            event_loop(&mut terminal, App::new(), &mut ctx, input, quit),
        )
        .await
        .expect("loop kept running")
    }

    #[test]
    fn saving_settings_applies_them_and_keeps_the_rest_of_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let (player, _player_rx) = mpsc::unbounded_channel();
        let (_snap_tx, snapshot) = watch::channel(PlayerSnapshot::default());
        let (_events_tx, engine_events) = mpsc::unbounded_channel();
        let mut ctx = Ctx::new(SharedClient::default(), player, snapshot, engine_events);
        ctx.config_path = dir.path().join("config.toml");
        config("https://m.example", "alice")
            .save(&ctx.config_path)
            .unwrap();
        let (presence_tx, mut presence_rx) = mpsc::unbounded_channel();
        ctx.presence = Some(presence_tx);
        let (hub_tx, mut hub_rx) = mpsc::unbounded_channel();
        ctx.hub = Some(hub_tx);
        let mut app = App::logged_in("s");

        let off = Settings {
            discord: false,
            notifications: false,
            image_protocol: crate::ui::art::ProtocolChoice::Halfblocks,
        };
        save_settings(&off, &mut app, &ctx);
        assert!(matches!(
            presence_rx.try_recv(),
            Ok(PresenceMsg::Disconnect)
        ));
        assert!(!ctx.live.notifications.load(Ordering::SeqCst));
        let saved = Config::load(&ctx.config_path).unwrap().unwrap();
        assert!(!saved.discord && !saved.notifications);
        assert_eq!(saved.image_protocol.as_deref(), Some("halfblocks"));
        assert_eq!(saved.username, "alice");
        assert_eq!(saved.discord_default_image, "custom");

        save_settings(&Settings::default(), &mut app, &ctx);
        assert!(matches!(hub_rx.try_recv(), Ok(HubMsg::RefreshPresence)));
        assert!(presence_rx.try_recv().is_err());
        assert_eq!(
            Config::load(&ctx.config_path)
                .unwrap()
                .unwrap()
                .image_protocol,
            None
        );

        // With no config to write to, the app says so.
        std::fs::remove_file(&ctx.config_path).unwrap();
        save_settings(&off, &mut app, &ctx);
        assert!(
            app.status_text()
                .unwrap()
                .starts_with("Couldn't save settings")
        );
    }

    fn snap_with(ids: &[&str]) -> PlayerSnapshot {
        let items = ids
            .iter()
            .enumerate()
            .map(|(i, id)| crate::player::queue::QueueItem {
                uid: i as u64,
                song: serde_json::from_value(serde_json::json!({"id": id, "title": id})).unwrap(),
            })
            .collect();
        PlayerSnapshot {
            items,
            current: Some(0),
            status: crate::player::Status::Paused,
            position: 7.0,
            volume: 80,
            ..PlayerSnapshot::default()
        }
    }

    const ALICE: &str = "alice@http://m.example";

    fn alice() -> SharedClient {
        let shared = SharedClient::default();
        set_client(
            &shared,
            Some(
                SubsonicClient::new("http://m.example", "alice", Auth::Password("p".into()))
                    .unwrap(),
            ),
        );
        shared
    }

    fn saved_ids(path: &Path) -> Option<Vec<String>> {
        PersistedQueue::load(path)
            .unwrap()
            .map(|p| p.songs.into_iter().map(|s| s.id).collect())
    }

    #[tokio::test]
    async fn queue_saver_writes_on_stop_only_after_songs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        // Nothing was ever queued: an earlier saved queue is left alone.
        let (_tx, rx) = watch::channel(PlayerSnapshot::default());
        let (stop, stop_rx) = oneshot::channel();
        let saver = tokio::spawn(queue_saver(path.clone(), rx, alice(), stop_rx));
        stop.send(()).unwrap();
        saver.await.unwrap();
        assert!(!path.exists());

        let (tx, rx) = watch::channel(PlayerSnapshot::default());
        let (stop, stop_rx) = oneshot::channel();
        let saver = tokio::spawn(queue_saver(path.clone(), rx, alice(), stop_rx));
        tx.send(snap_with(&["a", "b"])).unwrap();
        tokio::task::yield_now().await;
        // The engine is gone by the time the final save runs.
        drop(tx);
        stop.send(()).unwrap();
        saver.await.unwrap();
        let saved = PersistedQueue::load(&path).unwrap().unwrap();
        assert_eq!(saved.position, 7.0);
        assert_eq!(saved.volume, 80);
        assert_eq!(saved.account, ALICE);
        assert_eq!(saved_ids(&path).unwrap(), ["a", "b"]);
    }

    #[tokio::test]
    async fn queue_saver_records_a_cleared_queue_right_away() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        PersistedQueue::from_snapshot(&snap_with(&["old"]))
            .save(&path)
            .unwrap();
        let (tx, rx) = watch::channel(PlayerSnapshot::default());
        let (_stop, stop_rx) = oneshot::channel();
        tokio::spawn(queue_saver(path.clone(), rx, alice(), stop_rx));
        tx.send(snap_with(&["a"])).unwrap();
        tokio::task::yield_now().await;
        tx.send(PlayerSnapshot::default()).unwrap();
        for _ in 0..200 {
            if saved_ids(&path).is_some_and(|ids| ids.is_empty()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("cleared queue was not saved: {:?}", saved_ids(&path));
    }

    #[tokio::test]
    async fn restore_only_fills_an_empty_queue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let (player, mut player_rx) = mpsc::unbounded_channel();

        let shared = alice();
        let restore = |snap: &watch::Receiver<PlayerSnapshot>, account: &str| {
            restore_queue(
                path.clone(),
                account.to_string(),
                shared.clone(),
                snap.clone(),
                player.clone(),
            )
        };

        // Missing and corrupt files are skipped.
        let (_tx, empty) = watch::channel(PlayerSnapshot::default());
        restore(&empty, ALICE).await;
        std::fs::write(&path, "{nope").unwrap();
        restore(&empty, ALICE).await;
        assert!(player_rx.try_recv().is_err());

        let saved = PersistedQueue {
            account: ALICE.into(),
            ..PersistedQueue::from_snapshot(&snap_with(&["a", "b"]))
        };
        saved.save(&path).unwrap();
        let (_tx2, busy) = watch::channel(snap_with(&["x"]));
        restore(&busy, ALICE).await;
        assert!(player_rx.try_recv().is_err());

        // Another login, or a file with no account, does not get the queue.
        restore(&empty, "bob@http://m.example").await;
        PersistedQueue {
            account: String::new(),
            ..saved.clone()
        }
        .save(&path)
        .unwrap();
        restore(&empty, "").await;
        assert!(player_rx.try_recv().is_err());

        saved.save(&path).unwrap();
        restore(&empty, ALICE).await;
        assert_eq!(player_rx.try_recv().unwrap(), PlayerCommand::Restore(saved));

        // A logout or another login before the restore runs keeps it out.
        set_client(&shared, None);
        restore(&empty, ALICE).await;
        assert!(player_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn unsent_submissions_wait_in_the_outbox() {
        use wiremock::matchers::path;
        use wiremock::{Mock, MockServer};

        let dir = tempfile::tempdir().unwrap();
        let outbox_path = dir.path().join("outbox.json");
        let outbox: SharedOutbox = Arc::new(tokio::sync::Mutex::new(Some(
            Outbox::open(outbox_path.clone()).unwrap(),
        )));
        let offline =
            SubsonicClient::new("http://127.0.0.1:1", "alice", Auth::Password("p".into())).unwrap();
        let submit = |id: &str| ScrobbleAction::Submit {
            song_id: id.into(),
            time_ms: 5,
        };
        send_scrobble(offline.clone(), Arc::clone(&outbox), submit("a")).await;
        // Now playing is not worth keeping.
        send_scrobble(
            offline,
            Arc::clone(&outbox),
            ScrobbleAction::NowPlaying {
                song_id: "b".into(),
            },
        )
        .await;
        let queued = Outbox::open(outbox_path.clone()).unwrap();
        assert_eq!(queued.len(), 1);
        drop(queued);

        let server = MockServer::start().await;
        Mock::given(path("/rest/scrobble"))
            .respond_with(wire(
                serde_json::json!({"subsonic-response":{"status":"ok","version":"1.16.1"}}),
            ))
            .mount(&server)
            .await;
        let online =
            SubsonicClient::new(&server.uri(), "alice", Auth::Password("p".into())).unwrap();
        // A scrobble this login made earlier, offline.
        outbox
            .lock()
            .await
            .as_mut()
            .unwrap()
            .push(PendingScrobble {
                song_id: "d".into(),
                time_ms: 5,
                account: account_key(&online),
            })
            .unwrap();
        send_scrobble(online, Arc::clone(&outbox), submit("c")).await;
        let sent: Vec<String> = paths(&server).await;
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert!(sent[0].contains("id=c") && sent[0].contains("submission=true"));
        assert!(sent[1].contains("id=d") && sent[1].contains("time=5"));
        // The scrobble made on the other server stays for that login.
        assert_eq!(Outbox::open(outbox_path).unwrap().len(), 1);
    }

    /// One test covers every case because the panic flag is process-wide,
    /// and a loop running in another test would see it.
    #[tokio::test]
    async fn loop_stops_after_a_panic_elsewhere_or_a_stop_signal() {
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

        // A stop signal ends the loop cleanly so `run` can shut mpv down.
        run_loop_until(async {}).await.unwrap();
    }

    #[tokio::test]
    async fn public_lookups_wait_for_discord_presence() {
        struct Silent;
        impl Notifier for Silent {
            fn notify(&self, _: &TrackNotification) -> anyhow::Result<()> {
                Ok(())
            }
        }
        let server = wiremock::MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let endpoints = Endpoints {
            caa: server.uri(),
            itunes: server.uri(),
            deezer: server.uri(),
        };
        let (tx, _rx) = mpsc::unbounded_channel();
        let ports = LivePorts {
            client: shared_for(&server),
            covers: dir.path().join("art"),
            public: PublicArtResolver::new(endpoints, dir.path().join("public_art.json")),
            notifier: Arc::new(Silent),
            notify_failed: Arc::new(AtomicBool::new(false)),
            presence: None,
            live: Arc::new(LiveSettings::default()),
            outbox: Arc::new(tokio::sync::Mutex::new(None)),
            tx,
        };
        ports.live.discord.store(false, Ordering::SeqCst);
        assert_eq!(ports.album_mbid("al1").await, None);
        let query = ArtQuery {
            artist: "Ar".into(),
            album: "Al".into(),
            release_mbid: None,
        };
        assert_eq!(ports.public_art(query).await, None);
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}
